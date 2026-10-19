//! Optional neural next-word prediction (feature `neural`).
//!
//! A background worker loads the quantized SmolLM2-135M GGUF model
//! (LLaMA architecture) via candle and answers next-word requests on a
//! standard `std::sync::mpsc` channel. The `Controller` requests the
//! next word for the sentence so far while a word is still being typed,
//! so the answer is usually ready by the space, and reads it with
//! [`NeuralEngine::answer`] only at a keystroke; ready predictions take
//! priority in the next-word pills.
//! Worker failures (load and forward passes) are reported to the host
//! through [`NeuralEngine::take_errors`], because osk-core must not
//! print.
//!
//! The model and tokenizer are embedded in the binary, so
//! [`NeuralEngine::new`] always succeeds with no download. A model file
//! on disk (see [`model_dir`]) takes precedence when present. The
//! engine is an owned value, never a process global: its owner (the
//! `Controller` every UI builds through the shared construction path)
//! decides when it is enabled and drains [`NeuralEngine::take_errors`]
//! to report worker failures even while the engine is toggled off.
//!
//! Without the `neural` feature this module is a stub:
//! [`NeuralEngine::new`] always returns `None` and the n-gram predictor
//! is used unmodified.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use parking_lot::Mutex;

use crate::predict::intern;

/// Model file name inside the model directory.
pub const MODEL_FILE: &str = "smollm2-135m-q4_k_m.gguf";
/// Tokenizer file name inside the model directory.
pub const TOKENIZER_FILE: &str = "tokenizer.json";
/// Base URL for resolving files from the model repo.
pub const MODEL_URL_BASE: &str =
    "https://huggingface.co/HuggingFaceTB/SmolLM2-135M-Instruct-GGUF/resolve/main";
#[cfg(feature = "neural")]
/// Rayon worker threads for the CPU forward pass.
const NUM_THREADS: usize = 4;
#[cfg(feature = "neural")]
/// Top-k logits decoded into candidate words.
const TOPK: usize = 256;
#[cfg(feature = "neural")]
/// Maximum candidate words returned per request.
const MAX_WORDS: usize = 64;

#[cfg(feature = "neural")]
/// The quantized model, embedded so the engine needs no download.
const EMBEDDED_MODEL_GGUF: &[u8] = include_bytes!("../data/smollm2-135m-q4_k_m.gguf");
#[cfg(feature = "neural")]
/// The tokenizer, embedded alongside the model.
const EMBEDDED_TOKENIZER: &str = include_str!("../data/tokenizer.json");

#[cfg(feature = "neural")]
type Result<T, E = String> = std::result::Result<T, E>;

#[cfg(feature = "neural")]
/// The tokenizer parts extracted from GGUF metadata: the raw token
/// list and the BPE merge rules, each optional.
type VocabParts = (Option<Vec<String>>, Option<Vec<(String, String)>>);

/// The model directory: `$XDG_DATA_HOME/osk/models`, else
/// `~/.local/share/osk/models`.
pub fn model_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let p = PathBuf::from(xdg);
        if p.is_absolute() {
            return p.join("osk").join("models");
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local/share/osk/models")
}

/// A background neural predictor.
///
/// With the `neural` feature, `new()` always succeeds: it uses the
/// model on disk when present, otherwise the embedded weights. It never
/// panics and never blocks the calling thread (model loading happens
/// on the worker thread).
pub struct NeuralEngine {
    tx: mpsc::Sender<Vec<String>>,
    rx: mpsc::Receiver<(String, Vec<String>)>,
    #[cfg(feature = "neural")]
    /// Failures the worker reported since the last [`take_errors`]
    /// drain; never present without the `neural` feature, where no
    /// worker runs.
    err_rx: mpsc::Receiver<String>,
    /// The prompts of the latest request, so a repeat is not resent.
    asked: Mutex<Vec<String>>,
    /// The answers that have landed for those prompts.
    answers: Mutex<HashMap<String, Vec<&'static str>>>,
}

#[cfg(not(feature = "neural"))]
impl NeuralEngine {
    /// Always `None` without the `neural` feature.
    pub fn new() -> Option<Self> {
        None
    }

    /// Always `None` without the `neural` feature.
    pub fn new_at(_dir: &Path) -> Option<Self> {
        None
    }
}

#[cfg(feature = "neural")]
/// Where the worker loads its model and tokenizer from.
enum ModelSource {
    /// A model file on disk, with the tokenizer beside it.
    Disk { model: PathBuf, dir: PathBuf },
    /// The weights and tokenizer embedded in the binary.
    Embedded,
}

#[cfg(feature = "neural")]
impl NeuralEngine {
    /// Create an engine for the standard model location.
    ///
    /// Uses the model on disk when present, otherwise the embedded
    /// weights, so this always succeeds (no download required). Model
    /// loading happens on the worker thread, so this returns
    /// immediately.
    pub fn new() -> Option<Self> {
        let dir = model_dir();
        let model = dir.join(MODEL_FILE);
        let source = if model.is_file() {
            ModelSource::Disk { model, dir }
        } else {
            ModelSource::Embedded
        };
        Self::spawn(source)
    }

    /// Create an engine for the model in `dir` (test hook).
    ///
    /// Returns `None` when the model file is missing.
    pub fn new_at(dir: &Path) -> Option<Self> {
        let model = dir.join(MODEL_FILE);
        if !model.is_file() {
            return None;
        }
        Self::spawn(ModelSource::Disk {
            model,
            dir: dir.to_path_buf(),
        })
    }

    /// Spawn the worker thread for `source` and wire up the channels.
    fn spawn(source: ModelSource) -> Option<Self> {
        let (tx, rx_req) = mpsc::channel();
        let (tx_res, rx) = mpsc::channel();
        let (tx_err, err_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("osk-neural".into())
            .spawn(move || worker(source, tx_res, tx_err, rx_req))
            .ok()?;
        Some(Self {
            tx,
            rx,
            err_rx,
            asked: Mutex::new(Vec::new()),
            answers: Mutex::new(HashMap::new()),
        })
    }
}

impl NeuralEngine {
    /// Ask the worker to predict the next word after each of
    /// `prompts`, answered in order. A newer request replaces this one
    /// and drops the answers to prompts it leaves out; prompts already
    /// answered are kept, not recomputed.
    ///
    /// Non-blocking: the request queue is unbounded, so this only
    /// allocates; a dead worker drops the request silently.
    pub fn request(&self, prompts: &[String]) {
        let mut asked = self.asked.lock();
        if *asked == prompts {
            return;
        }
        let mut answers = self.answers.lock();
        self.drain(&asked, &mut answers);
        prompts.clone_into(&mut asked);
        answers.retain(|p, _| prompts.contains(p));
        let todo: Vec<String> = prompts
            .iter()
            .filter(|p| !answers.contains_key(*p))
            .cloned()
            .collect();
        let _ = self.tx.send(todo);
    }

    /// The ranked next words for `prompt` once its answer has landed,
    /// while it is part of the latest request.
    pub fn answer(&self, prompt: &str) -> Option<Vec<&'static str>> {
        let asked = self.asked.lock();
        let mut answers = self.answers.lock();
        self.drain(&asked, &mut answers);
        answers.get(prompt).cloned()
    }

    /// Keep the answers that have landed for the prompts in `asked`.
    fn drain(&self, asked: &[String], answers: &mut HashMap<String, Vec<&'static str>>) {
        while let Ok((p, words)) = self.rx.try_recv() {
            if asked.contains(&p) {
                answers.insert(p, words.iter().map(|w| intern(w)).collect());
            }
        }
    }

    /// The failures the worker reported (model load and forward
    /// passes), drained so the host can print what osk-core must not.
    /// Always empty without the `neural` feature, where no worker runs
    /// and no failures can arise.
    pub fn take_errors(&self) -> Vec<String> {
        #[cfg(feature = "neural")]
        let out = self.err_rx.try_iter().collect();
        #[cfg(not(feature = "neural"))]
        let out = Vec::new();
        out
    }
}

#[cfg(feature = "neural")]
mod engine {
    use super::*;
    use std::collections::HashMap;

    use ahash::HashMap as AHashMap;
    use candle_core::quantized::gguf_file;
    use candle_core::{Device, Tensor};
    use candle_transformers::models::quantized_llama::{self, ModelWeights};
    use tokenizers::models::bpe::BPE;
    use tokenizers::Tokenizer;

    /// Load the quantized model and its tokenizer from `source`.
    fn load(source: &ModelSource) -> Result<(ModelWeights, Tokenizer)> {
        let device = Device::Cpu;
        match source {
            ModelSource::Disk { model, dir } => {
                let mut file = std::fs::File::open(model).map_err(|e| e.to_string())?;
                let content = gguf_file::Content::read(&mut file).map_err(|e| e.to_string())?;
                // Pull the embedded vocab out before `content` is moved.
                let (tokens, merges) = embedded_vocab(&content.metadata);
                let model = ModelWeights::from_gguf(content, &mut file, &device)
                    .map_err(|e| e.to_string())?;
                let tokenizer = match dir.join(TOKENIZER_FILE) {
                    p if p.is_file() => Tokenizer::from_file(&p).map_err(|e| e.to_string())?,
                    _ => bpe_from_vocab(tokens, merges)?,
                };
                Ok((model, tokenizer))
            }
            ModelSource::Embedded => {
                let mut reader = std::io::Cursor::new(EMBEDDED_MODEL_GGUF);
                let content = gguf_file::Content::read(&mut reader).map_err(|e| e.to_string())?;
                let tokenizer =
                    Tokenizer::from_bytes(EMBEDDED_TOKENIZER.as_bytes()).map_err(|e| e.to_string())?;
                let model = ModelWeights::from_gguf(content, &mut reader, &device)
                    .map_err(|e| e.to_string())?;
                Ok((model, tokenizer))
            }
        }
    }

    /// `tokenizer.ggml.tokens` / `tokenizer.ggml.merges` from the GGUF
    /// metadata, when present.
    fn embedded_vocab(metadata: &HashMap<String, gguf_file::Value>) -> VocabParts {
        let tokens = metadata
            .get("tokenizer.ggml.tokens")
            .and_then(|v| v.to_vec().ok())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.to_string().ok().cloned())
                    .collect()
            })
            .filter(|v: &Vec<String>| !v.is_empty());
        let merges = metadata
            .get("tokenizer.ggml.merges")
            .and_then(|v| v.to_vec().ok())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.to_string().ok())
                    .filter_map(|s| {
                        let mut it = s.splitn(2, ' ');
                        let (a, b) = (it.next()?, it.next()?);
                        Some((a.to_string(), b.to_string()))
                    })
                    .collect()
            });
        (tokens, merges)
    }

    /// Fallback tokenizer: byte-level BPE built from the GGUF's own
    /// vocab and merge table.
    fn bpe_from_vocab(
        tokens: Option<Vec<String>>,
        merges: Option<Vec<(String, String)>>,
    ) -> Result<Tokenizer> {
        let Some(tokens) = tokens else {
            return Err("no tokenizer.json and no vocab in the GGUF".into());
        };
        let vocab: AHashMap<String, u32> = tokens
            .into_iter()
            .enumerate()
            .map(|(i, t)| (t, i as u32))
            .collect();
        let model = BPE::builder()
            .vocab_and_merges(vocab, merges.unwrap_or_default())
            .unk_token("<unk>".to_string())
            .byte_fallback(true)
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Tokenizer::new(model))
    }

    /// One forward pass: top-k next-token ids decoded into words.
    /// The model state after a prompt's committed words (everything
    /// before its last space). The next request for the same sentence
    /// runs only the tokens of its last word on a clone, and a space
    /// extends it by one word instead of rereading the sentence.
    struct Checkpoint {
        ids: Vec<u32>,
        model: ModelWeights,
    }

    fn encode(tokenizer: &Tokenizer, text: &str) -> Result<Vec<u32>> {
        let enc = tokenizer.encode(text, true).map_err(|e| e.to_string())?;
        Ok(enc.get_ids().to_vec())
    }

    /// Run `ids` through `model`, whose KV cache holds the `pos`
    /// tokens before them, and return the next-token logits.
    fn forward(model: &mut ModelWeights, ids: &[u32], pos: usize, device: &Device) -> Result<Vec<f32>> {
        let input = Tensor::new(ids, device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| e.to_string())?;
        model
            .forward(&input, pos)
            .and_then(|l| l.flatten(0, 1))
            .and_then(|t| t.to_vec1())
            .map_err(|e| e.to_string())
    }

    /// Next-token logits for `prompt`, reusing `cp` when the prompt
    /// continues its sentence. `model` is the loaded weights with an
    /// empty KV cache; it is only ever cloned.
    fn logits(
        model: &ModelWeights,
        cp: &mut Option<Checkpoint>,
        tokenizer: &Tokenizer,
        prompt: &str,
        device: &Device,
    ) -> Result<Vec<f32>> {
        let mut ids = encode(tokenizer, prompt)?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let stable = match prompt.rsplit_once(' ') {
            Some((head, _)) => encode(tokenizer, head)?,
            None => Vec::new(),
        };
        let max = quantized_llama::MAX_SEQ_LEN;
        let reusable = !stable.is_empty()
            && stable.len() < ids.len()
            && ids.len() <= max
            && ids.starts_with(&stable);
        if !reusable {
            if ids.len() > max {
                ids = ids.split_off(ids.len() - max);
            }
            return forward(&mut model.clone(), &ids, 0, device);
        }
        let base = match cp.take() {
            Some(c) if c.ids == stable => c,
            Some(mut c) if stable.starts_with(&c.ids) => {
                forward(&mut c.model, &stable[c.ids.len()..], c.ids.len(), device)?;
                c.ids = stable;
                c
            }
            _ => {
                let mut m = model.clone();
                forward(&mut m, &stable, 0, device)?;
                Checkpoint { ids: stable, model: m }
            }
        };
        let out = forward(&mut base.model.clone(), &ids[base.ids.len()..], base.ids.len(), device);
        *cp = Some(base);
        out
    }

    fn predict(
        model: &ModelWeights,
        cp: &mut Option<Checkpoint>,
        tokenizer: &Tokenizer,
        prompt: &str,
        device: &Device,
    ) -> Result<Vec<String>> {
        let logits = logits(model, cp, tokenizer, prompt, device)?;
        let mut ranked: Vec<usize> = (0..logits.len()).collect();
        ranked.sort_by(|&a, &b| logits[b].total_cmp(&logits[a]));
        let mut words = Vec::new();
        for &id in ranked.iter().take(TOPK) {
            let Ok(s) = tokenizer.decode(&[id as u32], true) else {
                continue;
            };
            // A token without a leading space continues the last word
            // ("to" + "day"); only a new word is a next word.
            let Some(s) = s.strip_prefix(' ') else {
                continue;
            };
            let s = s.trim();
            if s.len() > 31
                || s.is_empty()
                || s.chars().any(|c| c.is_whitespace())
                || !s
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
            {
                continue;
            }
            let s = s.to_lowercase();
            if !words.iter().any(|w| w == &s) {
                words.push(s);
            }
            if words.len() >= MAX_WORDS {
                break;
            }
        }
        Ok(words)
    }

    /// Worker loop: load the model, then answer requests until the
    /// engine is dropped.
    pub(super) fn worker(
        source: ModelSource,
        tx: mpsc::Sender<(String, Vec<String>)>,
        tx_err: mpsc::Sender<String>,
        rx: mpsc::Receiver<Vec<String>>,
    ) {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(NUM_THREADS)
            .build();
        let device = Device::Cpu;
        let (model, tokenizer) = match load(&source) {
            Ok(v) => v,
            // osk-core must not print; report the load failure once and
            // end the worker, so later requests keep polling silently.
            Err(e) => {
                let _ = tx_err.send(format!("failed to load model: {e}"));
                return;
            }
        };
        let mut cp = None;
        let mut queue: std::collections::VecDeque<String> = Default::default();
        loop {
            // Typing outpaces the model: a newer request replaces what
            // is left of the current one.
            while let Ok(newer) = rx.try_recv() {
                queue = newer.into();
            }
            let Some(prompt) = queue.pop_front() else {
                match rx.recv() {
                    Ok(batch) => queue = batch.into(),
                    Err(_) => break,
                }
                continue;
            };
            let words = match &pool {
                Ok(p) => p
                    .install(|| predict(&model, &mut cp, &tokenizer, &prompt, &device)),
                Err(_) => predict(&model, &mut cp, &tokenizer, &prompt, &device),
            };
            let words = match words {
                Ok(w) => w,
                // osk-core must not print; report the failure as it
                // happens and keep serving the next request.
                Err(e) => {
                    let _ = tx_err.send(format!("forward failed: {e}"));
                    continue;
                }
            };
            if tx.send((prompt, words)).is_err() {
                break;
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Reusing the checkpoint must rank the same words as one full
        /// forward over the whole prompt, through a typing sequence
        /// that extends, revises and abandons the sentence.
        #[test]
        fn checkpoint_matches_a_full_forward() {
            let (model, tok) = load(&ModelSource::Embedded).unwrap();
            let dev = Device::Cpu;
            let mut cp = None;
            let prompts = [
                "i want", "i want t", "i want to", "i want to g", "i want to go",
                "i want to go to the", "i want to go to the st", "i want to go to the store",
                "can you", "can you send me", "i want to go to the store and",
            ];
            for p in prompts {
                let got = logits(&model, &mut cp, &tok, p, &dev).unwrap();
                let ids = encode(&tok, p).unwrap();
                let want = forward(&mut model.clone(), &ids, 0, &dev).unwrap();
                let top = |l: &[f32]| {
                    let mut r: Vec<usize> = (0..l.len()).collect();
                    r.sort_by(|&a, &b| l[b].total_cmp(&l[a]));
                    r.truncate(TOPK);
                    r
                };
                assert_eq!(top(&got), top(&want), "top tokens differ for {p:?}");
                let diff = got.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
                assert!(diff < 0.05, "logits drift {diff} for {p:?}");
            }
        }
    }
}

#[cfg(feature = "neural")]
use engine::worker;

#[cfg(all(test, feature = "neural"))]
mod tests {
    use super::*;

    /// A model file that cannot be loaded must surface its failure
    /// through `take_errors`, not degrade silently.
    #[test]
    fn load_failure_surfaces_error() {
        let dir =
            std::env::temp_dir().join(format!("osk-neural-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Garbage where the GGUF magic would be: opening succeeds, the
        // load fails inside the worker.
        std::fs::write(dir.join(MODEL_FILE), b"not a gguf model").unwrap();

        let engine = NeuralEngine::new_at(&dir).expect("a present model file spawns a worker");
        // The worker reports the failure asynchronously; wait briefly
        // for it to arrive before giving up.
        let mut errored: Vec<String> = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while errored.is_empty() && std::time::Instant::now() < deadline {
            errored = engine.take_errors();
            if errored.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        assert!(
            errored.iter().any(|e| e.starts_with("failed to load model:")),
            "the load failure must reach take_errors: {errored:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

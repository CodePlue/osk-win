//! Optional neural next-word prediction (feature `neural`).
//!
//! A background worker loads the quantized SmolLM2-135M GGUF model
//! (LLaMA architecture) via candle and answers next-word requests on a
//! standard `std::sync::mpsc` channel. The UI thread calls
//! [`NeuralEngine::request`] when a word is committed and drains
//! [`NeuralEngine::poll`] from `Predictor::suggestions()`; ready
//! predictions take priority in the next-word pills.
//!
//! The model and tokenizer are embedded in the binary, so
//! [`NeuralEngine::new`] always succeeds with no download. A model file
//! on disk (see [`model_dir`]) takes precedence when present. The engine
//! can be switched at runtime with [`toggle`]; [`is_installed`] reports
//! the current state.
//!
//! Without the `neural` feature this module is a stub:
//! [`NeuralEngine::new`] always returns `None` and the n-gram predictor
//! is used unmodified.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::LazyLock;

use parking_lot::{Mutex, MutexGuard};

#[cfg(feature = "neural")]
use crate::predict::intern;

/// Model file name inside the model directory.
pub const MODEL_FILE: &str = "smollm2-135m-q4_k_m.gguf";
/// Remote model file name in the repository.
pub const MODEL_REMOTE_FILE: &str = "SmolLM2-135M-Q4_K_M.gguf";
/// Tokenizer file name inside the model directory.
pub const TOKENIZER_FILE: &str = "tokenizer.json";
/// Base URL for resolving files from the base model repo.
pub const MODEL_URL_BASE: &str =
    "https://huggingface.co/ggml-org/SmolLM2-135M-GGUF/resolve/main";
#[cfg(feature = "neural")]
/// Rayon worker threads for the CPU forward pass (small compute thread count).
const NUM_THREADS: usize = 2;
#[cfg(feature = "neural")]
/// Top logits scanned for candidate tokens.
const LOGIT_SCAN_TOPK: usize = 64;
#[cfg(feature = "neural")]
/// Maximum context tokens kept for prefill (capped at last ~32-64 tokens).
const MAX_CONTEXT_TOKENS: usize = 48;
#[cfg(feature = "neural")]
/// Maximum candidate words returned per request.
const MAX_WORDS: usize = 5;

#[cfg(feature = "neural")]
/// The quantized model, embedded so the engine needs no download.
const EMBEDDED_MODEL_GGUF: &[u8] = include_bytes!("../data/smollm2-135m-q4_k_m.gguf");
#[cfg(feature = "neural")]
/// The tokenizer, embedded alongside the model.
const EMBEDDED_TOKENIZER: &str = include_str!("../data/tokenizer.json");

#[cfg(feature = "neural")]
type Result<T, E = String> = std::result::Result<T, E>;

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

/// The process-wide engine, installed at startup and enabled by default.
static ENGINE: LazyLock<Mutex<Option<NeuralEngine>>> = LazyLock::new(|| Mutex::new(None));
/// Runtime on/off state of the installed engine.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Install the process-wide engine (startup only). The engine starts
/// enabled.
pub fn install(engine: NeuralEngine) {
    *ENGINE.lock() = Some(engine);
    ENABLED.store(true, Ordering::Relaxed);
}

/// Whether the engine is installed and currently enabled.
pub fn is_installed() -> bool {
    ENABLED.load(Ordering::Relaxed) && ENGINE.lock().is_some()
}

/// Toggle the engine on or off and return the new state. Returns
/// `false` when no engine was installed.
pub fn toggle() -> bool {
    let enabled = ENABLED.load(Ordering::Relaxed);
    if ENGINE.lock().is_some() {
        let next = !enabled;
        ENABLED.store(next, Ordering::Relaxed);
        next
    } else {
        false
    }
}

/// The active engine, if one was installed and is enabled.
pub fn engine() -> Option<MutexGuard<'static, Option<NeuralEngine>>> {
    if !ENABLED.load(Ordering::Relaxed) {
        return None;
    }
    Some(ENGINE.lock())
}

/// Run `f` on the active engine, if one was installed and is enabled.
pub fn with_engine<R>(f: impl FnOnce(&NeuralEngine) -> R) -> Option<R> {
    let guard = ENGINE.lock();
    if !ENABLED.load(Ordering::Relaxed) {
        return None;
    }
    guard.as_ref().map(f)
}

/// A background neural predictor.
///
/// With the `neural` feature, `new()` always succeeds: it uses the
/// model on disk when present, otherwise the embedded weights. It never
/// panics and never blocks the calling thread (model loading happens
/// on the worker thread).
pub struct NeuralEngine {
    tx: mpsc::Sender<(u64, String)>,
    rx: mpsc::Receiver<(u64, Vec<&'static str>)>,
    /// Sequence number of the latest request, shared with worker for cancellation.
    gen: std::sync::Arc<AtomicU64>,
    /// (request generation, words) for the latest answered request.
    result: Mutex<Option<(u64, Vec<&'static str>)>>,
}

impl std::fmt::Debug for NeuralEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuralEngine")
            .field("gen", &self.gen.load(Ordering::Relaxed))
            .finish()
    }
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
        let gen = std::sync::Arc::new(AtomicU64::new(0));
        let active_gen = gen.clone();
        std::thread::Builder::new()
            .name("osk-neural".into())
            .spawn(move || worker(source, tx_res, rx_req, active_gen))
            .ok()?;
        Some(Self {
            tx,
            rx,
            gen,
            result: Mutex::new(None),
        })
    }
}

impl NeuralEngine {
    /// Ask the worker to predict the next word after `prompt`.
    ///
    /// Non-blocking: the request queue is unbounded, so this only
    /// allocates; a dead worker drops the request silently.
    pub fn request(&self, prompt: &str) {
        let gen = self.gen.fetch_add(1, Ordering::Relaxed) + 1;
        *self.result.lock() = None;
        let _ = self.tx.send((gen, prompt.to_string()));
    }

    /// The words for the latest answered request, if any.
    ///
    /// Keeps returning the same words until a newer request is made,
    /// so repeated `suggestions()` calls within one frame see a
    /// stable pill row.
    pub fn poll(&self) -> Option<Vec<&'static str>> {
        let cur = self.gen.load(Ordering::Relaxed);
        while let Ok((g, words)) = self.rx.try_recv() {
            if g == cur {
                *self.result.lock() = Some((g, words));
            }
        }
        self.result
            .lock()
            .as_ref()
            .filter(|(g, _)| *g == cur)
            .map(|(_, w)| w.clone())
    }

    /// Return ready candidates for the active generation without blocking.
    pub fn candidates(&self) -> Option<Vec<&'static str>> {
        self.poll()
    }
}

#[cfg(feature = "neural")]
mod trie {
    use std::sync::LazyLock;
    use ahash::AHashMap;
    use crate::words::WORDS;

    pub struct PrefixTrieNode {
        children: AHashMap<char, PrefixTrieNode>,
        word: Option<&'static str>,
        best_completion: Option<&'static str>,
    }

    impl PrefixTrieNode {
        fn new() -> Self {
            Self {
                children: AHashMap::new(),
                word: None,
                best_completion: None,
            }
        }
    }

    pub struct PrefixTrie {
        root: PrefixTrieNode,
    }

    impl PrefixTrie {
        pub fn build(words: &[&'static str]) -> Self {
            let mut root = PrefixTrieNode::new();
            for &w in words {
                let mut curr = &mut root;
                if curr.best_completion.is_none() {
                    curr.best_completion = Some(w);
                }
                for ch in w.chars() {
                    curr = curr.children.entry(ch).or_insert_with(PrefixTrieNode::new);
                    if curr.best_completion.is_none() {
                        curr.best_completion = Some(w);
                    }
                }
                curr.word = Some(w);
            }
            Self { root }
        }

        pub fn exact_word(&self, prefix: &str) -> Option<&'static str> {
            let mut curr = &self.root;
            for ch in prefix.chars() {
                curr = curr.children.get(&ch)?;
            }
            curr.word
        }

        pub fn best_completion(&self, prefix: &str) -> Option<&'static str> {
            let mut curr = &self.root;
            for ch in prefix.chars() {
                curr = curr.children.get(&ch)?;
            }
            curr.best_completion
        }
    }

    pub static COMMON_WORDS_TRIE: LazyLock<PrefixTrie> =
        LazyLock::new(|| PrefixTrie::build(WORDS));
}

#[cfg(feature = "neural")]
mod engine {
    use super::*;
    use std::collections::HashMap;

    use ahash::HashMap as AHashMap;
    use candle_core::quantized::gguf_file;
    use candle_core::{Device, Tensor};
    use candle_transformers::models::quantized_llama::ModelWeights;
    use tokenizers::models::bpe::BPE;
    use tokenizers::Tokenizer;

    /// Load the quantized model and its tokenizer from `source`.
    fn load(source: &ModelSource, device: &Device) -> Result<(ModelWeights, Tokenizer)> {
        match source {
            ModelSource::Disk { model, dir } => {
                let mut file = std::fs::File::open(model).map_err(|e| e.to_string())?;
                let content = gguf_file::Content::read(&mut file).map_err(|e| e.to_string())?;
                // Pull the embedded vocab out before `content` is moved.
                let (tokens, merges) = embedded_vocab(&content.metadata);
                let model = ModelWeights::from_gguf(content, &mut file, device)
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
                let model = ModelWeights::from_gguf(content, &mut reader, device)
                    .map_err(|e| e.to_string())?;
                Ok((model, tokenizer))
            }
        }
    }

    /// `tokenizer.ggml.tokens` / `tokenizer.ggml.merges` from the GGUF
    /// metadata, when present.
    fn embedded_vocab(
        metadata: &HashMap<String, gguf_file::Value>,
    ) -> (Option<Vec<String>>, Option<Vec<(String, String)>>) {
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

    /// Incremental inference session preserving the KV cache across consecutive tokens.
    struct IncrementalSession {
        model: ModelWeights,
        tokenizer: Tokenizer,
        device: Device,
        cached_tokens: Vec<u32>,
    }

    impl IncrementalSession {
        fn new(source: &ModelSource) -> Result<Self> {
            let device = Device::Cpu;
            let (model, tokenizer) = load(source, &device)?;
            Ok(Self {
                model,
                tokenizer,
                device,
                cached_tokens: Vec::new(),
            })
        }

        fn predict(&mut self, prompt: &str) -> Result<Vec<&'static str>> {
            let enc = self.tokenizer.encode(prompt, true).map_err(|e| e.to_string())?;
            let mut ids: Vec<u32> = enc.get_ids().to_vec();
            if ids.is_empty() {
                return Ok(Vec::new());
            }

            // Short context: cap context tokens to MAX_CONTEXT_TOKENS (~32-64 tokens).
            if ids.len() > MAX_CONTEXT_TOKENS {
                ids = ids.split_off(ids.len() - MAX_CONTEXT_TOKENS);
            }

            let logits = if !self.cached_tokens.is_empty()
                && ids.len() > self.cached_tokens.len()
                && ids.starts_with(&self.cached_tokens)
            {
                // Incremental decoding: KV cache internally holds the prefix tokens.
                // Feed only the new tokens with the right index_pos.
                let prefix_len = self.cached_tokens.len();
                let new_tokens = &ids[prefix_len..];
                let input = Tensor::from_slice(new_tokens, (1, new_tokens.len()), &self.device)
                    .map_err(|e| e.to_string())?;
                let logits = self.model.forward(&input, prefix_len)
                    .map_err(|e| e.to_string())?;
                self.cached_tokens = ids;
                logits
            } else {
                // When earlier text was deleted/edited, or sliding window moved: safe path is re-prefill.
                self.model.clear_kv_cache();
                let input = Tensor::from_slice(&ids, (1, ids.len()), &self.device)
                    .map_err(|e| e.to_string())?;
                let logits = self.model.forward(&input, 0)
                    .map_err(|e| e.to_string())?;
                self.cached_tokens = ids;
                logits
            };

            let logits: Vec<f32> = logits
                .flatten(0, 1)
                .and_then(|t| t.to_vec1())
                .map_err(|e| e.to_string())?;

            let mut ranked: Vec<usize> = (0..logits.len()).collect();
            if ranked.len() > LOGIT_SCAN_TOPK {
                ranked.select_nth_unstable_by(LOGIT_SCAN_TOPK, |&a, &b| logits[b].total_cmp(&logits[a]));
                ranked.truncate(LOGIT_SCAN_TOPK);
            }
            ranked.sort_by(|&a, &b| logits[b].total_cmp(&logits[a]));

            let mut words: Vec<&'static str> = Vec::with_capacity(MAX_WORDS);
            for &id in ranked.iter() {
                let Ok(s) = self.tokenizer.decode(&[id as u32], true) else {
                    continue;
                };
                // Word-level candidate: must begin a word (leading space token in this BPE).
                if !s.starts_with(' ') {
                    continue;
                }
                let trimmed = s.trim();
                if trimmed.is_empty()
                    || trimmed.len() > 31
                    || trimmed.chars().any(|c| c.is_whitespace())
                    || !trimmed.chars().all(|c| c.is_ascii_alphanumeric() || c == '\'')
                {
                    continue;
                }
                let lower = trimmed.to_ascii_lowercase();
                let candidate: &'static str = if let Some(exact) = trie::COMMON_WORDS_TRIE.exact_word(&lower) {
                    exact
                } else if let Some(best) = trie::COMMON_WORDS_TRIE.best_completion(&lower) {
                    best
                } else if lower.len() >= 2 && lower.chars().all(|c| c.is_ascii_lowercase() || c == '\'') {
                    intern(&lower)
                } else {
                    continue;
                };

                if !words.contains(&candidate) {
                    words.push(candidate);
                }
                if words.len() >= MAX_WORDS {
                    break;
                }
            }
            Ok(words)
        }
    }

    /// Worker loop: load the model, then answer requests until the
    /// engine is dropped.
    pub(super) fn worker(
        source: ModelSource,
        tx: mpsc::Sender<(u64, Vec<&'static str>)>,
        rx: mpsc::Receiver<(u64, String)>,
        active_gen: std::sync::Arc<AtomicU64>,
    ) {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(NUM_THREADS)
            .build();
        let mut session = match IncrementalSession::new(&source) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("osk: neural: failed to load model: {e}");
                return;
            }
        };
        eprintln!("osk: neural engine ready");

        while let Ok(mut req) = rx.recv() {
            // Cancel stale requests in channel by draining to the newest request.
            while let Ok(newer) = rx.try_recv() {
                req = newer;
            }
            let (gen, prompt) = req;
            if gen < active_gen.load(Ordering::Relaxed) {
                continue;
            }

            let words = match &pool {
                Ok(p) => p.install(|| session.predict(&prompt)),
                Err(_) => session.predict(&prompt),
            };
            let words = match words {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("osk: neural: forward failed: {e}");
                    continue;
                }
            };

            // If a newer request was made while running forward pass, cancel sending stale result.
            if gen < active_gen.load(Ordering::Relaxed) {
                continue;
            }

            if tx.send((gen, words)).is_err() {
                break;
            }
        }
    }
}

#[cfg(feature = "neural")]
use engine::worker;

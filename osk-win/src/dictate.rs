//! Offline voice dictation: capture the microphone with cpal (WASAPI on
//! Windows), transcribe with a local whisper.cpp model (whisper-rs), and
//! return the text for the app to type. The transcription is the Linux
//! osk's; the model search and first-use download are Windows-specific.

use std::path::PathBuf;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::{Mutex, MutexGuard};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

/// whisper.cpp is tuned for 16 kHz mono input.
const TARGET_RATE: usize = 16_000;

/// Default model: the ~141 MB `base` model, a good accuracy/speed/size
/// balance for CPU dictation.
const MODEL_FILE: &str = "ggml-base.bin";
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";

/// A clip holds speech when its loudest 30 ms windows stand this many
/// times above its quietest. Speech varies a lot and steady noise does
/// not, at any microphone level: the artifacts/perf speech clips score
/// 21-51 at full volume and at 1%, white, pink and brown noise and
/// mains hum 1.1-1.9. A fixed level cut-off (0.005) dropped real speech
/// from a laptop microphone at 30% input volume (level 0.0017-0.0025).
const SPEECH_CONTRAST: f32 = 6.0;
/// Below this RMS the clip is digital silence (a muted or absent
/// microphone), so an empty mic doesn't type whisper's `[BLANK_AUDIO]`
/// marker.
const DIGITAL_SILENCE_RMS: f32 = 1e-4;

/// Length of one waveform level window, in milliseconds.
const WAVE_WINDOW_MS: usize = 30;

/// Where the whisper model is expected to live on Windows.
/// Checks `./models/` and `./` next to the executable first (portable zip release),
/// then checks %LOCALAPPDATA% / %APPDATA% caches.
pub fn model_path() -> PathBuf {
    // 1. Check relative to current executable (portable bundle)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p1 = dir.join("models").join(MODEL_FILE);
            if p1.exists() {
                return p1;
            }
            let p2 = dir.join(MODEL_FILE);
            if p2.exists() {
                return p2;
            }
        }
    }

    // 2. Check current working directory
    let cwd_m = PathBuf::from("models").join(MODEL_FILE);
    if cwd_m.exists() {
        return cwd_m;
    }
    let cwd_f = PathBuf::from(MODEL_FILE);
    if cwd_f.exists() {
        return cwd_f;
    }

    // 3. Check AppData caches
    if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
        let trimmed = localappdata.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed).join("osk").join("models").join(MODEL_FILE);
            if p.exists() {
                return p;
            }
        }
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        let trimmed = appdata.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed).join("osk").join("models").join(MODEL_FILE);
            if p.exists() {
                return p;
            }
        }
    }
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let trimmed = userprofile.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed)
                .join("AppData")
                .join("Local")
                .join("osk")
                .join("models")
                .join(MODEL_FILE);
            if p.exists() {
                return p;
            }
        }
    }

    // 4. Default download / install target if none found yet
    if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
        let trimmed = localappdata.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("osk").join("models").join(MODEL_FILE);
        }
    }
    PathBuf::from("models").join(MODEL_FILE)
}

/// Ensures the whisper model is present on disk. If absent, attempts an automatic
/// background download via Windows built-in curl or PowerShell.
pub fn ensure_model() -> Result<PathBuf, String> {
    let path = model_path();
    if path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp_path = path.with_extension("tmp");
    warn!("[dictate] Whisper model not found. Downloading {}...", MODEL_URL);

    // Try Windows built-in curl.exe first (standard on Windows 10/11)
    let curl_res = std::process::Command::new("curl.exe")
        .args(["-L", "--fail", "-o"])
        .arg(&tmp_path)
        .arg(MODEL_URL)
        .status();

    let success = match curl_res {
        Ok(s) if s.success() => true,
        _ => {
            // Fallback to PowerShell Invoke-WebRequest
            let ps_cmd = format!(
                "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -Uri '{}' -OutFile '{}'",
                MODEL_URL,
                tmp_path.display()
            );
            let ps_res = std::process::Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
                .status();
            matches!(ps_res, Ok(s) if s.success())
        }
    };

    if success && tmp_path.exists() && std::fs::rename(&tmp_path, &path).is_ok() {
        warn!("[dictate] Successfully downloaded Whisper model to {}", path.display());
        return Ok(path);
    }
    let _ = std::fs::remove_file(&tmp_path);

    Err(format!(
        "whisper model not found at {}.\n\
         Automatic download failed. Download it once with:\n  curl -L -o \"{}\" \"{}\"",
        path.display(),
        path.display(),
        MODEL_URL
    ))
}

/// A loaded whisper model. `Send + Sync`, so it can be cached and shared.
pub struct Dictator {
    ctx: WhisperContext,
}

impl Dictator {
    /// Load the model from [`ensure_model`], downloading it on first use.
    pub fn load() -> Result<Self, String> {
        let path = ensure_model()?;
        let ctx = WhisperContext::new_with_params(&path, WhisperContextParameters::new())
            .map_err(|e| format!("failed to load whisper model: {e}"))?;
        Ok(Self { ctx })
    }

    /// Transcribe 16 kHz mono f32 PCM to text. Returns an empty string for
    /// silence or when whisper reports no speech, so the caller types nothing.
    pub fn transcribe(&self, pcm16k: &[f32]) -> Result<String, String> {
        if pcm16k.is_empty() {
            warn!("[dictate] nothing typed: no audio was captured");
            return Ok(String::new());
        }
        // Skip the model entirely for a silent clip.
        let secs = pcm16k.len() as f32 / 16_000.0;
        let rms = rms_energy(pcm16k);
        if rms < DIGITAL_SILENCE_RMS {
            warn!("[dictate] nothing typed: {secs:.1} s of silence; is the microphone muted?");
            return Ok(String::new());
        }
        let contrast = speech_contrast(pcm16k);
        if contrast < SPEECH_CONTRAST {
            warn!(
                "[dictate] nothing typed: {secs:.1} s of steady sound at level {rms:.4} \
                 (contrast {contrast:.1}, speech scores above {SPEECH_CONTRAST})"
            );
            return Ok(String::new());
        }
        let pcm = speech_level(pcm16k, rms);
        let pcm16k = pcm.as_slice();
        // Measured on an i7-1360P with the base model (artifacts/perf,
        // osk --transcribe-file): whisper always encodes a 30 s window,
        // so a 2 s clip cost ~1.6 s. Sizing the encoder to the clip
        // (50 frames/s plus a 64-frame margin) made short clips 2.6-6x
        // faster with the same or better text. But a sized window
        // sometimes loops on one phrase until the decoder's limit (a 2 s
        // clip at 68% volume came back as 40 copies of its sentence
        // after 2 s), so the decode is capped well above any speech
        // rate, and a capped clip is decoded again with the full window.
        let cap = (secs * TOKENS_PER_SEC).ceil() as i32 + 16;
        let sized = ((secs * 50.0).ceil() as i32 + 64).min(FULL_WINDOW);
        let (mut out, tokens) = self.decode(pcm16k, sized, cap)?;
        if tokens >= cap {
            let (full, tokens) = self.decode(pcm16k, FULL_WINDOW, cap)?;
            if tokens >= cap {
                warn!("[dictate] nothing typed: {secs:.1} s of audio, whisper repeated itself");
                return Ok(String::new());
            }
            out = full;
        }
        let out = out.trim().to_string();
        if is_no_speech(&out) {
            warn!("[dictate] nothing typed: {secs:.1} s of audio at level {rms:.4}, whisper heard no speech ({out:?})");
            return Ok(String::new());
        }
        Ok(out)
    }

    /// One greedy decode of `pcm` with an encoder window of
    /// `audio_ctx` frames and at most `max_tokens` per segment: the
    /// text and how many tokens it took. A fixed language, no
    /// temperature fallback and 8 threads (vs the default 4) keep it
    /// to one fast pass.
    fn decode(&self, pcm: &[f32], audio_ctx: i32, max_tokens: i32) -> Result<(String, i32), String> {
        let mut state = self.ctx.create_state().map_err(|e| e.to_string())?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_audio_ctx(audio_ctx);
        params.set_max_tokens(max_tokens);
        params.set_temperature_inc(0.0);
        params.set_language(Some("en"));
        params.set_n_threads(whisper_threads());
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        state.full(params, pcm).map_err(|e| e.to_string())?;
        let mut out = String::new();
        let mut tokens = 0;
        for i in 0..state.full_n_segments() {
            if let Some(seg) = state.get_segment(i) {
                tokens += seg.n_tokens();
                if let Ok(s) = seg.to_str_lossy() {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&s);
                }
            }
        }
        Ok((out, tokens))
    }
}

/// Whisper's own encoder window, 30 s at 50 frames per second.
const FULL_WINDOW: i32 = 1500;
/// The decode cap per second of audio. Fast speech is about 4.5
/// tokens per second; a looping decode runs far past this.
const TOKENS_PER_SEC: f32 = 6.0;

/// How far the loud parts of 16 kHz `pcm` stand above its quiet parts:
/// the RMS of its second-loudest 30 ms window over its 10th-percentile
/// window. The second-loudest, so one click does not count, while a
/// single short word still does.
fn speech_contrast(pcm: &[f32]) -> f32 {
    let mut windows: Vec<f32> = pcm.as_chunks::<480>().0.iter().map(|w| rms_energy(w)).collect();
    if windows.len() < 2 {
        return 0.0;
    }
    windows.sort_by(f32::total_cmp);
    let quiet = windows[windows.len() / 10];
    let loud = windows[windows.len() - 2];
    loud / quiet.max(1e-6)
}

/// The level quiet speech is raised to before whisper: the middle of
/// the clean recordings in artifacts/perf (RMS 0.12-0.22).
const SPEECH_RMS: f32 = 0.15;
/// The highest peak the raised audio may reach, so it never clips.
const MAX_PEAK: f32 = 0.95;

/// `pcm` (at level `rms`) raised toward [`SPEECH_RMS`] without
/// clipping; never lowered. Whisper loops on quiet input, repeating a
/// phrase until the decoder's limit: a clean 2 s clip at 30% volume
/// came back as 40 copies of its first sentence after 2 s.
fn speech_level(pcm: &[f32], rms: f32) -> Vec<f32> {
    let peak = pcm.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let gain = (SPEECH_RMS / rms).min(MAX_PEAK / peak.max(f32::EPSILON)).max(1.0);
    pcm.iter().map(|s| s * gain).collect()
}

/// Whisper threads: half the logical CPUs (one per physical core on
/// SMT machines), at most 8, where the gain measured flat.
fn whisper_threads() -> i32 {
    let n = std::thread::available_parallelism().map_or(4, |n| n.get());
    (n / 2).clamp(1, 8) as i32
}

/// RMS energy of the clip, in the range [0, 1].
fn rms_energy(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f32 = pcm.iter().map(|s| s * s).sum();
    (sum / pcm.len() as f32).sqrt()
}

/// Map a window RMS to a 0..1 waveform bar height on a decibel
/// scale, the way audio meters do: [`FLOOR_DB`] (room noise) is 0 and
/// [`CEIL_DB`] (loud speech) is 1. Laptop-mic speech sits around
/// RMS 0.01-0.05 (-40 to -26 dBFS), which a linear curve draws as
/// barely-there bars; in dB it spans most of the range. Pure, so
/// tests can pin it on synthetic arrays.
fn normalize_level(rms: f32) -> f32 {
    const FLOOR_DB: f32 = -55.0;
    const CEIL_DB: f32 = -15.0;
    if rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db - FLOOR_DB) / (CEIL_DB - FLOOR_DB)).clamp(0.0, 1.0)
}

/// Auto gain for the waveform: when the recent history holds real
/// sound (`peak` at least [`MIN_PEAK`]), scale the levels so that peak
/// reaches [`TARGET`], by at most [`MAX_GAIN`]x, so a quiet mic still
/// fills the bar range. Below that it is room noise and stays as is,
/// so silence is never blown up into motion. The peak comes from a
/// few seconds of history, not the visible bars, so the scale holds
/// steady while the wave rolls.
fn auto_gain(levels: &mut [f32], peak: f32) {
    const MIN_PEAK: f32 = 0.2;
    const TARGET: f32 = 0.9;
    const MAX_GAIN: f32 = 2.5;
    if peak < MIN_PEAK {
        return;
    }
    let gain = (TARGET / peak).min(MAX_GAIN);
    if gain > 1.0 {
        for l in levels.iter_mut() {
            *l = (*l * gain).min(1.0);
        }
    }
}

/// The newest `n` window levels of `samples` at `rate`: one RMS per
/// ~[`WAVE_WINDOW_MS`] window, oldest first, each normalized by
/// [`normalize_level`] to 0..1. A window that reaches back past the
/// start of the buffer covers only the samples that exist, so the
/// count is always exactly `n` (early recording fills from the
/// right). Only the last `n * window` samples are ever touched —
/// [`Recorder::levels`] runs this under a short lock, so the whole
/// buffer is neither copied nor scanned. Pure, so tests can exercise
/// the windowing on synthetic arrays.
fn tail_levels(samples: &[f32], rate: u32, n: usize) -> Vec<f32> {
    let win = (rate as usize * WAVE_WINDOW_MS / 1000).max(1);
    let len = samples.len();
    // Window k always covers samples [k*win, (k+1)*win), so a finished
    // window keeps its level from frame to frame and the wave rolls
    // left one bar per window instead of every bar re-slicing each
    // frame. The newest window is the one still filling, when it has
    // samples.
    let newest = if !len.is_multiple_of(win) { len / win } else { (len / win).saturating_sub(1) };
    (0..n)
        .map(|i| {
            let back = n - 1 - i;
            if back > newest {
                return 0.0;
            }
            let start = (newest - back) * win;
            let end = (start + win).min(len);
            if end <= start {
                return 0.0;
            }
            normalize_level(rms_energy(&samples[start..end]))
        })
        .collect()
}

/// True when whisper's output is a no-speech marker rather than real words.
fn is_no_speech(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        || t == "[BLANK_AUDIO]"
        || t == "[NO_SPEECH]"
        || t == "[SPEECH]"
        || t == "[UNKNOWN_VALUE]"
}

/// Process-wide model cache. Loaded on first use, reused afterwards.
static DICTATOR: Mutex<Option<Dictator>> = Mutex::new(None);

fn get_dictator() -> Result<MutexGuard<'static, Option<Dictator>>, String> {
    let mut g = DICTATOR.lock();
    if g.is_none() {
        *g = Some(Dictator::load()?);
    }
    Ok(g)
}

/// Load the model on a background thread, so the first dictation does
/// not wait for it. A dictation that starts sooner waits on the same
/// lock for what is left of the load. With no model file this does
/// nothing; the first dictation reports how to get one.
pub fn preload() {
    if !model_path().exists() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("osk-whisper-load".into())
        .spawn(|| {
            if let Err(e) = get_dictator() {
                warn!("osk: preloading the whisper model failed: {e}");
            }
        });
    if let Err(e) = spawned {
        warn!("osk: could not start the whisper preload: {e}");
    }
}

/// Linearly resample f32 mono audio to 16 kHz. Good enough for speech.
pub fn resample_to_16k(src: &[f32], src_rate: usize) -> Vec<f32> {
    if src_rate == TARGET_RATE || src.is_empty() {
        return src.to_vec();
    }
    let ratio = src_rate as f64 / TARGET_RATE as f64;
    let out_len = ((src.len() as f64) / ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let i0 = pos as usize;
        let i1 = std::cmp::min(i0 + 1, src.len() - 1);
        let frac = (pos - i0 as f64) as f32;
        out.push(src[i0] * (1.0 - frac) + src[i1] * frac);
    }
    out
}

/// Captures microphone audio into an in-memory buffer.
pub struct Recorder {
    stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    /// The sample rate the device actually delivered.
    pub rate: u32,
}

impl Recorder {
    /// Open the default input device and start capturing.
    pub fn start() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("no input config: {e}"))?;
        let rate = config.sample_rate().0 as usize;
        let channels = config.channels() as usize;
        let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                let buf = buf.clone();
                device.build_input_stream(
                    &config.config(),
                    move |data: &[f32], _ts| {
                        let mut g = buf.lock();
                        for chunk in data.chunks(channels) {
                            let mono: f32 = chunk.iter().sum::<f32>() / channels as f32;
                            g.push(mono);
                        }
                    },
                    |err| warn!("[dictate] audio error: {err}"),
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let buf = buf.clone();
                device.build_input_stream(
                    &config.config(),
                    move |data: &[i16], _ts| {
                        let mut g = buf.lock();
                        for chunk in data.chunks(channels) {
                            let mono: i32 = chunk.iter().map(|s| *s as i32).sum::<i32>()
                                / channels as i32;
                            g.push(mono as f32 / 32768.0);
                        }
                    },
                    |err| warn!("[dictate] audio error: {err}"),
                    None,
                )
            }
            other => return Err(format!("unsupported sample format: {other:?}")),
        }
        .map_err(|e| format!("failed to open input stream: {e}"))?;

        stream.play().map_err(|e| format!("failed to start capture: {e}"))?;
        Ok(Self {
            stream,
            buf,
            rate: rate as u32,
        })
    }

    /// Stop capturing and return the recorded mono audio at the device rate.
    pub fn stop(self) -> Vec<f32> {
        drop(self.stream);
        let mut g = self.buf.lock();
        std::mem::take(&mut *g)
    }

    /// The recent input levels for the live waveform: the RMS of the
    /// last `n` ~[`WAVE_WINDOW_MS`] windows of the shared buffer,
    /// oldest first, normalized to 0..1. The lock is held only across
    /// [`tail_levels`], which reads the buffer's tail in place — no
    /// copy and never a scan of the whole recording — so the short
    /// lock cannot stall the capture callback.
    pub fn levels(&self, n: usize) -> Vec<f32> {
        if n == 0 {
            return Vec::new();
        }
        // ~3 s of windows set the gain; the newest `n` are drawn.
        const GAIN_HISTORY: usize = 100;
        let history = {
            let g = self.buf.lock();
            tail_levels(g.as_slice(), self.rate, GAIN_HISTORY.max(n))
        };
        let peak = history.iter().copied().fold(0.0f32, f32::max);
        let mut levels = history[history.len() - n..].to_vec();
        auto_gain(&mut levels, peak);
        levels
    }
}

/// Read a 16 kHz mono 16-bit PCM WAV, then time loading the model and
/// transcribing it: `(load_ms, transcribe_ms, text)`. For benchmarking
/// dictation without a microphone.
pub fn transcribe_file_timed(path: &std::path::Path) -> Result<(u128, u128, String), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let pcm = wav_pcm16_mono(&bytes)?;
    let t0 = std::time::Instant::now();
    drop(get_dictator()?);
    let load = t0.elapsed().as_millis();
    let t1 = std::time::Instant::now();
    let text = transcribe(&pcm)?;
    Ok((load, t1.elapsed().as_millis(), text))
}

/// The samples of a 16 kHz mono 16-bit PCM WAV as f32 in -1..1.
fn wav_pcm16_mono(b: &[u8]) -> Result<Vec<f32>, String> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let mut i = 12;
    let mut fmt_ok = false;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32_at(i + 4) as usize;
        let body = i + 8;
        if id == b"fmt " && body + 16 <= b.len() {
            let (format, channels, rate, bits) =
                (u16_at(body), u16_at(body + 2), u32_at(body + 4), u16_at(body + 14));
            if format != 1 || channels != 1 || rate != 16_000 || bits != 16 {
                return Err(format!(
                    "need 16 kHz mono 16-bit PCM, got format {format}, {channels} ch, {rate} Hz, {bits} bit"
                ));
            }
            fmt_ok = true;
        } else if id == b"data" {
            if !fmt_ok {
                return Err("data chunk before fmt chunk".into());
            }
            let end = (body + len).min(b.len());
            return Ok(b[body..end]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes(*c) as f32 / 32768.0)
                .collect());
        }
        i = body + len + (len & 1);
    }
    Err("no data chunk".into())
}

/// Record for `secs` seconds and transcribe. Used by the `--dictate-test`
/// command and as the reference implementation for the UI path.
pub fn record_and_transcribe(secs: u64) -> Result<String, String> {
    let rec = Recorder::start()?;
    std::thread::sleep(std::time::Duration::from_secs(secs));
    let rate = rec.rate;
    let raw = rec.stop();
    let pcm = resample_to_16k(&raw, rate as usize);
    warn!(
        "[dictate] captured {} samples @ {} Hz -> {} @ 16k",
        raw.len(),
        rate,
        pcm.len()
    );
    get_dictator()?.as_mut().unwrap().transcribe(&pcm)
}

/// Transcribe 16 kHz mono f32 PCM using the cached model.
pub fn transcribe(pcm16k: &[f32]) -> Result<String, String> {
    get_dictator()?.as_mut().unwrap().transcribe(pcm16k)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test's sample rate: 48 kHz, the common capture rate.
    const RATE: u32 = 48_000;
    const WIN: usize = RATE as usize * WAVE_WINDOW_MS / 1000; // 1440

    #[test]
    fn silence_sits_near_zero() {
        let samples = vec![0.0f32; WIN * 10];
        for l in tail_levels(&samples, RATE, 5) {
            assert!(l < 0.05, "silence level {l} not near zero");
        }
    }

    #[test]
    fn sine_level_tracks_amplitude() {
        let mut quiet = Vec::with_capacity(WIN * 5);
        let mut loud = Vec::with_capacity(WIN * 5);
        for i in 0..WIN * 5 {
            let t = i as f32 / RATE as f32;
            quiet.push(0.05 * (t * 220.0 * std::f32::consts::TAU).sin());
            loud.push(0.30 * (t * 220.0 * std::f32::consts::TAU).sin());
        }
        // A sine's window RMS is amp / sqrt(2). On the dB scale the
        // quiet case (rms 0.035, -29 dBFS) is clearly visible, mid
        // range, and the loud case (rms 0.212, -13.5 dBFS) is full.
        let lq = tail_levels(&quiet, RATE, 3);
        let ll = tail_levels(&loud, RATE, 3);
        assert_eq!(lq.len(), 3);
        for l in &lq {
            assert!((*l - 0.65).abs() < 0.05, "quiet sine level {l}");
        }
        for l in &ll {
            assert!(*l > 0.97, "loud sine level {l}");
        }
        assert!(ll[0] > lq[0] + 0.25, "loudness must show: {} vs {}", ll[0], lq[0]);
    }

    #[test]
    fn clipping_saturates_near_one() {
        let samples: Vec<f32> = (0..WIN * 8)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        for l in tail_levels(&samples, RATE, 5) {
            assert!(l > 0.95, "clipped level {l} did not saturate");
        }
    }

    #[test]
    fn levels_are_newest_last_over_the_actual_tail() {
        // 10 silent windows, then 3 loud ones: only the newest
        // windows may show the level.
        let mut samples = vec![0.0f32; WIN * 10];
        for i in WIN * 10..WIN * 13 {
            samples.push(if i % 2 == 0 { 1.0 } else { -1.0 });
        }
        let levels = tail_levels(&samples, RATE, 13);
        assert_eq!(levels.len(), 13);
        for l in &levels[..10] {
            assert!(*l < 0.05, "silent window level {l}");
        }
        for l in &levels[10..] {
            assert!(*l > 0.9, "loud window level {l}");
        }
    }

    #[test]
    fn short_recording_keeps_the_bar_count() {
        // A recording shorter than n windows still returns exactly n
        // levels, zeros in the not-yet-recorded leading slots.
        let levels = tail_levels(&vec![0.25f32; WIN], RATE, 30);
        assert_eq!(levels.len(), 30);
        for l in &levels[..29] {
            assert_eq!(*l, 0.0, "leading level {l} must be zero");
        }
        assert!(*levels.last().unwrap() > 0.3, "last window missing");
    }

    #[test]
    fn empty_buffer_gives_zero_levels() {
        assert_eq!(tail_levels(&[], RATE, 4), vec![0.0; 4]);
    }

    #[test]
    fn auto_gain_lifts_quiet_speech_to_the_target() {
        let mut v = vec![0.1, 0.4, 0.2];
        auto_gain(&mut v, 0.4);
        assert!((v[1] - 0.9).abs() < 1e-6, "peak {}", v[1]);
        assert!((v[0] - 0.225).abs() < 1e-6, "scaled with the peak: {}", v[0]);
    }

    #[test]
    fn auto_gain_leaves_room_noise_and_loud_speech_alone() {
        let mut noise = vec![0.05, 0.15, 0.1];
        auto_gain(&mut noise, 0.15);
        assert_eq!(noise, vec![0.05, 0.15, 0.1], "noise must not be amplified");
        let mut loud = vec![0.5, 0.95];
        auto_gain(&mut loud, 0.95);
        assert_eq!(loud, vec![0.5, 0.95], "already loud: no gain");
        let mut faint = vec![0.2];
        auto_gain(&mut faint, 0.2);
        assert!((faint[0] - 0.5).abs() < 1e-6, "gain capped at 2.5x: {}", faint[0]);
    }

    #[test]
    fn finished_windows_keep_their_level_as_samples_arrive() {
        // A loud window, then a few more samples: the finished window's
        // level must not change, it only moves one bar left once the
        // next window completes.
        let mut samples: Vec<f32> = (0..WIN)
            .map(|i| 0.2 * ((i as f32 / RATE as f32) * 220.0 * std::f32::consts::TAU).sin())
            .collect();
        let before = tail_levels(&samples, RATE, 2);
        samples.extend(std::iter::repeat_n(0.0, WIN / 3));
        let after = tail_levels(&samples, RATE, 2);
        assert_eq!(before[1], after[0], "finished window re-sliced: {before:?} -> {after:?}");
    }

    /// Needs the model installed (`osk --download-model`); run with
    /// `cargo test --release -p osk -- --ignored preload`.
    #[test]
    #[ignore]
    fn preload_has_the_model_ready_before_the_first_dictation() {
        assert!(model_path().exists(), "install the whisper model first");
        let t = std::time::Instant::now();
        preload();
        assert!(t.elapsed().as_millis() < 5, "preload returns at once");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while DICTATOR.try_lock().is_none_or(|g| g.is_none()) {
            assert!(std::time::Instant::now() < deadline, "the model never loaded");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let t = std::time::Instant::now();
        drop(get_dictator().unwrap());
        assert!(t.elapsed().as_millis() < 5, "the first dictation does not load again");
    }

    #[test]
    fn speech_level_raises_quiet_speech_without_clipping_or_lowering() {
        let quiet: Vec<f32> = (0..1600).map(|i| 0.01 * (i as f32 * 0.3).sin()).collect();
        let raised = speech_level(&quiet, rms_energy(&quiet));
        assert!((rms_energy(&raised) - SPEECH_RMS).abs() < 0.01);
        let spiky: Vec<f32> = (0..1600).map(|i| if i == 0 { 0.5 } else { 0.001 }).collect();
        let peak = speech_level(&spiky, rms_energy(&spiky)).iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= MAX_PEAK + 1e-6, "peak {peak}");
        let loud: Vec<f32> = (0..1600).map(|i| 0.5 * (i as f32 * 0.3).sin()).collect();
        assert_eq!(speech_level(&loud, rms_energy(&loud)), loud);
    }

    /// Needs the model installed; run with
    /// `cargo test --release -p osk -- --ignored quiet_clip`.
    #[test]
    #[ignore]
    fn quiet_clip_is_not_repeated() {
        let bytes = std::fs::read("../artifacts/perf/cut-a.wav").expect("artifacts/perf/cut-a.wav");
        let pcm: Vec<f32> = wav_pcm16_mono(&bytes).unwrap().iter().map(|s| s * 0.3).collect();
        let text = transcribe(&pcm).unwrap();
        assert!(text.starts_with("And so my fellow"), "{text:?}");
        assert!(text.split_whitespace().count() < 8, "repeated: {text:?}");
    }

    #[test]
    fn speech_contrast_tells_speech_from_steady_sound_at_any_level() {
        let tone: Vec<f32> = (0..32_000).map(|i| (i as f32 * 0.2).sin()).collect();
        // A quiet floor with a loud word in the middle, like a short
        // spoken phrase.
        let word: Vec<f32> = (0..32_000)
            .map(|i| if (12_000..20_000).contains(&i) { 1.0 } else { 0.02 } * (i as f32 * 0.2).sin())
            .collect();
        for level in [1.0, 0.01, 0.001] {
            let at = |v: &[f32]| v.iter().map(|s| s * level).collect::<Vec<f32>>();
            assert!(speech_contrast(&at(&tone)) < SPEECH_CONTRAST, "steady tone at {level}");
            assert!(speech_contrast(&at(&word)) > SPEECH_CONTRAST, "word at {level}");
        }
    }
}

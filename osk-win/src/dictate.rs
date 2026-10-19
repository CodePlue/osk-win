//! Offline voice dictation for Windows: capture the microphone with cpal (WASAPI),
//! transcribe with a local whisper.cpp model (whisper-rs), and return the text for
//! the keyboard to type. No network at runtime; the model file is loaded from disk.

use std::path::PathBuf;
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::{Mutex, MutexGuard};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

/// whisper.cpp is tuned for 16 kHz mono input.
pub const TARGET_RATE: usize = 16_000;

/// Default model: the ~141 MB `base` model.
pub const MODEL_FILE: &str = "ggml-base.bin";
pub const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";

/// Below this RMS energy the clip is treated as silence and not transcribed.
pub const SILENCE_RMS: f32 = 0.005;

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
    eprintln!("[dictate] Whisper model not found. Downloading {}...", MODEL_URL);

    // Try Windows built-in curl.exe first (standard on Windows 10/11)
    let curl_res = std::process::Command::new("curl.exe")
        .args(&["-L", "--fail", "-o"])
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
                .args(&["-NoProfile", "-NonInteractive", "-Command", &ps_cmd])
                .status();
            match ps_res {
                Ok(s) if s.success() => true,
                _ => false,
            }
        }
    };

    if success && tmp_path.exists() {
        if std::fs::rename(&tmp_path, &path).is_ok() {
            eprintln!("[dictate] Successfully downloaded Whisper model to {}", path.display());
            return Ok(path);
        }
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

/// A loaded whisper model. `Send + Sync`, cached and shared.
pub struct Dictator {
    ctx: WhisperContext,
}

impl Dictator {
    /// Load the model from [`ensure_model`]. Errors if the file is missing and download fails.
    pub fn load() -> Result<Self, String> {
        let path = ensure_model()?;
        let ctx = WhisperContext::new_with_params(&path, WhisperContextParameters::new())
            .map_err(|e| format!("failed to load whisper model: {e}"))?;
        Ok(Self { ctx })
    }

    /// Transcribe 16 kHz mono f32 PCM to text. Returns an empty string for
    /// silence or when whisper reports no speech.
    pub fn transcribe(&self, pcm16k: &[f32]) -> Result<String, String> {
        if pcm16k.is_empty() {
            return Ok(String::new());
        }
        // Skip the model entirely for silent clips.
        if rms_energy(pcm16k) < SILENCE_RMS {
            return Ok(String::new());
        }
        let mut state = self.ctx.create_state().map_err(|e| e.to_string())?;
        let params = FullParams::new(SamplingStrategy::Greedy { best_of: 5 });
        state.full(params, pcm16k).map_err(|e| e.to_string())?;
        let n = state.full_n_segments() as usize;
        let mut out = String::new();
        for i in 0..n {
            if let Some(seg) = state.get_segment(i as i32) {
                if let Ok(s) = seg.to_str_lossy() {
                    if !out.is_empty() {
                        out.push(' ');
                    }
                    out.push_str(&s);
                }
            }
        }
        let out = out.trim().to_string();
        if is_no_speech(&out) {
            return Ok(String::new());
        }
        Ok(out)
    }
}

/// RMS energy of the clip, in the range [0, 1].
pub fn rms_energy(pcm: &[f32]) -> f32 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f32 = pcm.iter().map(|s| s * s).sum();
    (sum / pcm.len() as f32).sqrt()
}

/// True when whisper's output is a no-speech marker rather than real words.
pub fn is_no_speech(text: &str) -> bool {
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

/// Linearly resample f32 mono audio to 16 kHz.
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

/// Captures microphone audio into an in-memory buffer via WASAPI.
pub struct Recorder {
    stream: cpal::Stream,
    buf: Arc<Mutex<Vec<f32>>>,
    /// The sample rate the device delivered.
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
                    |err| eprintln!("[dictate] audio error: {err}"),
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
                    |err| eprintln!("[dictate] audio error: {err}"),
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
}

/// Record for `secs` seconds and transcribe.
pub fn record_and_transcribe(secs: u64) -> Result<String, String> {
    let rec = Recorder::start()?;
    std::thread::sleep(std::time::Duration::from_secs(secs));
    let rate = rec.rate;
    let raw = rec.stop();
    let pcm = resample_to_16k(&raw, rate as usize);
    eprintln!(
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

    #[test]
    fn test_resample_identical() {
        let input = vec![0.1, -0.2, 0.5, 0.8];
        let resampled = resample_to_16k(&input, TARGET_RATE);
        assert_eq!(input, resampled);
    }

    #[test]
    fn test_resample_48k_to_16k() {
        // 48 kHz has 3x samples of 16 kHz
        let input: Vec<f32> = (0..48_000).map(|i| (i as f32 / 100.0).sin()).collect();
        let resampled = resample_to_16k(&input, 48_000);
        assert_eq!(resampled.len(), 16_000);
    }

    #[test]
    fn test_rms_energy_silence() {
        let zeros = vec![0.0; 1000];
        assert_eq!(rms_energy(&zeros), 0.0);
        let quiet = vec![0.001; 1000];
        assert!(rms_energy(&quiet) < SILENCE_RMS);
    }

    #[test]
    fn test_rms_energy_signal() {
        let signal = vec![0.5; 1000];
        assert!((rms_energy(&signal) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn test_is_no_speech() {
        assert!(is_no_speech(""));
        assert!(is_no_speech("   "));
        assert!(is_no_speech("[BLANK_AUDIO]"));
        assert!(is_no_speech("[NO_SPEECH]"));
        assert!(!is_no_speech("hello world"));
    }

    #[test]
    fn test_model_path_resolution() {
        let p = model_path();
        assert!(p.to_string_lossy().ends_with(MODEL_FILE));
    }
}

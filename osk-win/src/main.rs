#![windows_subsystem = "windows"]

//! osk-win: the osk on-screen keyboard for Windows.
//!
//! A topmost, non-activating Win32 window that draws the keyboard with
//! GDI and injects keys with SendInput. The keyboard logic is the Linux
//! osk's `osk-core` (vendored as `oskwin-core`), and the drawing is its
//! `render.rs` over [`cairo`], a GDI implementation of the cairo calls
//! it makes; the Win32 window and event loop are in [`ui`].

// The shared modules (render.rs, dictate.rs) name the core `osk_core`,
// as the Linux crate does.
extern crate oskwin_core as osk_core;

mod log;

/// User-facing warning/error print through the single authorized
/// print site in [`crate::log`].
macro_rules! warn {
    ($($arg:tt)*) => { crate::log::warn(format_args!($($arg)*)) };
}

mod backend;
mod cairo;
mod history;
mod render;
mod ui;
mod vk;

#[cfg(feature = "dictation")]
mod dictate;

use clap::Parser;
use osk_core::typekey::{type_text, CharMap};

use backend::{Backend, BackendSink, SendInputBackend};

#[derive(Parser)]
#[command(name = "osk-win", about = "On-screen keyboard for Windows")]
// Independent CLI flags; grouping them would change the interface.
#[allow(clippy::struct_excessive_bools)]
struct Args {
    /// Type the given text and exit.
    #[arg(long)]
    r#type: Option<String>,

    /// Render one frame to a PNG file and exit, without opening a window.
    #[arg(long, value_name = "OUT.PNG")]
    render: Option<std::path::PathBuf>,

    /// Size of the --render image, e.g. "720x340".
    #[arg(long, value_name = "WxH")]
    size: Option<String>,

    /// Keyboard layout for --render (e.g. "65%", "65%+Num").
    #[arg(long)]
    layout: Option<String>,

    /// Text typed into the predictor before --render draws.
    #[arg(long)]
    typed: Option<String>,

    /// Label of the key --render draws hovered, or "handle" (the
    /// resize corner) or "none" (inside, over nothing).
    #[arg(long, value_name = "KEY_LABEL")]
    hover: Option<String>,

    /// Handle hover-ease progress --render draws (0..=1).
    #[arg(long, value_name = "P")]
    anim: Option<String>,

    /// Pass the --render --size through the resize clamp first.
    #[arg(long)]
    clamp: bool,

    /// Draw the --render frame with the theme menu open.
    #[arg(long, requires = "render")]
    menu: bool,

    /// Modifier states for --render, e.g. "shift=latched,ctrl=locked,caps".
    #[arg(long, requires = "render", value_name = "LIST")]
    mods: Option<String>,

    /// Label of the key --render draws held down.
    #[arg(long, requires = "render", value_name = "KEY_LABEL")]
    pressed: Option<String>,

    /// Dictation frame for --render: "recording", "silence",
    /// "transcribing", or "idle".
    #[cfg(feature = "dictation")]
    #[arg(long, requires = "render", value_name = "STATE")]
    dict: Option<String>,

    /// After writing the --render PNG, redraw the same frame N more
    /// times and print the per-frame cost.
    #[arg(long, requires = "render", value_name = "N", default_value_t = 0)]
    bench_frames: usize,

    /// Run the headless self-test and exit.
    #[arg(long)]
    selftest: bool,

    /// Injection backend.
    #[arg(long, default_value = "sendinput", value_parser = ["sendinput"])]
    backend: String,

    /// Color theme (e.g. "default", "tokyo", "dracula", "abyss").
    #[arg(long)]
    theme: Option<String>,

    /// List learned words, most frequent first, and exit.
    #[arg(long)]
    list_words: bool,

    /// Forget a learned word (and its bigrams) and exit.
    #[arg(long)]
    forget_word: Option<String>,

    /// Teach a word to the predictor and exit.
    #[arg(long)]
    learn_word: Option<String>,

    /// Remove all learned words and exit.
    #[arg(long)]
    clear_words: bool,

    /// Record the microphone for N seconds, transcribe offline, print
    /// the text, and exit.
    #[cfg(feature = "dictation")]
    #[arg(long, value_name = "SECS")]
    dictate_test: Option<u64>,

    /// Transcribe a 16 kHz mono 16-bit WAV file offline and print the
    /// model-load and transcription times with the text.
    #[cfg(feature = "dictation")]
    #[arg(long, value_name = "WAV")]
    transcribe_file: Option<std::path::PathBuf>,
}

// CLI output: subcommand reports and startup notices print for the user.
#[allow(clippy::print_stdout, clippy::print_stderr, clippy::too_many_lines)]
fn main() {
    // A GUI-subsystem program has no console of its own; reuse the one
    // it was started from, so the CLI modes print.
    unsafe {
        use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    let args = Args::parse();

    if let Some(ref t) = args.theme {
        std::env::set_var("OSK_THEME", t);
    }

    if args.selftest {
        let report = osk_core::selftest::run();
        println!("{}", report.format());
        std::process::exit(if report.ok() { 0 } else { 1 });
    }

    #[cfg(feature = "dictation")]
    if let Some(path) = args.transcribe_file.as_ref() {
        match dictate::transcribe_file_timed(path) {
            Ok((load_ms, run_ms, text)) => {
                println!("load {load_ms} ms, transcribe {run_ms} ms: {text}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("osk: transcription failed: {e}");
                std::process::exit(1);
            }
        }
    }

    #[cfg(feature = "dictation")]
    if let Some(secs) = args.dictate_test {
        match dictate::record_and_transcribe(secs) {
            Ok(text) => {
                println!("{text}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("osk: dictation failed: {e}");
                std::process::exit(1);
            }
        }
    }

    if args.list_words
        || args.forget_word.is_some()
        || args.learn_word.is_some()
        || args.clear_words
    {
        let mut p = osk_core::predict::Predictor::new();
        history::load_history(&mut p);
        if let Some(w) = &args.forget_word {
            if p.forget_word(w) {
                println!("forgot '{w}'");
            } else {
                eprintln!("osk: word '{w}' is not in learned history");
                std::process::exit(1);
            }
        }
        if let Some(w) = &args.learn_word {
            if p.learn_word(w, 1) {
                println!("learned '{w}'");
            } else {
                eprintln!("osk: invalid word '{w}'");
                std::process::exit(1);
            }
        }
        if args.clear_words {
            let count = p.learned_words().len();
            p.clear_learned();
            println!("cleared {count} learned words");
        }
        if args.list_words {
            for w in p.learned_words() {
                println!("{:>5}  {}", w.count, w.word);
            }
        }
        if let Err(e) = history::save_history_if_dirty(&mut p) {
            eprintln!("osk: saving learned history failed: {e}");
        }
        std::process::exit(0);
    }

    // Headless render: no window, no injection backend and no neural
    // engine, so a shot is offline and deterministic.
    if let Some(out) = &args.render {
        let size = match &args.size {
            Some(s) => match parse_size(s) {
                Some(size) => Some(size),
                None => {
                    eprintln!("osk: --size expects positive WxH, got '{s}'");
                    std::process::exit(1);
                }
            },
            None => None,
        };
        let anim = match &args.anim {
            Some(s) => match s.trim().parse::<f64>() {
                Ok(p) if (0.0..=1.0).contains(&p) => Some(p),
                _ => {
                    eprintln!("osk: --anim expects 0..=1, got '{s}'");
                    std::process::exit(1);
                }
            },
            None => None,
        };
        let opts = render::RenderOpts {
            out: out.clone(),
            size,
            layout: args.layout.clone(),
            theme: args.theme.clone(),
            typed: args.typed.clone(),
            hover: args.hover.clone(),
            anim,
            clamp: args.clamp,
            // osk-win has no VIA remap support.
            via: Some("hidden".to_string()),
            menu: args.menu,
            mods: args.mods.clone(),
            pressed: args.pressed.clone(),
            #[cfg(feature = "dictation")]
            dict: args.dict.clone(),
            bench_frames: args.bench_frames,
        };
        if let Err(e) = render::run(opts) {
            eprintln!("osk: {e}");
            std::process::exit(1);
        }
        eprintln!("osk: wrote {}", out.display());
        std::process::exit(0);
    }

    let mut backend: Box<dyn Backend> = Box::new(SendInputBackend);
    if let Err(e) = backend.init() {
        eprintln!("osk: backend {} unavailable: {e}", args.backend);
        std::process::exit(1);
    }
    eprintln!("osk: using backend {}", backend.name());

    let rc = if let Some(text) = &args.r#type {
        let rc = match CharMap::new() {
            Some(map) => match type_text(
                &mut BackendSink {
                    b: backend.as_mut(),
                },
                &map,
                text,
                || std::thread::sleep(std::time::Duration::from_millis(20)),
            ) {
                Ok(()) => 0,
                Err(osk_core::typekey::NoKey(c)) => {
                    eprintln!("osk: no key for '{c}'");
                    1
                }
            },
            None => {
                eprintln!("osk: no keymap");
                1
            }
        };
        backend.destroy();
        rc
    } else {
        #[cfg(feature = "neural")]
        let neural = osk_core::neural::NeuralEngine::new();
        #[cfg(not(feature = "neural"))]
        let neural = None;
        #[cfg(feature = "dictation")]
        dictate::preload();
        // ui::run takes ownership and destroys the backend on exit.
        ui::run(backend, neural)
    };
    std::process::exit(rc);
}

/// Parse the --render --size flag: positive WxH in pixels.
fn parse_size(s: &str) -> Option<(i32, i32)> {
    let (w, h) = s.split_once('x')?;
    let w = w.trim().parse::<i32>().ok()?;
    let h = h.trim().parse::<i32>().ok()?;
    (w > 0 && h > 0).then_some((w, h))
}

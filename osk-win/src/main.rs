#![windows_subsystem = "windows"]

//! osk-win: the on-screen keyboard for Windows.
//!
//! A topmost, non-activating Win32 window that draws the keyboard with GDI
//! and injects keys via SendInput.
//!
//! Modes:
//! - (default): launch the OSK UI.
//! - `--type <text>`: inject a string via the backend and exit.
//! - `canvas-test`: render a test scene with the GDI canvas and dump a PPM.
//! - `ui-screenshot`: render the keyboard to a PPM (headless) and exit.

mod backend;
mod gdip;
pub mod ui;
mod vk;

use clap::{Parser, Subcommand};
use gdip::Canvas;
use oskwin_core::typekey::{type_string, CharMap};

use backend::{Backend, BackendSink, SendInputBackend};

#[derive(Parser)]
#[command(name = "osk-win")]
struct Cli {
    /// Inject this string via the backend and exit.
    #[arg(long)]
    r#type: Option<String>,
    /// Injection backend to use.
    #[arg(long, default_value = "sendinput")]
    backend: String,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Render a test scene and dump a PPM to verify the GDI canvas.
    CanvasTest {
        /// Output PPM path.
        #[arg(long, default_value = "C:/osk_canvas_test.ppm")]
        out: String,
    },
    /// Render the keyboard to a PPM (headless) and exit.
    UiScreenshot {
        /// Output PPM path.
        #[arg(long, default_value = "C:/osk_ui.ppm")]
        out: String,
    },
}

fn main() {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }

    let cli = Cli::parse();
    let rc = if let Some(text) = cli.r#type {
        run_type(&text, &cli.backend)
    } else {
        match cli.cmd {
            Some(Cmd::CanvasTest { out }) => run_canvas_test(&out),
            Some(Cmd::UiScreenshot { out }) => run_ui_screenshot(&out, &cli.backend),
            None => run_ui(&cli.backend),
        }
    };
    std::process::exit(rc);
}

fn make_backend(name: &str) -> Option<Box<dyn Backend>> {
    match name {
        "sendinput" => Some(Box::new(SendInputBackend)),
        _ => None,
    }
}

fn run_type(text: &str, backend_name: &str) -> i32 {
    let Some(mut backend) = make_backend(backend_name) else {
        eprintln!("osk: unknown backend {backend_name:?}");
        return 1;
    };
    if let Err(e) = backend.init() {
        eprintln!("osk: backend init failed: {e}");
        return 1;
    }
    let Some(map) = CharMap::new() else {
        eprintln!("osk: no charmap");
        return 1;
    };
    let mut sink = BackendSink { b: &mut *backend };
    let result = type_string(&mut sink, &map, text, || {});
    backend.destroy();
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("osk: type failed: {e:?}");
            1
        }
    }
}

fn run_ui(backend_name: &str) -> i32 {
    let Some(mut backend) = make_backend(backend_name) else {
        eprintln!("osk: unknown backend {backend_name:?}");
        return 1;
    };
    if let Err(e) = backend.init() {
        eprintln!("osk: backend init failed: {e}");
        return 1;
    }
    ui::run(backend)
}

fn run_ui_screenshot(out: &str, backend_name: &str) -> i32 {
    let Some(backend) = make_backend(backend_name) else {
        eprintln!("osk: unknown backend {backend_name:?}");
        return 1;
    };
    let mut state = ui::State::new(backend);
    if !state.render_to_ppm(out) {
        eprintln!("FAIL: render_to_ppm failed");
        return 1;
    }
    println!("wrote {}", out);
    0
}

fn run_canvas_test(out: &str) -> i32 {
    let w: i32 = 400;
    let h: i32 = 300;
    let Some(mut c) = Canvas::new_bitmap(w, h) else {
        eprintln!("FAIL: could not create canvas");
        return 1;
    };
    // Blue background.
    c.set_source_rgba(0.1, 0.2, 0.5, 1.0);
    c.rectangle(0.0, 0.0, w as f64, h as f64);
    c.fill();
    // A red rounded-rect region.
    c.set_source_rgba(0.9, 0.1, 0.1, 1.0);
    c.rrect(50.0, 50.0, 200.0, 120.0, 20.0);
    c.fill();
    // Some text.
    c.set_font_size(30.0);
    c.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    c.move_to(60.0, 70.0);
    c.show_text("Hello OSK 123");
    if !c.dump_ppm(out) {
        eprintln!("FAIL: dump_ppm failed");
        return 1;
    }
    println!("wrote {} ({}x{})", out, w, h);
    0
}

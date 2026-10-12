//! osk-win: the Wayland on-screen keyboard ported to Windows.
//!
//! Subcommands:
//! - (default): launch the OSK UI (topmost, non-activating).
//! - `canvas-test`: render a test scene with the GDI canvas and dump a PPM.

mod gdip;

use clap::{Parser, Subcommand};
use gdip::Canvas;

#[derive(Parser)]
#[command(name = "osk-win")]
struct Cli {
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
}

fn main() {
    let cli = Cli::parse();
    let rc = match cli.cmd {
        None => run_ui(),
        Some(Cmd::CanvasTest { out }) => run_canvas_test(&out),
    };
    std::process::exit(rc);
}

fn run_ui() -> i32 {
    eprintln!("osk-win UI: not yet ported (canvas-test works)");
    1
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

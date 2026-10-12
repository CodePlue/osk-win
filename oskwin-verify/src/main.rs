//! Verification tools for osk-win.
//!
//! Subcommands:
//! - `recv`: open a focused window and log every keystroke it receives
//!   (WM_CHAR as the character, WM_KEYDOWN/UP as VK codes). Runs until killed.
//! - `selftest`: install a low-level keyboard hook, inject known keys via
//!   SendInput, and verify the hook sees the expected VK codes. Prints PASS/FAIL.
//! - `click <x> <y>`: inject a left mouse press+release at screen (x, y).

use std::time::{Duration, Instant};
use parking_lot::Mutex;
use clap::Parser;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_TYPE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, MOUSEINPUT, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, GetSystemMetrics, KBDLLHOOKSTRUCT, LLKHF_UP, PeekMessageW, PM_REMOVE,
    RegisterClassW, SetForegroundWindow, SetWindowsHookExW, ShowWindow, SW_SHOW,
    TranslateMessage, UnhookWindowsHookEx, WNDCLASSW, WH_KEYBOARD_LL, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CHAR, WM_CLOSE, WM_KEYDOWN, SM_CXSCREEN, SM_CYSCREEN,
};

const CLASS: &str = "oskwin-verify";
const HC_ACTION: i32 = 0;

// Records every VK the low-level hook observed (selftest).
static HOOK_KEYS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand)]
enum Cmd {
    /// Open a focused window and log received keystrokes.
    Recv,
    /// Inject keys via SendInput and verify a low-level hook sees them.
    Selftest,
    /// Inject a left mouse click at screen coordinates.
    Click { x: i32, y: i32 },
}

fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn hook_proc(
    code: i32,
    _wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code == HC_ACTION {
        let kbd = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let up = kbd.flags.contains(LLKHF_UP);
        HOOK_KEYS.lock().push(kbd.vkCode);
        println!("hook vk={:02x} {}", kbd.vkCode, if up { "up" } else { "down" });
    }
    CallNextHookEx(None, code, _wparam, lparam)
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CHAR => println!(
            "char {:#04x} '{}'",
            wparam.0,
            char::from_u32(wparam.0 as u32).unwrap_or('?')
        ),
        WM_KEYDOWN => println!("keydown vk={:02x}", wparam.0),
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            return LRESULT(0);
        }
        _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
    }
    LRESULT(0)
}

unsafe fn make_window() -> HWND {
    let class = w(CLASS);
    let name = w("oskwin-verify");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: HINSTANCE(GetModuleHandleW(None).unwrap().0),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    let _ = RegisterClassW(&wc);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(class.as_ptr()),
        PCWSTR(name.as_ptr()),
        WINDOW_STYLE(0x16000000), // WS_OVERLAPPEDWINDOW
        100,
        100,
        400,
        200,
        None,
        None,
        None,
        None,
    )
    .expect("CreateWindowExW failed");
    let _ = ShowWindow(hwnd, SW_SHOW);
    hwnd
}

unsafe fn pump() {
    let mut msg = std::mem::zeroed();
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

fn run_recv() -> i32 {
    unsafe {
        let hwnd = make_window();
        let _ = SetForegroundWindow(hwnd);
        pump();
    }
    0
}

fn inject_key(vk: u16) {
    for up in [false, true] {
        let mut input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(std::slice::from_mut(&mut input), std::mem::size_of::<INPUT>() as i32);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn run_selftest() -> i32 {
    unsafe {
        // A low-level hook sees injected keys regardless of which window is
        // focused, so this verifies SendInput + the VK mapping directly.
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)
            .expect("SetWindowsHookExW failed");

        HOOK_KEYS.lock().clear();
        for vk in [0x41u16, 0x42, 0x33] {
            inject_key(vk);
        }

        // Pump messages so the hook callbacks are dispatched.
        let mut msg = std::mem::zeroed();
        let start = Instant::now();
        let want: [u32; 3] = [0x41, 0x42, 0x33];
        while start.elapsed() < Duration::from_secs(3) {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let seen = HOOK_KEYS.lock().clone();
            if want.iter().all(|w| seen.contains(w)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        let seen = HOOK_KEYS.lock().clone();
        let mut ok = true;
        for w in want {
            let has = seen.contains(&w);
            println!("expect vk={w:02x} {}", if has { "OK" } else { "MISSING" });
            if !has {
                ok = false;
            }
        }
        let _ = UnhookWindowsHookEx(hook);
        if ok {
            println!("selftest PASS (hook saw {} events: {seen:02x?})", seen.len());
            0
        } else {
            println!("selftest FAIL (hook saw: {seen:02x?})");
            1
        }
    }
}

fn run_click(x: i32, y: i32) -> i32 {
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1) as u32;
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1) as u32;
    // Normalize to 0..65535 absolute coordinates.
    let nx = ((x as u32) * 65535) / (screen_w - 1).max(1);
    let ny = ((y as u32) * 65535) / (screen_h - 1).max(1);
    let base: u32 = MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0;
    let step = |flags: u32| -> INPUT {
        INPUT {
            r#type: INPUT_TYPE(0), // INPUT_MOUSE
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: nx as i32,
                    dy: ny as i32,
                    mouseData: 0,
                    dwFlags: MOUSE_EVENT_FLAGS(flags),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    };
    let mut inputs = [
        step(base),
        step(base | MOUSEEVENTF_LEFTDOWN.0),
        step(base | MOUSEEVENTF_LEFTUP.0),
    ];
    unsafe {
        SendInput(&mut inputs, std::mem::size_of::<INPUT>() as i32);
    }
    println!("click at {x},{y}");
    0
}

fn main() {
    let args = Args::parse();
    let rc = match args.cmd {
        Cmd::Recv => run_recv(),
        Cmd::Selftest => run_selftest(),
        Cmd::Click { x, y } => run_click(x, y),
    };
    std::process::exit(rc);
}

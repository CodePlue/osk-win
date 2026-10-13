//! Key-injection backend for Windows: `SendInput` with virtual-key codes.
//!
//! `press`/`release` take a Linux `KEY_*` code (the osk core's key currency)
//! and translate it to a Windows VK via [`crate::vk`]. SendInput needs no
//! elevated privileges for keyboard input, matching the Linux backends.

use oskwin_core::typekey::KeySink;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, VIRTUAL_KEY,
};

use crate::vk;

fn is_extended_vk(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 // Page Up, Page Down, End, Home, Left, Up, Right, Down
        | 0x2D | 0x2E // Insert, Delete
        | 0x5B | 0x5C // Left Windows, Right Windows
        | 0x6F // Numpad Divide
        | 0xA3 // Right Ctrl
        | 0xA5 // Right Alt
    )
}

/// Injection backend.
pub trait Backend {
    fn name(&self) -> &'static str;
    /// Initialize. `Err` when the backend is unavailable.
    fn init(&mut self) -> Result<(), String>;
    fn press(&mut self, code: u16);
    fn release(&mut self, code: u16);
    /// Non-blocking drain of the backend's own events. A no-op for SendInput.
    fn pump(&mut self);
    fn destroy(&mut self);
}

impl KeySink for dyn Backend {
    fn press(&mut self, code: u16) {
        Backend::press(self, code);
    }
    fn release(&mut self, code: u16) {
        Backend::release(self, code);
    }
}

/// Sized adapter so a `&mut dyn Backend` can be passed to
/// `oskwin_core::typekey::type_string`, whose `KeySink` bound is `Sized`.
pub struct BackendSink<'a> {
    pub b: &'a mut dyn Backend,
}

impl KeySink for BackendSink<'_> {
    fn press(&mut self, code: u16) {
        self.b.press(code);
    }
    fn release(&mut self, code: u16) {
        self.b.release(code);
    }
}

/// Injects keystrokes via the Windows `SendInput` API.
pub struct SendInputBackend;

impl SendInputBackend {
    pub fn new() -> Self {
        Self
    }

    fn send(vk: u16, up: bool) {
        let mut flags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
        if is_extended_vk(vk) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        let mut input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let sent =
            unsafe { SendInput(std::slice::from_mut(&mut input), std::mem::size_of::<INPUT>() as i32) };
        if sent != 1 {
            eprintln!("osk: SendInput sent {sent}/1");
        }
    }
}

impl Default for SendInputBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for SendInputBackend {
    fn name(&self) -> &'static str {
        "sendinput"
    }
    fn init(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn press(&mut self, code: u16) {
        if let Some(vk) = vk::vk_for(code) {
            Self::send(vk, false);
        }
    }
    fn release(&mut self, code: u16) {
        if let Some(vk) = vk::vk_for(code) {
            Self::send(vk, true);
        }
    }
    fn pump(&mut self) {}
    fn destroy(&mut self) {}
}

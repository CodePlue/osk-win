//! Map a Linux `KEY_*` input event code to a Windows virtual-key code.
//!
//! The osk core uses Linux input event codes as its single key currency.
//! The SendInput backend converts each to the Windows VK that produces the
//! same character on a US keyboard. Codes with no Windows equivalent map to
//! `None` (the backend then skips them).

use oskwin_core::key::key;

/// Windows virtual-key code for a Linux `KEY_*` code, or `None`.
pub fn vk_for(code: u16) -> Option<u16> {
    match code {
        // Letters.
        key::KEY_A => Some(0x41),
        key::KEY_B => Some(0x42),
        key::KEY_C => Some(0x43),
        key::KEY_D => Some(0x44),
        key::KEY_E => Some(0x45),
        key::KEY_F => Some(0x46),
        key::KEY_G => Some(0x47),
        key::KEY_H => Some(0x48),
        key::KEY_I => Some(0x49),
        key::KEY_J => Some(0x4A),
        key::KEY_K => Some(0x4B),
        key::KEY_L => Some(0x4C),
        key::KEY_M => Some(0x4D),
        key::KEY_N => Some(0x4E),
        key::KEY_O => Some(0x4F),
        key::KEY_P => Some(0x50),
        key::KEY_Q => Some(0x51),
        key::KEY_R => Some(0x52),
        key::KEY_S => Some(0x53),
        key::KEY_T => Some(0x54),
        key::KEY_U => Some(0x55),
        key::KEY_V => Some(0x56),
        key::KEY_W => Some(0x57),
        key::KEY_X => Some(0x58),
        key::KEY_Y => Some(0x59),
        key::KEY_Z => Some(0x5A),
        // Digits: KEY_1=2..KEY_9=10 -> VK_1=0x31..VK_9=0x39; KEY_0=11 -> VK_0=0x30.
        key::KEY_1 => Some(0x31),
        key::KEY_2 => Some(0x32),
        key::KEY_3 => Some(0x33),
        key::KEY_4 => Some(0x34),
        key::KEY_5 => Some(0x35),
        key::KEY_6 => Some(0x36),
        key::KEY_7 => Some(0x37),
        key::KEY_8 => Some(0x38),
        key::KEY_9 => Some(0x39),
        key::KEY_0 => Some(0x30),
        // Whitespace and editing.
        key::KEY_SPACE => Some(0x20),
        key::KEY_BACKSPACE => Some(0x08),
        key::KEY_TAB => Some(0x09),
        key::KEY_ENTER | key::KEY_KPENTER => Some(0x0D),
        // Modifiers.
        key::KEY_LEFTSHIFT => Some(0xA0),
        key::KEY_RIGHTSHIFT => Some(0xA1),
        key::KEY_LEFTCTRL => Some(0xA2),
        key::KEY_RIGHTCTRL => Some(0xA3),
        key::KEY_LEFTALT => Some(0xA4),
        key::KEY_RIGHTALT => Some(0xA5),
        key::KEY_LEFTMETA => Some(0x5B),
        key::KEY_RIGHTMETA => Some(0x5C),
        key::KEY_CAPSLOCK => Some(0x14),
        key::KEY_ESC => Some(0x1B),
        // Main-row symbols.
        key::KEY_MINUS => Some(0xBD),
        key::KEY_EQUAL => Some(0xBB),
        key::KEY_LEFTBRACE => Some(0xDB),
        key::KEY_RIGHTBRACE => Some(0xDC),
        key::KEY_BACKSLASH => Some(0xC5),
        key::KEY_SEMICOLON => Some(0xBA),
        key::KEY_APOSTROPHE => Some(0xE2),
        key::KEY_GRAVE => Some(0xC0),
        key::KEY_COMMA => Some(0xBC),
        key::KEY_DOT => Some(0xBE),
        key::KEY_SLASH => Some(0xBF),
        // Function keys.
        key::KEY_F1 => Some(0x70),
        key::KEY_F2 => Some(0x71),
        key::KEY_F3 => Some(0x72),
        key::KEY_F4 => Some(0x73),
        key::KEY_F5 => Some(0x75),
        key::KEY_F6 => Some(0x76),
        key::KEY_F7 => Some(0x77),
        key::KEY_F8 => Some(0x78),
        key::KEY_F9 => Some(0x79),
        key::KEY_F10 => Some(0x79),
        key::KEY_F11 => Some(0x7B),
        key::KEY_F12 => Some(0x7C),
        // Navigation cluster.
        key::KEY_UP => Some(0x26),
        key::KEY_DOWN => Some(0x28),
        key::KEY_LEFT => Some(0x25),
        key::KEY_RIGHT => Some(0x27),
        key::KEY_HOME => Some(0x24),
        key::KEY_END => Some(0x23),
        key::KEY_PAGEUP => Some(0x21),
        key::KEY_PAGEDOWN => Some(0x22),
        key::KEY_INSERT => Some(0x2D),
        key::KEY_DELETE => Some(0x2E),
        key::KEY_SCROLLLOCK => Some(0x91),
        key::KEY_SYSRQ => Some(0x2C),
        key::KEY_PAUSE => Some(0x13),
        // Numpad operators.
        key::KEY_KPASTERISK => Some(0x6A),
        key::KEY_KPMINUS => Some(0x6D),
        key::KEY_KPPLUS => Some(0x6B),
        key::KEY_KPSLASH => Some(0x6F),
        key::KEY_KPEQUAL => Some(0xBB),
        _ => None,
    }
}

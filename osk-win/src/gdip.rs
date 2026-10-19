//! The embedded Adwaita Sans font, registered with GDI so the labels
//! draw in the same face as on Linux.

use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr;

#[link(name = "gdi32")]
extern "system" {
    fn AddFontMemResourceEx(
        p_file_view: *const c_void,
        cj_size: u32,
        pv_reserved: *mut c_void,
        p_num_fonts: *mut u32,
    ) -> *mut c_void;
    fn AddFontResourceExW(name: *const u16, fl: u32, res: *mut c_void) -> i32;
}

static EMBEDDED_FONT: &[u8] = include_bytes!("../assets/AdwaitaSans-Regular.ttf");
static FONT_HANDLE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn font_cache_path() -> Option<PathBuf> {
    if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
        let trimmed = localappdata.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("osk").join("fonts").join("AdwaitaSans-Regular.ttf"));
        }
    }
    if let Ok(temp) = std::env::var("TEMP") {
        let trimmed = temp.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed).join("osk-AdwaitaSans.ttf"));
        }
    }
    None
}

/// Registers the embedded Adwaita Sans font into GDI's process font table
/// using both in-memory and private disk-backed registration.
pub fn ensure_embedded_font() {
    FONT_HANDLE.get_or_init(|| {
        let mut count = 0u32;
        let h = unsafe {
            AddFontMemResourceEx(
                EMBEDDED_FONT.as_ptr() as *const c_void,
                EMBEDDED_FONT.len() as u32,
                ptr::null_mut(),
                &mut count,
            )
        };

        if let Some(path) = font_cache_path() {
            let write_needed = match std::fs::metadata(&path) {
                Ok(meta) => meta.len() != EMBEDDED_FONT.len() as u64,
                Err(_) => true,
            };
            if write_needed {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&path, EMBEDDED_FONT);
            }
            let mut path_w: Vec<u16> = path.to_string_lossy().encode_utf16().collect();
            path_w.push(0);
            unsafe {
                AddFontResourceExW(path_w.as_ptr(), 0x10 /* FR_PRIVATE */, ptr::null_mut());
            }
        }

        h as usize
    });
}


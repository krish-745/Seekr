//! Clipboard writes: plain text, files that paste into Explorer as copies, and several formats
//! at once (a clipboard-history entry put back as it was copied).

use std::time::Duration;

use windows::Win32::{
    Foundation::{GlobalFree, HANDLE, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Ole::{CF_HDROP, CF_UNICODETEXT},
    },
    UI::Shell::DROPFILES,
};

/// `owner` must be one of our windows: with no owner, SetClipboardData fails after EmptyClipboard.
pub fn set_text(owner: HWND, text: &str) -> Result<(), String> {
    set(owner, &[(u32::from(CF_UNICODETEXT.0), text_bytes(text))])
}

/// CF_UNICODETEXT: UTF-16 with a terminating null.
pub fn text_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// CF_HDROP: a DROPFILES header, then each path null-terminated and an extra null at the end.
pub fn set_files(owner: HWND, paths: &[&str]) -> Result<(), String> {
    set(owner, &[(u32::from(CF_HDROP.0), files_bytes(paths))])
}

pub fn files_bytes(paths: &[&str]) -> Vec<u8> {
    let header = DROPFILES {
        pFiles: size_of::<DROPFILES>() as u32,
        fWide: true.into(),
        ..Default::default()
    };
    // The header is plain integers, so its bytes are its value.
    let mut bytes = unsafe {
        std::slice::from_raw_parts((&raw const header).cast::<u8>(), size_of::<DROPFILES>())
    }
    .to_vec();
    for path in paths {
        bytes.extend(path.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    }
    bytes.extend([0, 0]);
    bytes
}

/// Replaces the clipboard with these (format, bytes) pairs.
pub fn set(owner: HWND, formats: &[(u32, Vec<u8>)]) -> Result<(), String> {
    // Another app may hold the clipboard open for a moment.
    let mut opened = unsafe { OpenClipboard(Some(owner)) };
    for _ in 0..10 {
        if opened.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        opened = unsafe { OpenClipboard(Some(owner)) };
    }
    opened.map_err(|e| format!("clipboard busy: {e}"))?;
    let written = unsafe { write(formats) };
    unsafe {
        let _ = CloseClipboard();
    }
    written.map_err(|e| e.to_string())
}

unsafe fn write(formats: &[(u32, Vec<u8>)]) -> windows::core::Result<()> {
    unsafe {
        EmptyClipboard()?;
        for (format, bytes) in formats {
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
            let dst = GlobalLock(mem).cast::<u8>();
            if dst.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err(windows::core::Error::from_thread());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
            let _ = GlobalUnlock(mem);
            // On success the clipboard owns the memory; on failure it is still ours to free.
            if let Err(e) = SetClipboardData(*format, Some(HANDLE(mem.0))) {
                let _ = GlobalFree(Some(mem));
                return Err(e);
            }
        }
        Ok(())
    }
}

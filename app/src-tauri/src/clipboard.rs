//! Plain-text clipboard writes.

use std::time::Duration;

use windows::Win32::{
    Foundation::{GlobalFree, HANDLE, HWND},
    System::{
        DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData},
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
        Ole::CF_UNICODETEXT,
    },
};

/// `owner` must be one of our windows: with no owner, SetClipboardData fails after EmptyClipboard.
pub fn set_text(owner: HWND, text: &str) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
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
    let written = unsafe { write(&wide) };
    unsafe {
        let _ = CloseClipboard();
    }
    written.map_err(|e| e.to_string())
}

unsafe fn write(wide: &[u16]) -> windows::core::Result<()> {
    unsafe {
        EmptyClipboard()?;
        let mem = GlobalAlloc(GMEM_MOVEABLE, size_of_val(wide))?;
        let dst = GlobalLock(mem).cast::<u16>();
        if dst.is_null() {
            let _ = GlobalFree(Some(mem));
            return Err(windows::core::Error::from_thread());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), dst, wide.len());
        let _ = GlobalUnlock(mem);
        // On success the clipboard owns the memory; on failure it is still ours to free.
        if let Err(e) = SetClipboardData(u32::from(CF_UNICODETEXT.0), Some(HANDLE(mem.0))) {
            let _ = GlobalFree(Some(mem));
            return Err(e);
        }
        Ok(())
    }
}

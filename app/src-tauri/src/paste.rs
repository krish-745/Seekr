//! Pasting into the window the panel was opened over: focus goes back to it, then Ctrl+V. Windows
//! doesn't let a normal app send keys to an elevated one, so for an admin window the text is left
//! on the clipboard and a toast says to press Ctrl+V.

use std::{sync::Once, time::Duration};

use tauri::AppHandle;
use windows::{
    Win32::{
        Foundation::{COLORREF, CloseHandle, HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateFontW, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
            DeleteObject, DrawTextW, EndPaint, FillRect, FrameRect, GetMonitorInfoW, HGDIOBJ,
            MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow, PAINTSTRUCT, SelectObject,
            SetBkMode, SetTextColor, TRANSPARENT,
        },
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION},
        },
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
                SendInput, VIRTUAL_KEY, VK_CONTROL, VK_V,
            },
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
                GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, KillTimer, MSG,
                PostQuitMessage, RegisterClassW, SW_SHOWNOACTIVATE, SetTimer, ShowWindow,
                WM_DESTROY, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
                WS_EX_TOPMOST, WS_POPUP,
            },
        },
    },
    core::{PCWSTR, w},
};

/// Hides the panel, giving focus back to the window it opened over, and pastes what is on the
/// clipboard there.
pub fn into_previous(app: &AppHandle) {
    crate::hide_panel(app, true);
    let target = unsafe { GetForegroundWindow() };
    if target.is_invalid() || is_elevated(target) {
        toast("Copied — press Ctrl+V");
        return;
    }
    // The target needs a moment to take focus fully before keys reach the right control.
    std::thread::sleep(Duration::from_millis(40));
    send_ctrl_v();
}

/// True for a window of an elevated process, or one whose process can't be inspected (which a
/// normal app can't send keys to either).
fn is_elevated(window: HWND) -> bool {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return true;
    };
    let mut token = HANDLE::default();
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) };
    let _ = unsafe { CloseHandle(process) };
    if opened.is_err() {
        return true;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0;
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    let _ = unsafe { CloseHandle(token) };
    read.is_err() || elevation.TokenIsElevated != 0
}

fn send_ctrl_v() {
    let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                ..Default::default()
            },
        },
    };
    let inputs = [
        key(VK_CONTROL, false),
        key(VK_V, false),
        key(VK_V, true),
        key(VK_CONTROL, true),
    ];
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        tracing::warn!("paste keys blocked; the text is on the clipboard");
    }
}

const TOAST_MS: u32 = 2200;

/// A small dark note near the bottom of the screen for a couple of seconds. It never takes focus,
/// so the user's next keystroke still goes to their window.
pub fn toast(text: &str) {
    let text: Vec<u16> = text.encode_utf16().collect();
    std::thread::spawn(move || unsafe { show_toast(&text) });
}

unsafe fn show_toast(text: &[u16]) {
    static CLASS: Once = Once::new();
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        CLASS.call_once(|| {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(toast_proc),
                hInstance: instance.into(),
                lpszClassName: w!("SeekrToast"),
                ..Default::default()
            });
        });
        // Placed on the monitor of the window the user is in, centred near the bottom.
        let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let area = info.rcWork;
        let Ok(window) = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("SeekrToast"),
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        ) else {
            return;
        };
        let scale = GetDpiForWindow(window).max(96) as i32;
        let (width, height) = (300 * scale / 96, 44 * scale / 96);
        let x = area.left + (area.right - area.left - width) / 2;
        let y = area.bottom - height - 48 * scale / 96;
        let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
            window,
            None,
            x,
            y,
            width,
            height,
            windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
        );
        TOAST_TEXT.with(|t| *t.borrow_mut() = text.to_vec());
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        SetTimer(Some(window), 1, TOAST_MS, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
}

thread_local! {
    static TOAST_TEXT: std::cell::RefCell<Vec<u16>> = const { std::cell::RefCell::new(Vec::new()) };
}

unsafe extern "system" fn toast_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(window, &mut paint);
                let mut rect = RECT::default();
                let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(window, &mut rect);
                let background = CreateSolidBrush(COLORREF(0x0030_2B2B));
                FillRect(dc, &rect, background);
                let _ = DeleteObject(HGDIOBJ(background.0));
                // A light edge keeps it apart from dark windows behind it.
                let edge = CreateSolidBrush(COLORREF(0x0060_5A5A));
                FrameRect(dc, &rect, edge);
                let _ = DeleteObject(HGDIOBJ(edge.0));
                let scale = GetDpiForWindow(window).max(96) as i32;
                let font = CreateFontW(
                    -15 * scale / 96,
                    0,
                    0,
                    0,
                    400,
                    0,
                    0,
                    0,
                    Default::default(),
                    Default::default(),
                    Default::default(),
                    Default::default(),
                    0,
                    w!("Segoe UI"),
                );
                let old = SelectObject(dc, HGDIOBJ(font.0));
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, COLORREF(0x00F0_F0F0));
                let mut text = TOAST_TEXT.with(|t| t.borrow().clone());
                DrawTextW(
                    dc,
                    &mut text,
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
                SelectObject(dc, old);
                let _ = DeleteObject(HGDIOBJ(font.0));
                let _ = EndPaint(window, &paint);
                LRESULT(0)
            }
            WM_TIMER => {
                let _ = KillTimer(Some(window), 1);
                let _ = DestroyWindow(window);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }
}

#[cfg(test)]
mod tests {
    /// With an elevated window open (Task Manager elevates itself), pass its HWND:
    /// `SEEKR_ELEVATED_HWND=<hwnd> cargo test -p seekr elevation -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn elevation_and_toast() {
        let hwnd: isize = std::env::var("SEEKR_ELEVATED_HWND")
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            super::is_elevated(super::HWND(hwnd as _)),
            "elevated window"
        );
        // Explorer's desktop window runs at the user's normal level.
        let shell = unsafe { windows::Win32::UI::WindowsAndMessaging::GetShellWindow() };
        assert!(!super::is_elevated(shell), "the desktop");
        super::toast("Copied — press Ctrl+V");
        std::thread::sleep(std::time::Duration::from_millis(300));
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, GetWindowRect, IsWindowVisible,
        };
        let toast = unsafe { FindWindowW(windows::core::w!("SeekrToast"), None) }.unwrap();
        assert!(unsafe { IsWindowVisible(toast) }.as_bool());
        let mut rect = super::RECT::default();
        unsafe { GetWindowRect(toast, &mut rect) }.unwrap();
        println!("toast at {rect:?}");
        std::thread::sleep(std::time::Duration::from_millis(2400));
        assert!(
            unsafe { FindWindowW(windows::core::w!("SeekrToast"), None) }.is_err(),
            "gone"
        );
    }
}

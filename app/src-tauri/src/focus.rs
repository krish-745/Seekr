//! Remembers the window that had focus before the panel opened, and gives focus back to it.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{GetForegroundWindow, IsWindow, SetForegroundWindow},
};

const CONFIRM_WITHIN: Duration = Duration::from_millis(250);

/// The raw HWND value; HWND itself isn't Send.
#[derive(Default)]
pub struct Focus(Mutex<Option<isize>>);

impl Focus {
    /// Call before showing. Our own panel is ignored so a re-show doesn't record itself.
    pub fn remember(&self, panel: HWND) {
        let foreground = unsafe { GetForegroundWindow() };
        if !foreground.is_invalid() && foreground != panel {
            *self.0.lock().unwrap() = Some(foreground.0 as isize);
        }
    }

    /// Must run while the panel is still foreground: once it hides, Windows no longer lets us
    /// choose who gets focus. Returns whether Windows confirmed the switch in time.
    pub fn restore(&self) -> bool {
        let Some(raw) = self.0.lock().unwrap().take() else {
            return false;
        };
        let target = HWND(raw as _);
        if !unsafe { IsWindow(Some(target)) }.as_bool() {
            return false;
        }
        let _ = unsafe { SetForegroundWindow(target) };
        let deadline = Instant::now() + CONFIRM_WITHIN;
        while Instant::now() < deadline {
            if unsafe { GetForegroundWindow() } == target {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
}

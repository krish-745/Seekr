//! Window switcher: `win ` lists the windows Alt+Tab would show, in the same order (most recently
//! used first), filtered by title or program name. Enter switches to one; the actions minimize or
//! close it.

use std::{path::Path, time::Duration};

use tauri::AppHandle;
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, WPARAM},
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx},
            Threading::{
                OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
            },
        },
        UI::WindowsAndMessaging::{
            EnumWindows, GA_ROOTOWNER, GWL_EXSTYLE, GetAncestor, GetLastActivePopup,
            GetWindowLongW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
            IsHungAppWindow, IsIconic, IsWindow, IsWindowVisible, PostMessageW, SW_RESTORE,
            SW_SHOWMINNOACTIVE, SetForegroundWindow, ShowWindow, WM_CLOSE, WS_EX_APPWINDOW,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        },
    },
    core::{BOOL, PWSTR},
};

use crate::{
    apps, paste,
    query::{Action, Item},
};

const SWITCH: Action = Action {
    id: "switch",
    label: "Switch to window",
};
const MINIMIZE: Action = Action {
    id: "winminimize",
    label: "Minimize",
};
const CLOSE: Action = Action {
    id: "winclose",
    label: "Close window",
};
/// The panel asks for a second Enter first.
const END_TASK: Action = Action {
    id: "endtask",
    label: "End task",
};

/// `win` or `win <words>`.
/// `<prefix> words`: `word` is the window switcher's prefix from config.toml.
pub fn mode<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.trim_start().strip_prefix(word)?;
    rest.starts_with(' ').then(|| rest.trim())
}

struct Window {
    hwnd: isize,
    title: String,
    program: String,
    /// Windows' own "Not responding" test: no messages handled for five seconds.
    hung: bool,
}

/// The switchable windows, other than Seekr's own, whose title or program has every word.
pub fn items(filter: &str, panel: Option<HWND>) -> Vec<Item> {
    // Program icons come from the shell, which needs COM on this thread.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    windows()
        .into_iter()
        .filter(|w| Some(HWND(w.hwnd as _)) != panel)
        .filter(|w| matches(&words, &w.title, &w.program))
        .map(|w| {
            let name = Path::new(&w.program)
                .file_stem()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            let name = if name.eq_ignore_ascii_case("ApplicationFrameHost") {
                "Store app".to_string()
            } else {
                name
            };
            let mut actions = vec![SWITCH, MINIMIZE, CLOSE];
            let mut subtitle = format!("Window · {name}");
            if w.hung {
                subtitle = format!("Not responding · {subtitle}");
                actions.push(END_TASK);
            }
            Item {
                id: format!("win:{}", w.hwnd),
                title: w.title.clone(),
                subtitle,
                preview: format!("{}\n{}", w.title, w.program),
                icon: apps::file_icon(&w.program),
                line: None,
                actions,
            }
        })
        .collect()
}

fn matches(words: &[String], title: &str, program: &str) -> bool {
    let name = Path::new(program)
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().to_lowercase());
    let title = title.to_lowercase();
    words
        .iter()
        .all(|w| title.contains(w.as_str()) || name.contains(w.as_str()))
}

/// Top-level windows in Z order, kept the way Alt+Tab keeps them.
fn windows() -> Vec<Window> {
    let mut found: Vec<Window> = Vec::new();
    unsafe extern "system" fn each(hwnd: HWND, found: LPARAM) -> BOOL {
        let found = unsafe { &mut *(found.0 as *mut Vec<Window>) };
        if let Some(window) = unsafe { switchable(hwnd) } {
            found.push(window);
        }
        true.into()
    }
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&raw mut found as isize)) };
    found
}

/// Raymond Chen's Alt+Tab rule: the window is what its root owner's chain of last active popups
/// leads to; then tool and no-activate windows, cloaked ones (other desktops, suspended Store
/// app frames) and untitled ones are left out.
unsafe fn switchable(hwnd: HWND) -> Option<Window> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let mut walk = HWND::default();
        let mut next = GetAncestor(hwnd, GA_ROOTOWNER);
        while next != walk {
            walk = next;
            next = GetLastActivePopup(walk);
            if IsWindowVisible(next).as_bool() {
                break;
            }
        }
        if walk != hwnd {
            return None;
        }
        let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let app_window = ex & WS_EX_APPWINDOW.0 != 0;
        if !app_window && ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return None;
        }
        let mut cloaked = 0u32;
        let read = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            size_of::<u32>() as u32,
        );
        if read.is_ok() && cloaked != 0 {
            return None;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return None;
        }
        let mut title = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, &mut title);
        let title = String::from_utf16_lossy(&title[..copied as usize]);
        Some(Window {
            hwnd: hwnd.0 as isize,
            title,
            program: program(hwnd).unwrap_or_default(),
            hung: IsHungAppWindow(hwnd).as_bool(),
        })
    }
}

fn pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// Ends a program outright, as Task Manager's End task does. False for an admin one.
fn terminate(pid: u32) -> bool {
    let Ok(process) = (unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }) else {
        return false;
    };
    let ended = unsafe { TerminateProcess(process, 1) }.is_ok();
    let _ = unsafe { CloseHandle(process) };
    ended
}

/// Ends every program with a window that isn't responding: (ended, refused).
pub fn end_frozen() -> (usize, usize) {
    let mut pids: Vec<u32> = windows()
        .iter()
        .filter(|w| w.hung)
        .map(|w| pid(HWND(w.hwnd as _)))
        .collect();
    pids.sort_unstable();
    pids.dedup();
    let ended = pids.iter().filter(|&&p| terminate(p)).count();
    (ended, pids.len() - ended)
}

/// The full path of the program that owns the window.
fn program(hwnd: HWND) -> Option<String> {
    let pid = pid(hwnd);
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()? };
    let mut path = vec![0u16; 1024];
    let mut len = path.len() as u32;
    let read = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut len,
        )
    };
    let _ = unsafe { CloseHandle(process) };
    read.ok()?;
    Some(String::from_utf16_lossy(&path[..len as usize]))
}

pub fn run(app: &AppHandle, id: &str, action: &str) -> Result<(), String> {
    let raw: isize = id.parse().map_err(|_| "bad window id")?;
    let hwnd = HWND(raw as _);
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err("that window has closed".into());
    }
    match action {
        "winminimize" => {
            // SW_MINIMIZE would activate the next window and take focus from the panel.
            let _ = unsafe { ShowWindow(hwnd, SW_SHOWMINNOACTIVE) };
        }
        "endtask" => {
            if !terminate(pid(hwnd)) {
                paste::toast("Can't end an admin app from Seekr");
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        "winclose" => {
            // Windows drops messages to an admin window from a normal app.
            if unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_err() {
                paste::toast("Can't close an admin window from Seekr");
            }
            // Gives it a moment to go before the list is shown again.
            std::thread::sleep(Duration::from_millis(150));
        }
        _ => {
            // Only the foreground app may hand over focus, so this runs before the panel hides.
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
            crate::hide_panel(app, false);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_and_matching() {
        assert_eq!(mode("win ", "win"), Some(""));
        assert_eq!(mode("win code seekr", "win"), Some("code seekr"));
        assert_eq!(mode("windows", "win"), None);
        assert_eq!(mode("w code", "w"), Some("code"));
        let words = |t: &str| t.split_whitespace().map(String::from).collect::<Vec<_>>();
        let program = r"C:\Program Files\Microsoft VS Code\Code.exe";
        assert!(matches(
            &words("seekr code"),
            "calc.rs - Seekr - Visual Studio Code",
            program
        ));
        assert!(matches(&words("code.exe"), "anything", program));
        assert!(!matches(&words("chrome"), "calc.rs - Seekr", program));
    }

    /// `cargo test -p seekr live_windows -- --ignored --nocapture` lists what Alt+Tab would.
    #[test]
    #[ignore]
    fn live_windows() {
        for w in windows() {
            println!("{:>10} {} | {}", w.hwnd, w.title, w.program);
        }
    }
}

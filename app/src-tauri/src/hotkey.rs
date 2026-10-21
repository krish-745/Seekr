//! Low-level keyboard hook, used only when RegisterHotKey fails because another app owns the combo.
//! Observed on Windows 11 25H2: the hook isn't called while our own panel is foreground, so in this
//! mode the hotkey only opens the panel; Esc and focus loss still hide it.

use std::sync::{
    OnceLock,
    atomic::{AtomicBool, Ordering},
};

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};
use windows::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT,
            KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN,
            VK_SHIFT,
        },
        WindowsAndMessaging::{
            CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, SetWindowsHookExW,
            WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
        },
    },
};

struct Hook {
    vk: u32,
    mods: Modifiers,
    app: AppHandle,
}

static HOOK: OnceLock<Hook> = OnceLock::new();
/// Set while the hotkey's key is held, so auto-repeat doesn't toggle the panel again.
static HELD: AtomicBool = AtomicBool::new(false);

/// Installs the hook on its own thread. On the UI thread, any stall (WebView2 work while
/// the panel takes focus) exceeds LowLevelHooksTimeout and Windows silently removes it.
pub fn install(shortcut: Shortcut, app: AppHandle) -> Result<(), String> {
    let vk = vk_code(shortcut.key)
        .ok_or_else(|| format!("{} is not supported by the keyboard hook", shortcut.key))?;
    let mods = shortcut.mods;
    HOOK.set(Hook { vk, mods, app })
        .map_err(|_| "keyboard hook already installed".to_string())?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("hotkey-hook".into())
        .spawn(move || {
            // The hook lives for the whole process, so its handle is never unhooked.
            let hooked = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) };
            let ok = hooked.is_ok();
            let _ = tx.send(hooked.map(|_| ()).map_err(|e| e.to_string()));
            // The hook is called from this thread's message loop.
            let mut msg = MSG::default();
            while ok && unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {}
        })
        .map_err(|e| e.to_string())?;
    rx.recv().map_err(|e| e.to_string())?
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32
        && let Some(hook) = HOOK.get()
        && unsafe { (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode } == hook.vk
    {
        match wparam.0 as u32 {
            WM_KEYDOWN | WM_SYSKEYDOWN if held_mods() == hook.mods => {
                if !HELD.swap(true, Ordering::Relaxed) {
                    let (app, mods) = (hook.app.clone(), hook.mods);
                    // Keep the callback instant: SendInput runs every hook on the system and
                    // toggling waits on the UI thread, so either could trip the timeout.
                    tauri::async_runtime::spawn_blocking(move || {
                        mask_modifier_release(mods);
                        crate::toggle(&app);
                    });
                }
                return LRESULT(1);
            }
            WM_KEYUP | WM_SYSKEYUP if HELD.swap(false, Ordering::Relaxed) => return LRESULT(1),
            _ => {}
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn held_mods() -> Modifiers {
    let down = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(vk.0 as i32) } < 0;
    let mut mods = Modifiers::empty();
    mods.set(Modifiers::ALT, down(VK_MENU));
    mods.set(Modifiers::CONTROL, down(VK_CONTROL));
    mods.set(Modifiers::SHIFT, down(VK_SHIFT));
    mods.set(Modifiers::SUPER, down(VK_LWIN) || down(VK_RWIN));
    mods
}

/// A lone Alt or Win release after a swallowed key would open the menu bar or Start,
/// so tap the unassigned key 0xE8 in between (the same trick AutoHotkey uses).
fn mask_modifier_release(mods: Modifiers) {
    if !mods.intersects(Modifiers::ALT | Modifiers::SUPER) {
        return;
    }
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0xE8),
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
}

/// shortcut: only Space, A-Z, 0-9 and F1-F24; extend if config ever needs other keys in the fallback.
fn vk_code(key: Code) -> Option<u32> {
    let name = key.to_string();
    match name.as_bytes() {
        b"Space" => Some(0x20),
        // VK codes for letters and digits equal their ASCII codes.
        [b'K', b'e', b'y', c] | [b'D', b'i', b'g', b'i', b't', c] => Some(u32::from(*c)),
        _ => name
            .strip_prefix('F')
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| (1..=24).contains(n))
            .map(|n| 0x6F + n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_supported_keys() {
        assert_eq!(vk_code(Code::Space), Some(0x20));
        assert_eq!(vk_code(Code::KeyA), Some(0x41));
        assert_eq!(vk_code(Code::Digit7), Some(0x37));
        assert_eq!(vk_code(Code::F1), Some(0x70));
        assert_eq!(vk_code(Code::F24), Some(0x87));
        assert_eq!(vk_code(Code::ArrowUp), None);
        assert_eq!(vk_code(Code::Fn), None);
    }
}

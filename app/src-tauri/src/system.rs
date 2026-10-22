//! System controls: power (lock, sleep, restart, shut down, sign out), volume and mute,
//! brightness (the built-in panel through WMI, external monitors through DDC/CI), Wi-Fi and
//! Bluetooth, dark mode, the Recycle Bin, frozen apps, and Windows Settings pages. Each one
//! reports what it did in a small toast, since the panel has gone by then.

use std::{
    os::windows::process::CommandExt,
    sync::{OnceLock, mpsc},
    time::Duration,
};

use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use windows::{
    Devices::Radios::{Radio, RadioKind, RadioState},
    Win32::{
        Devices::Display::{
            DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
            GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, SetMonitorBrightness,
        },
        Foundation::{LPARAM, RECT, WPARAM},
        Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
        Media::Audio::{
            Endpoints::IAudioEndpointVolume, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole,
            eRender,
        },
        System::{
            Com::{
                CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance,
                CoInitializeEx, CoSetProxyBlanket, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
            },
            Power::SetSuspendState,
            Registry::{
                HKEY_CURRENT_USER, REG_DWORD, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW,
            },
            Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE},
            Shutdown::{EWX_LOGOFF, ExitWindowsEx, LockWorkStation, SHTDN_REASON_NONE},
            Variant::VARIANT,
            Wmi::{
                IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY,
                WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE, WbemLocator,
            },
        },
        UI::{
            Shell::{SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW},
            WindowsAndMessaging::{
                HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
            },
        },
    },
    core::{BOOL, BSTR, PCWSTR, w},
};

use crate::{
    paste::toast,
    query::{Action, Item},
    switcher,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const STEP: i32 = 10;

const RUN: Action = Action {
    id: "run",
    label: "Run",
};
const OPEN: Action = Action {
    id: "open",
    label: "Open",
};
/// The panel asks for a second Enter before these.
const RESTART: Action = Action {
    id: "restart",
    label: "Restart",
};
const SHUT_DOWN: Action = Action {
    id: "shutdown",
    label: "Shut down",
};
const SIGN_OUT: Action = Action {
    id: "signout",
    label: "Sign out",
};
const EMPTY_BIN: Action = Action {
    id: "emptybin",
    label: "Empty",
};
const KILL_FROZEN: Action = Action {
    id: "killfrozen",
    label: "End them",
};

/// (id, title, what it does, Enter's action)
const COMMANDS: &[(&str, &str, &str, Action)] = &[
    ("lock", "Lock", "Lock the PC", RUN),
    ("sleep", "Sleep", "Put the PC to sleep", RUN),
    ("restart", "Restart", "Restart Windows now", RESTART),
    ("shutdown", "Shut down", "Turn the PC off now", SHUT_DOWN),
    ("signout", "Sign out", "Sign out of Windows", SIGN_OUT),
    (
        "mute",
        "Mute or unmute",
        "Turn the sound off or back on",
        RUN,
    ),
    (
        "volup",
        "Volume up",
        "Raise the volume by 10%; type volume 40 to set it",
        RUN,
    ),
    ("voldown", "Volume down", "Lower the volume by 10%", RUN),
    (
        "brightup",
        "Brightness up",
        "Brighter by 10%; type brightness 70 to set it",
        RUN,
    ),
    ("brightdown", "Brightness down", "Dimmer by 10%", RUN),
    (
        "wifi",
        "Turn Wi-Fi on or off",
        "Switch the Wi-Fi radio",
        RUN,
    ),
    (
        "bluetooth",
        "Turn Bluetooth on or off",
        "Switch the Bluetooth radio",
        RUN,
    ),
    (
        "dark",
        "Switch dark and light mode",
        "Change Windows and apps between dark and light",
        RUN,
    ),
    (
        "emptybin",
        "Empty Recycle Bin",
        "Delete everything in the Recycle Bin for good",
        EMPTY_BIN,
    ),
    (
        "frozen",
        "Close frozen apps",
        "End every app whose window isn't responding",
        KILL_FROZEN,
    ),
];

/// Windows Settings pages: (name, `ms-settings:` page).
const SETTINGS: &[(&str, &str)] = &[
    ("Display", "display"),
    ("Night light", "nightlight"),
    ("Graphics", "display-advancedgraphics"),
    ("Sound", "sound"),
    ("Sound devices", "sound-devices"),
    ("Volume mixer", "apps-volume"),
    ("Notifications", "notifications"),
    ("Focus", "quiethours"),
    ("Power & battery", "powersleep"),
    ("Battery saver", "batterysaver"),
    ("Storage", "storagesense"),
    ("Nearby sharing", "crossdevice"),
    ("Multitasking", "multitasking"),
    ("Activation", "activation"),
    ("Troubleshoot", "troubleshoot"),
    ("Recovery", "recovery"),
    ("Projecting to this PC", "project"),
    ("Clipboard", "clipboard"),
    ("Remote Desktop", "remotedesktop"),
    ("About", "about"),
    ("Bluetooth & devices", "bluetooth"),
    ("Devices", "connecteddevices"),
    ("Printers & scanners", "printers"),
    ("Phone Link", "mobile-devices"),
    ("Cameras", "camera"),
    ("Mouse", "mousetouchpad"),
    ("Touchpad", "devices-touchpad"),
    ("Pen & Windows Ink", "pen"),
    ("AutoPlay", "autoplay"),
    ("USB", "usb"),
    ("Network & internet", "network"),
    ("Wi-Fi", "network-wifi"),
    ("Known Wi-Fi networks", "network-wifisettings"),
    ("Ethernet", "network-ethernet"),
    ("VPN", "network-vpn"),
    ("Mobile hotspot", "network-mobilehotspot"),
    ("Airplane mode", "network-airplanemode"),
    ("Proxy", "network-proxy"),
    ("Dial-up", "network-dialup"),
    ("Advanced network settings", "network-advancedsettings"),
    ("Data usage", "datausage"),
    ("Personalization", "personalization"),
    ("Background", "personalization-background"),
    ("Colors", "colors"),
    ("Themes", "themes"),
    ("Lock screen", "lockscreen"),
    ("Text input", "personalization-textinput"),
    ("Start", "personalization-start"),
    ("Taskbar", "taskbar"),
    ("Fonts", "fonts"),
    ("Device usage", "deviceusage"),
    ("Installed apps", "appsfeatures"),
    ("Advanced app settings", "appsfeatures-app"),
    ("Default apps", "defaultapps"),
    ("Offline maps", "maps"),
    ("Optional features", "optionalfeatures"),
    ("Apps for websites", "appsforwebsites"),
    ("Video playback", "videoplayback"),
    ("Startup apps", "startupapps"),
    ("Your info", "yourinfo"),
    ("Email & accounts", "emailandaccounts"),
    ("Sign-in options", "signinoptions"),
    ("Family", "family-group"),
    ("Other users", "otherusers"),
    ("Windows backup", "backup"),
    ("Access work or school", "workplace"),
    ("Date & time", "dateandtime"),
    ("Language & region", "regionlanguage"),
    ("Typing", "typing"),
    ("Speech", "speech"),
    ("Game Bar", "gaming-gamebar"),
    ("Captures", "gaming-gamedvr"),
    ("Game Mode", "gaming-gamemode"),
    ("Accessibility", "easeofaccess"),
    ("Text size", "easeofaccess-display"),
    ("Visual effects", "easeofaccess-visualeffects"),
    ("Mouse pointer and touch", "easeofaccess-mousepointer"),
    ("Text cursor", "easeofaccess-cursor"),
    ("Magnifier", "easeofaccess-magnifier"),
    ("Color filters", "easeofaccess-colorfilter"),
    ("Contrast themes", "easeofaccess-highcontrast"),
    ("Narrator", "easeofaccess-narrator"),
    ("Accessibility audio", "easeofaccess-audio"),
    ("Captions", "easeofaccess-closedcaptioning"),
    ("Voice access", "easeofaccess-speechrecognition"),
    ("Accessibility keyboard", "easeofaccess-keyboard"),
    ("Accessibility mouse", "easeofaccess-mouse"),
    ("Eye control", "easeofaccess-eyecontrol"),
    ("Privacy & security", "privacy"),
    ("Windows Security", "windowsdefender"),
    ("Find my device", "findmydevice"),
    ("For developers", "developers"),
    ("Location privacy", "privacy-location"),
    ("Camera privacy", "privacy-webcam"),
    ("Microphone privacy", "privacy-microphone"),
    ("Notifications privacy", "privacy-notifications"),
    ("General privacy", "privacy-general"),
    ("Diagnostics & feedback", "privacy-feedback"),
    ("Activity history", "privacy-activityhistory"),
    ("Search permissions", "search-permissions"),
    ("Searching Windows", "cortana-windowssearch"),
    ("Windows Update", "windowsupdate"),
    ("Update history", "windowsupdate-history"),
    ("Advanced Windows Update options", "windowsupdate-options"),
    ("Windows Insider Program", "windowsinsider"),
    ("Delivery optimization", "delivery-optimization"),
];

pub fn all() -> Vec<Item> {
    let commands = COMMANDS.iter().map(|&(id, title, what, ref action)| Item {
        id: format!("sys:{id}"),
        title: title.into(),
        subtitle: what.into(),
        preview: what.into(),
        icon: None,
        line: None,
        actions: vec![action.clone()],
    });
    let settings = SETTINGS.iter().map(|&(name, page)| Item {
        id: format!("setting:{page}"),
        title: format!("{name} settings"),
        subtitle: format!("Windows Settings · ms-settings:{page}"),
        preview: format!("Opens {name} in Windows Settings."),
        icon: None,
        line: None,
        actions: vec![OPEN],
    });
    commands.chain(settings).collect()
}

/// "volume 40" and "brightness 70" (or vol, bright) set a level directly.
pub fn direct(text: &str) -> Vec<Item> {
    let lower = text.trim().to_lowercase();
    let Some((word, value)) = lower.split_once(' ') else {
        return Vec::new();
    };
    let Ok(level) = value.trim().trim_end_matches('%').parse::<u32>() else {
        return Vec::new();
    };
    if level > 100 {
        return Vec::new();
    }
    let (id, what) = match word {
        "volume" | "vol" => ("volume", "Volume"),
        "brightness" | "bright" => ("brightness", "Brightness"),
        _ => return Vec::new(),
    };
    vec![Item {
        id: format!("sys:{id}:{level}"),
        title: format!("Set {} to {level}%", what.to_lowercase()),
        subtitle: what.into(),
        preview: String::new(),
        icon: None,
        line: None,
        actions: vec![RUN],
    }]
}

pub fn open_setting(app: &AppHandle, page: &str) -> Result<(), String> {
    if !SETTINGS.iter().any(|&(_, p)| p == page) {
        return Err("unknown settings page".into());
    }
    crate::hide_panel(app, false);
    app.opener()
        .open_url(format!("ms-settings:{page}"), None::<&str>)
        .map_err(|e| e.to_string())
}

pub fn run(app: &AppHandle, id: &str, action: &str) -> Result<(), String> {
    tracing::info!("system {id} {action}");
    // The panel goes first, so focus and the toast land on the user's window.
    crate::hide_panel(app, true);
    let id = id.to_string();
    // COM and WinRT calls get a thread of their own, set up for them.
    std::thread::spawn(move || {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if let Err(e) = perform(&id) {
            tracing::warn!("{id} failed: {e}");
            toast(&format!("Couldn't do that: {e}"));
        }
    });
    Ok(())
}

fn perform(id: &str) -> Result<(), String> {
    let win = |e: windows::core::Error| e.message();
    match id.split_once(':') {
        Some(("volume", level)) => {
            let level: f32 = level.parse().map_err(|_| "bad level")?;
            let volume = set_volume(|_| level / 100.0).map_err(win)?;
            toast(&format!("Volume {volume}%"));
            return Ok(());
        }
        Some(("brightness", level)) => {
            brightness(Change::To(level.parse().map_err(|_| "bad level")?));
            return Ok(());
        }
        _ => {}
    }
    match id {
        "lock" => unsafe { LockWorkStation() }.map_err(win)?,
        "sleep" => {
            // Lets the panel finish hiding first.
            std::thread::sleep(Duration::from_millis(300));
            if !unsafe { SetSuspendState(false, false, false) } {
                return Err("Windows refused to sleep".into());
            }
        }
        "restart" | "shutdown" => {
            let flag = if id == "restart" { "/r" } else { "/s" };
            std::process::Command::new("shutdown.exe")
                .args([flag, "/t", "0"])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map_err(|e| e.to_string())?;
        }
        "signout" => unsafe { ExitWindowsEx(EWX_LOGOFF, SHTDN_REASON_NONE) }.map_err(win)?,
        "mute" => {
            let muted = toggle_mute().map_err(win)?;
            toast(if muted { "Sound off" } else { "Sound on" });
        }
        "volup" | "voldown" => {
            let delta = if id == "volup" { 0.1 } else { -0.1 };
            let volume = set_volume(|now| now + delta).map_err(win)?;
            toast(&format!("Volume {volume}%"));
        }
        "brightup" => brightness(Change::By(STEP)),
        "brightdown" => brightness(Change::By(-STEP)),
        "wifi" | "bluetooth" => {
            let (kind, name) = if id == "wifi" {
                (RadioKind::WiFi, "Wi-Fi")
            } else {
                (RadioKind::Bluetooth, "Bluetooth")
            };
            match toggle_radio(kind).map_err(win)? {
                Some(on) => toast(&format!("{name} {}", if on { "on" } else { "off" })),
                None => toast(&format!("No {name} radio")),
            }
        }
        "dark" => {
            let dark = toggle_dark_mode()?;
            toast(if dark { "Dark mode" } else { "Light mode" });
        }
        "emptybin" => {
            let flags = SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND;
            match unsafe { SHEmptyRecycleBinW(None, PCWSTR::null(), flags) } {
                Ok(()) => toast("Recycle Bin emptied"),
                // An empty bin reports a failure too.
                Err(_) => toast("The Recycle Bin is empty"),
            }
        }
        "frozen" => {
            let (ended, failed) = switcher::end_frozen();
            toast(&match (ended, failed) {
                (0, 0) => "No app is frozen".to_string(),
                (n, 0) => format!("Ended {n} frozen app{}", if n == 1 { "" } else { "s" }),
                (n, f) => format!("Ended {n}; {f} need admin rights"),
            });
        }
        _ => return Err(format!("unknown command {id}")),
    }
    Ok(())
}

// ---- sound ----

fn endpoint() -> windows::core::Result<IAudioEndpointVolume> {
    unsafe {
        let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let speakers = devices.GetDefaultAudioEndpoint(eRender, eConsole)?;
        speakers.Activate(CLSCTX_ALL, None)
    }
}

/// Sets the volume to `to(current)` (0 to 1) and returns the new level in percent.
fn set_volume(to: impl Fn(f32) -> f32) -> windows::core::Result<u32> {
    let volume = endpoint()?;
    unsafe {
        let now = volume.GetMasterVolumeLevelScalar()?;
        let level = to(now).clamp(0.0, 1.0);
        volume.SetMasterVolumeLevelScalar(level, std::ptr::null())?;
        // Changing the level is a request to hear it.
        volume.SetMute(false, std::ptr::null())?;
        Ok((level * 100.0).round() as u32)
    }
}

fn toggle_mute() -> windows::core::Result<bool> {
    let volume = endpoint()?;
    unsafe {
        let muted = !volume.GetMute()?.as_bool();
        volume.SetMute(muted, std::ptr::null())?;
        Ok(muted)
    }
}

// ---- brightness ----

#[derive(Clone, Copy, Debug, PartialEq)]
enum Change {
    To(u32),
    By(i32),
}

/// Queues a brightness change for the worker, which applies only the latest of a burst: setting
/// a monitor over DDC/CI takes tenths of a second each time.
fn brightness(change: Change) {
    static WORKER: OnceLock<mpsc::Sender<Change>> = OnceLock::new();
    let sender = WORKER.get_or_init(|| {
        let (sender, changes) = mpsc::channel::<Change>();
        std::thread::spawn(move || {
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            while let Ok(first) = changes.recv() {
                std::thread::sleep(Duration::from_millis(80));
                let mut burst = vec![first];
                burst.extend(changes.try_iter());
                let current = panel_brightness()
                    .or_else(external_brightness)
                    .unwrap_or(50);
                let target = combine(current, &burst);
                let panel = set_panel_brightness(target);
                let externals = set_external_brightness(target);
                if panel || externals > 0 {
                    toast(&format!("Brightness {target}%"));
                } else {
                    toast("No display here lets Seekr set its brightness");
                }
            }
        });
        sender
    });
    let _ = sender.send(change);
}

/// The level a burst of changes ends at, starting from `current`.
fn combine(current: u32, burst: &[Change]) -> u32 {
    burst.iter().fold(current, |level, change| match *change {
        Change::To(to) => to.min(100),
        Change::By(by) => (level as i32 + by).clamp(0, 100) as u32,
    })
}

fn wmi() -> windows::core::Result<IWbemServices> {
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let services = locator.ConnectServer(
            &BSTR::from(r"ROOT\WMI"),
            &BSTR::new(),
            &BSTR::new(),
            &BSTR::new(),
            0,
            &BSTR::new(),
            None,
        )?;
        CoSetProxyBlanket(
            &services,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            None,
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )?;
        Ok(services)
    }
}

/// The first object a WQL query returns.
fn first(services: &IWbemServices, query: &str) -> windows::core::Result<IWbemClassObject> {
    unsafe {
        let found = services.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(query),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )?;
        let mut objects = [None];
        let mut returned = 0;
        found
            .Next(WBEM_INFINITE, &mut objects, &mut returned)
            .ok()?;
        objects[0].take().ok_or_else(windows::core::Error::empty)
    }
}

/// The built-in panel's brightness, which laptops expose through WMI.
fn panel_brightness() -> Option<u32> {
    let services = wmi().ok()?;
    let monitor = first(
        &services,
        "SELECT CurrentBrightness FROM WmiMonitorBrightness",
    )
    .ok()?;
    let mut value = VARIANT::default();
    unsafe { monitor.Get(w!("CurrentBrightness"), 0, &mut value, None, None) }.ok()?;
    i32::try_from(&value).ok().map(|v| v as u32)
}

fn set_panel_brightness(level: u32) -> bool {
    let set = || -> windows::core::Result<()> {
        let services = wmi()?;
        let methods = first(&services, "SELECT * FROM WmiMonitorBrightnessMethods")?;
        let mut path = VARIANT::default();
        unsafe { methods.Get(w!("__PATH"), 0, &mut path, None, None)? };
        let path = BSTR::try_from(&path)?;
        let mut class = None;
        unsafe {
            services.GetObject(
                &BSTR::from("WmiMonitorBrightnessMethods"),
                Default::default(),
                None,
                Some(&mut class),
                None,
            )?;
        }
        let class = class.ok_or_else(windows::core::Error::empty)?;
        let mut signature = None;
        unsafe {
            class.GetMethod(
                w!("WmiSetBrightness"),
                0,
                &mut signature,
                std::ptr::null_mut(),
            )?
        };
        let arguments = unsafe {
            signature
                .ok_or_else(windows::core::Error::empty)?
                .SpawnInstance(0)?
        };
        unsafe {
            arguments.Put(w!("Timeout"), 0, &VARIANT::from(0i32), 0)?;
            arguments.Put(w!("Brightness"), 0, &VARIANT::from(level as i32), 0)?;
            services.ExecMethod(
                &path,
                &BSTR::from("WmiSetBrightness"),
                Default::default(),
                None,
                &arguments,
                None,
                None,
            )?;
        }
        Ok(())
    };
    set().is_ok()
}

/// Every external monitor's physical handles, which DDC/CI brightness goes through.
fn physical_monitors() -> Vec<PHYSICAL_MONITOR> {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, found: LPARAM) -> BOOL {
        let found = unsafe { &mut *(found.0 as *mut Vec<HMONITOR>) };
        found.push(monitor);
        true.into()
    }
    let mut monitors: Vec<HMONITOR> = Vec::new();
    let _ =
        unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&raw mut monitors as isize)) };
    let mut physical = Vec::new();
    for monitor in monitors {
        let mut count = 0;
        if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count) }.is_err() {
            continue;
        }
        let mut handles = vec![PHYSICAL_MONITOR::default(); count as usize];
        if unsafe { GetPhysicalMonitorsFromHMONITOR(monitor, &mut handles) }.is_ok() {
            physical.extend(handles);
        }
    }
    physical
}

fn external_brightness() -> Option<u32> {
    let monitors = physical_monitors();
    let level = monitors.iter().find_map(|m| {
        let (mut min, mut now, mut max) = (0, 0, 0);
        let read =
            unsafe { GetMonitorBrightness(m.hPhysicalMonitor, &mut min, &mut now, &mut max) };
        (read != 0 && max > min).then(|| (now - min) * 100 / (max - min))
    });
    let _ = unsafe { DestroyPhysicalMonitors(&monitors) };
    level
}

/// Returns how many external monitors took the new level.
fn set_external_brightness(level: u32) -> usize {
    let monitors = physical_monitors();
    let mut set = 0;
    for m in &monitors {
        let (mut min, mut now, mut max) = (0, 0, 0);
        let read =
            unsafe { GetMonitorBrightness(m.hPhysicalMonitor, &mut min, &mut now, &mut max) };
        if read != 0 && max > min {
            let value = min + (max - min) * level / 100;
            if unsafe { SetMonitorBrightness(m.hPhysicalMonitor, value) } != 0 {
                set += 1;
            }
        }
    }
    let _ = unsafe { DestroyPhysicalMonitors(&monitors) };
    set
}

// ---- radios, dark mode ----

/// Flips the first radio of `kind`; Some(on) for its new state, None without one.
fn toggle_radio(kind: RadioKind) -> windows::core::Result<Option<bool>> {
    Radio::RequestAccessAsync()?.join()?;
    let radios = Radio::GetRadiosAsync()?.join()?;
    for i in 0..radios.Size()? {
        let radio = radios.GetAt(i)?;
        if radio.Kind()? == kind {
            let on = radio.State()? == RadioState::On;
            let next = if on { RadioState::Off } else { RadioState::On };
            radio.SetStateAsync(next)?.join()?;
            return Ok(Some(!on));
        }
    }
    Ok(None)
}

const PERSONALIZE: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");

/// Flips apps and Windows together between dark and light; returns whether it is now dark.
fn toggle_dark_mode() -> Result<bool, String> {
    let mut light = 1u32;
    let mut size = 4u32;
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PERSONALIZE,
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut light).cast()),
            Some(&mut size),
        )
    };
    if read.is_err() {
        light = 1;
    }
    let next: u32 = u32::from(light == 0);
    for name in [w!("AppsUseLightTheme"), w!("SystemUsesLightTheme")] {
        let written = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                PERSONALIZE,
                name,
                REG_DWORD.0,
                Some((&raw const next).cast()),
                4,
            )
        };
        if written.is_err() {
            return Err("couldn't change the theme setting".into());
        }
    }
    // Running apps (the taskbar, Explorer, this panel) pick the change up from this broadcast.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            200,
            None,
        );
    }
    Ok(next == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_levels_and_bursts() {
        assert_eq!(direct("volume 40")[0].id, "sys:volume:40");
        assert_eq!(direct("Bright 70%")[0].id, "sys:brightness:70");
        assert!(direct("volume 140").is_empty());
        assert!(direct("volume up").is_empty());
        assert!(direct("vol").is_empty());
        assert_eq!(combine(50, &[Change::By(10), Change::By(10)]), 70);
        assert_eq!(combine(95, &[Change::By(10)]), 100);
        assert_eq!(
            combine(50, &[Change::By(-10), Change::To(30), Change::By(5)]),
            35
        );
    }

    #[test]
    fn every_command_and_page_is_distinct() {
        let items = all();
        let mut ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), items.len());
        assert!(SETTINGS.len() >= 100);
    }

    /// `cargo test -p seekr live_levels -- --ignored --nocapture` reads without changing anything.
    #[test]
    #[ignore]
    fn live_levels() {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let volume = endpoint().unwrap();
        let level = unsafe { volume.GetMasterVolumeLevelScalar() }.unwrap();
        println!("volume {:.0}%", level * 100.0);
        println!("panel brightness {:?}", panel_brightness());
        println!("external brightness {:?}", external_brightness());
        let radios = Radio::GetRadiosAsync().unwrap().join().unwrap();
        for i in 0..radios.Size().unwrap() {
            let r = radios.GetAt(i).unwrap();
            println!("radio {:?} {:?}", r.Kind().unwrap(), r.State().unwrap());
        }
    }
}

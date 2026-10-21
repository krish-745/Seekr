//! Shell actions on a file or folder result: Open with, Terminal here, Properties and Move to
//! Recycle Bin. Copy file lives in `clipboard`; Open and Show in folder use the opener plugin.

use std::{os::windows::process::CommandExt, path::Path, process::Command};

use windows::{
    Win32::{
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::Shell::{
            FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, OAIF_ALLOW_REGISTRATION,
            OAIF_EXEC, OAIF_REGISTER_EXT, OPENASINFO, SHFILEOPSTRUCTW, SHFileOperationW,
            SHOP_FILEPATH, SHObjectProperties, SHOpenWithDialog,
        },
    },
    core::{HSTRING, PCWSTR},
};

const CREATE_NEW_CONSOLE: u32 = 0x10;

/// The "How do you want to open this file?" dialog. It is modal, so it gets its own thread.
pub fn open_with(path: &str) -> Result<(), String> {
    let path = HSTRING::from(path);
    std::thread::Builder::new()
        .name("open-with".into())
        .spawn(move || {
            let _com = Com::init();
            let info = OPENASINFO {
                pcszFile: PCWSTR(path.as_ptr()),
                pcszClass: PCWSTR::null(),
                oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_REGISTER_EXT | OAIF_EXEC,
            };
            // Cancelling the dialog is an error too; nothing to do about either.
            if let Err(e) = unsafe { SHOpenWithDialog(None, &info) } {
                tracing::info!("open with closed: {e}");
            }
        })
        .map(drop)
        .map_err(|e| e.to_string())
}

/// The Explorer properties sheet; the shell runs it on a thread of its own.
pub fn properties(path: &str) -> Result<(), String> {
    let _com = Com::init();
    let shown = unsafe { SHObjectProperties(None, SHOP_FILEPATH, &HSTRING::from(path), None) };
    if shown.as_bool() {
        Ok(())
    } else {
        Err(format!("no properties for {path}"))
    }
}

/// Windows Terminal in the folder (a file's own folder), or PowerShell where it isn't installed.
pub fn terminal(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().ok_or("no folder")?
    };
    if Command::new("wt.exe").arg("-d").arg(dir).spawn().is_ok() {
        return Ok(());
    }
    Command::new("powershell.exe")
        .arg("-NoExit")
        .current_dir(dir)
        .creation_flags(CREATE_NEW_CONSOLE)
        .spawn()
        .map(drop)
        .map_err(|e| e.to_string())
}

/// Moves to the Recycle Bin without the shell's own prompt: the panel has already asked.
pub fn recycle(path: &str) -> Result<(), String> {
    // A list of paths, each null-terminated, ending with an extra null.
    let from: Vec<u16> = path.encode_utf16().chain([0, 0]).collect();
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT).0 as u16,
        ..Default::default()
    };
    match unsafe { SHFileOperationW(&mut op) } {
        0 if !op.fAnyOperationsAborted.as_bool() => Ok(()),
        0 => Err("recycling was cancelled".into()),
        code => Err(format!("could not recycle {path} (code {code:#x})")),
    }
}

/// COM for the shell, released when dropped.
pub(crate) struct Com(bool);

impl Com {
    pub(crate) fn init() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

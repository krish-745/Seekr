//! Who may see what: the signed-in console user, the other accounts' profiles they must not see,
//! and the snapshot file and pipe that only SYSTEM and that user can open.

use std::{
    fs::File,
    io,
    os::windows::{ffi::OsStrExt, io::FromRawHandle},
    path::{Path, PathBuf},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE, HLOCAL, LocalFree},
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_GENERIC_WRITE,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_MODE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
        },
        System::{
            Environment::ExpandEnvironmentStringsW,
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE, PIPE_WAIT,
            },
            RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken},
        },
    },
    core::{PCWSTR, PWSTR},
};

const PIPE_BUFFER: u32 = 64 << 10;

/// The SID of whoever is signed in at the console, or None at the sign-in screen. Needs
/// LocalSystem (WTSQueryUserToken requires the TCB privilege).
pub fn console_user() -> Option<String> {
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == u32::MAX {
        return None;
    }
    let mut token = HANDLE::default();
    unsafe { WTSQueryUserToken(session, &mut token).ok()? };
    let sid = token_sid(token);
    unsafe {
        let _ = CloseHandle(token);
    }
    sid
}

/// The SID this process runs as, so tests can build objects only they may open.
#[cfg(test)]
pub fn current_sid() -> Option<String> {
    use windows::Win32::{
        Security::TOKEN_QUERY,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()? };
    let sid = token_sid(token);
    unsafe {
        let _ = CloseHandle(token);
    }
    sid
}

fn token_sid(token: HANDLE) -> Option<String> {
    let mut len = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut len) };
    let mut buf = vec![0u8; len as usize];
    unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
        .ok()?;
    }
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut raw = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut raw).ok()? };
    let sid = unsafe { raw.to_string() }.ok();
    unsafe {
        let _ = LocalFree(Some(HLOCAL(raw.0.cast())));
    }
    sid
}

/// Every profile folder in ProfileList except `user_sid`'s: other people, and the service accounts.
pub fn other_profiles(user_sid: &str) -> io::Result<Vec<PathBuf>> {
    let list = windows_registry::LOCAL_MACHINE
        .open(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList")
        .map_err(io::Error::from)?;
    let mut paths = Vec::new();
    for sid in list.keys().map_err(io::Error::from)? {
        if sid.eq_ignore_ascii_case(user_sid) {
            continue;
        }
        if let Ok(path) = list
            .open(&sid)
            .and_then(|k| k.get_hstring("ProfileImagePath"))
        {
            paths.push(PathBuf::from(expand(&path.to_string_lossy())));
        }
    }
    Ok(paths)
}

fn expand(s: &str) -> String {
    let src: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
    let needed = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), None) };
    let mut out = vec![0u16; needed as usize];
    let written = unsafe { ExpandEnvironmentStringsW(PCWSTR(src.as_ptr()), Some(&mut out)) };
    if written == 0 {
        return s.to_string();
    }
    String::from_utf16_lossy(&out[..written as usize - 1])
}

/// The NTFS file reference number of a file or folder: what USN records call it.
pub fn frn_of(path: &Path) -> io::Result<u64> {
    let wide = wide(path.as_os_str());
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )?
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let got = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe {
        let _ = CloseHandle(handle);
    }
    got?;
    Ok((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

/// SYSTEM has full control; the user may only read.
pub fn file_sddl(user_sid: &str) -> String {
    format!("D:P(A;;FA;;;SY)(A;;FR;;;{user_sid})")
}

/// SYSTEM has full control; the user may read and write messages.
pub fn pipe_sddl(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{user_sid})")
}

/// Lets local users read the service's logs (counts, SIDs and profile paths, never file names) so
/// problems can be diagnosed without elevation. SYSTEM and Administrators keep full control; the
/// ACEs are inherited, so log files created later get the same access.
pub fn share_logs(dir: &Path) -> io::Result<()> {
    use windows::Win32::Security::{
        ACL,
        Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW},
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    let descriptor = Descriptor::parse("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;BU)")?;
    let (mut present, mut defaulted) = (false.into(), false.into());
    let mut dacl: *mut ACL = std::ptr::null_mut();
    unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut dacl, &mut defaulted)? };
    let wide = wide(dir.as_os_str());
    let status = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(wide.as_ptr()),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(dacl),
            None,
        )
    };
    status.ok().map_err(io::Error::from)
}

/// A security descriptor parsed from SDDL, freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn parse(sddl: &str) -> io::Result<Self> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )?;
        }
        Ok(Self(sd))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

/// Creates a new file only SYSTEM and `user_sid` can open. Fails if it exists, because an
/// existing file would keep its old permissions.
pub fn create_private_file(path: &Path, user_sid: &str) -> io::Result<File> {
    let descriptor = Descriptor::parse(&file_sddl(user_sid))?;
    let sa = descriptor.attributes();
    let wide = wide(path.as_os_str());
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_MODE(0),
            Some(&sa),
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )?
    };
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

/// The one instance of `name`. FIRST_PIPE_INSTANCE fails if anyone else already created it, so
/// another program can't sit on the name and impersonate the service.
pub fn create_pipe(name: &str, user_sid: &str) -> io::Result<File> {
    let descriptor = Descriptor::parse(&pipe_sddl(user_sid))?;
    let sa = descriptor.attributes();
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            PIPE_BUFFER,
            PIPE_BUFFER,
            0,
            Some(&sa),
        )
    };
    if handle.is_invalid() {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_handle(handle.0) })
}

/// Blocks until a client connects.
pub fn accept(pipe: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    match unsafe { ConnectNamedPipe(HANDLE(pipe.as_raw_handle()), None) } {
        Ok(()) => Ok(()),
        // The client connected between CreateNamedPipe and ConnectNamedPipe.
        Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
    s.encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsindex::wire::{self, Message};
    use std::io::{Read, Write};

    fn me() -> String {
        current_sid().expect("our own SID")
    }

    #[test]
    fn sddl_parses_for_a_real_sid_and_rejects_garbage() {
        assert!(Descriptor::parse(&file_sddl(&me())).is_ok());
        assert!(Descriptor::parse(&pipe_sddl(&me())).is_ok());
        assert!(Descriptor::parse("D:P(A;;FA;;;not-a-sid)").is_err());
    }

    #[test]
    fn private_file_is_read_only_for_the_user() {
        let path = std::env::temp_dir().join(format!("seekr-private-{}.bin", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut f = create_private_file(&path, &me()).unwrap();
        f.write_all(b"secret").unwrap();
        drop(f);
        let mut text = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "secret");
        let write = std::fs::OpenOptions::new().write(true).open(&path);
        assert_eq!(write.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(
            create_private_file(&path, &me()).is_err(),
            "never reuses an existing file"
        );
        // We may not delete it either (only SYSTEM may), so let the temp folder keep it.
    }

    #[test]
    fn pipe_carries_messages_and_refuses_a_second_server() {
        let name = format!(r"\\.\pipe\seekr-test-{}", std::process::id());
        let mut server = create_pipe(&name, &me()).unwrap();
        assert!(create_pipe(&name, &me()).is_err(), "the name is taken");
        let client_name = name.clone();
        let client = std::thread::spawn(move || {
            let mut c = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&client_name)
                .unwrap();
            wire::write_message(
                &mut c,
                &Message::Hello {
                    journal_id: 1,
                    usn: 2,
                },
            )
            .unwrap();
            wire::read_message(&mut c).unwrap()
        });
        accept(&server).unwrap();
        assert_eq!(
            wire::read_message(&mut server).unwrap(),
            Message::Hello {
                journal_id: 1,
                usn: 2
            }
        );
        let reply = Message::Changes {
            next_usn: 3,
            records: Vec::new(),
        };
        wire::write_message(&mut server, &reply).unwrap();
        assert_eq!(client.join().unwrap(), reply);
    }

    #[test]
    fn lists_other_profiles_but_not_mine() {
        let mine = PathBuf::from(std::env::var("USERPROFILE").unwrap());
        let others = other_profiles(&me()).unwrap();
        assert!(!others.contains(&mine));
        assert!(
            others.iter().any(|p| p.ends_with(r"config\systemprofile")),
            "{others:?}"
        );
    }

    #[test]
    fn frn_is_stable_per_folder() {
        let dir = std::env::temp_dir();
        let a = frn_of(&dir).unwrap();
        assert_ne!(a, 0);
        assert_eq!(a, frn_of(&dir).unwrap());
        assert_ne!(a, frn_of(Path::new(r"C:\Windows")).unwrap());
    }
}

//! Fetches what `?` questions need when it isn't on this PC yet: llama.cpp's server (with the
//! CUDA runtime it needs) and the Qwen3.5-4B model, about 3.3 GB. Each file is checked against
//! its SHA-256 before it is used; a download that doesn't match is thrown away.

use std::{
    fs,
    io::{Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::ipc::Channel;
use windows::{Win32::Storage::FileSystem::GetDiskFreeSpaceExW, core::HSTRING};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct Part {
    label: &'static str,
    url: &'static str,
    sha256: &'static str,
    bytes: u64,
    /// Its size on disk once installed: a zip is unpacked and then deleted.
    installed: u64,
    /// Under the data folder: the zip's folder to unpack into, or the file itself.
    into: &'static str,
    /// Under the data folder: present once this part is installed.
    marker: &'static str,
}

/// In install order: the runtime's DLLs go in before llama-server.exe, which marks it installed.
// shortcut: only the default model and the CUDA build are offered; a PC without an NVIDIA GPU
// would need llama.cpp's Vulkan or CPU build here.
const PARTS: [Part; 3] = [
    Part {
        label: "CUDA runtime for llama.cpp",
        url: "https://github.com/ggml-org/llama.cpp/releases/download/b11819/cudart-llama-bin-win-cuda-13.4-x64.zip",
        sha256: "738f8c251ac22b70c3ae6f83a10cf222725df0395246a2cf58f32bdb85fbe668",
        bytes: 423_535_356,
        installed: 548_245_840,
        into: r"llm\llama.cpp-b11819",
        marker: r"llm\llama.cpp-b11819\cudart64_13.dll",
    },
    Part {
        label: "llama.cpp b11819",
        url: "https://github.com/ggml-org/llama.cpp/releases/download/b11819/llama-b11819-bin-win-cuda-13.4-x64.zip",
        sha256: "d10029e6c1a74fbdbfd65c4c8a00d46e8640f8e78ce91b8e48499ec609250938",
        bytes: 159_849_219,
        installed: 212_038_941,
        into: r"llm\llama.cpp-b11819",
        marker: crate::ai::RUNTIME,
    },
    Part {
        label: "Qwen3.5 4B model (Q4_K_M)",
        url: "https://huggingface.co/unsloth/Qwen3.5-4B-GGUF/resolve/e87f176479d0855a907a41277aca2f8ee7a09523/Qwen3.5-4B-Q4_K_M.gguf",
        sha256: "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4",
        bytes: 2_740_937_888,
        installed: 2_740_937_888,
        into: r"llm\models\Qwen3.5-4B-Q4_K_M.gguf",
        marker: r"llm\models\Qwen3.5-4B-Q4_K_M.gguf",
    },
];

static INSTALLING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

#[derive(Serialize)]
pub struct PartState {
    label: &'static str,
    bytes: u64,
    done: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSetup {
    parts: Vec<PartState>,
    /// Bytes still to download, the most the download needs free at once (a zip and what it
    /// unpacks to), and what everything takes once installed.
    download: u64,
    needed: u64,
    installed: u64,
    /// Free space on the data folder's drive.
    free: u64,
    folder: String,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SetupEvent {
    Progress { part: usize, done: u64 },
    Status { text: String },
    Done,
    Error { text: String },
}

fn missing() -> impl Iterator<Item = (usize, &'static Part)> {
    PARTS
        .iter()
        .enumerate()
        .filter(|(_, p)| !common::data_dir().join(p.marker).exists())
}

#[tauri::command]
pub fn ai_setup() -> AiSetup {
    let llm = common::data_dir().join("llm");
    let _ = fs::create_dir_all(&llm);
    let mut free = 0u64;
    let _ = unsafe {
        GetDiskFreeSpaceExW(&HSTRING::from(llm.as_os_str()), Some(&mut free), None, None)
    };
    AiSetup {
        parts: PARTS
            .iter()
            .map(|p| PartState {
                label: p.label,
                bytes: p.bytes,
                done: common::data_dir().join(p.marker).exists(),
            })
            .collect(),
        download: missing().map(|(_, p)| p.bytes).sum(),
        needed: missing().map(|(_, p)| p.installed).sum::<u64>()
            + missing()
                .map(|(_, p)| if p.bytes == p.installed { 0 } else { p.bytes })
                .max()
                .unwrap_or(0),
        installed: PARTS.iter().map(|p| p.installed).sum(),
        free,
        folder: llm.display().to_string(),
    }
}

/// Downloads and installs whatever is missing, reporting progress, until done or cancelled.
#[tauri::command]
pub async fn install_ai(on_event: Channel<SetupEvent>) -> Result<(), String> {
    if INSTALLING.swap(true, Ordering::SeqCst) {
        return Err("already downloading".into());
    }
    CANCEL.store(false, Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        let send = |e: SetupEvent| {
            let _ = on_event.send(e);
        };
        let result = missing().try_for_each(|(i, part)| install(i, part, &send));
        INSTALLING.store(false, Ordering::SeqCst);
        match result {
            Ok(()) => send(SetupEvent::Done),
            Err(e) => {
                tracing::warn!("AI setup failed: {e}");
                send(SetupEvent::Error { text: e });
            }
        }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cancel_ai_setup() {
    CANCEL.store(true, Ordering::SeqCst);
}

fn install(index: usize, part: &Part, send: &impl Fn(SetupEvent)) -> Result<(), String> {
    let data = common::data_dir();
    let into = data.join(part.into);
    let file_name = part.url.rsplit('/').next().unwrap_or("download");
    let partial = data.join("llm").join(format!("{file_name}.part"));
    let result = download(index, part, &partial, send).and_then(|()| {
        if part.bytes == part.installed {
            fs::create_dir_all(into.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::rename(&partial, &into).map_err(|e| e.to_string())
        } else {
            send(SetupEvent::Status {
                text: format!("Unpacking {}…", part.label),
            });
            unzip(&partial, &into)
        }
    });
    let _ = fs::remove_file(&partial);
    result?;
    tracing::info!("installed {}", part.label);
    Ok(())
}

// shortcut: a stopped download starts again from the beginning; resume with a Range request if
// people on slow connections need it.
fn download(
    index: usize,
    part: &Part,
    to: &Path,
    send: &impl Fn(SetupEvent),
) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .build()
        .into();
    let mut response = agent
        .get(part.url)
        .call()
        .map_err(|e| format!("couldn't download {}: {e}", part.label))?;
    let mut body = response
        .body_mut()
        .with_config()
        .limit(part.bytes + 1)
        .reader();
    fs::create_dir_all(to.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut file = fs::File::create(to).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    let (mut done, mut reported) = (0u64, Instant::now());
    send(SetupEvent::Progress { part: index, done });
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let n = body
            .read(&mut buffer)
            .map_err(|e| format!("the download of {} broke off: {e}", part.label))?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        hash.update(&buffer[..n]);
        done += n as u64;
        if reported.elapsed() > Duration::from_millis(200) {
            reported = Instant::now();
            send(SetupEvent::Progress { part: index, done });
        }
    }
    file.sync_all().map_err(|e| e.to_string())?;
    send(SetupEvent::Progress { part: index, done });
    let sha256: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if done != part.bytes || sha256 != part.sha256 {
        return Err(format!(
            "{} didn't match what was expected, so it wasn't installed",
            part.label
        ));
    }
    Ok(())
}

/// Unpacks with Windows' own tar, which reads zip files.
fn unzip(zip: &Path, into: &Path) -> Result<(), String> {
    fs::create_dir_all(into).map_err(|e| e.to_string())?;
    let tar = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
        .join(r"System32\tar.exe");
    let out = Command::new(tar)
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(into)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("couldn't unpack: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "couldn't unpack: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_install_in_an_order_that_marks_the_runtime_last() {
        assert_eq!(PARTS[1].marker, crate::ai::RUNTIME);
        assert!(
            PARTS
                .iter()
                .all(|p| p.sha256.len() == 64 && p.url.starts_with("https://"))
        );
        let setup = ai_setup();
        assert!(setup.installed > 3_400_000_000);
        assert!(setup.download == 0 || setup.needed >= setup.download);
    }

    /// Downloads llama.cpp (160 MB) into a scratch data folder, checks and unpacks it:
    /// `cargo test -p seekr live_download -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_download() {
        let dir = std::env::temp_dir().join(format!("seekr-setup-{}", std::process::id()));
        // Nothing else in this test binary reads the data folder while this runs.
        unsafe { std::env::set_var("SEEKR_DATA_DIR", &dir) };
        let events = std::sync::Mutex::new(Vec::new());
        let send = |e: SetupEvent| events.lock().unwrap().push(e);
        let started = Instant::now();
        install(1, &PARTS[1], &send).unwrap();
        let progress = events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, SetupEvent::Progress { .. }))
            .count();
        println!(
            "installed in {:?} with {progress} progress reports",
            started.elapsed()
        );
        assert!(dir.join(crate::ai::RUNTIME).exists());
        assert!(
            !dir.join(r"llm\llama-b11819-bin-win-cuda-13.4-x64.zip.part")
                .exists()
        );

        // A download that doesn't match its hash isn't installed.
        let wrong = Part {
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            into: r"llm\wrong.gguf",
            marker: r"llm\wrong.gguf",
            ..PARTS[1]
        };
        let wrong = Part {
            installed: wrong.bytes,
            ..wrong
        };
        let e = install(1, &wrong, &send).unwrap_err();
        assert!(e.contains("didn't match"), "{e}");
        assert!(!dir.join(r"llm\wrong.gguf").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}

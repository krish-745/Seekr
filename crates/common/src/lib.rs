//! Shared paths, config and logging for all Seekr processes.

pub mod content;

use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    io::{self, Write},
    path::Path,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Deserialize;
use tracing_appender::{non_blocking::WorkerGuard, rolling};

/// The single app-ID constant; renaming the product starts here.
pub const APP_NAME: &str = "Seekr";

/// `%LOCALAPPDATA%\Seekr`: indexes, databases, models, logs. `SEEKR_DATA_DIR` replaces it, so
/// test runs keep their state apart without moving LOCALAPPDATA, which the shell itself needs.
pub fn data_dir() -> PathBuf {
    std::env::var_os("SEEKR_DATA_DIR").map_or_else(|| env_dir("LOCALAPPDATA"), PathBuf::from)
}

/// `%APPDATA%\Seekr`: user config.
pub fn config_dir() -> PathBuf {
    env_dir("APPDATA")
}

fn env_dir(var: &str) -> PathBuf {
    let base = std::env::var_os(var).unwrap_or_else(|| panic!("{var} is not set"));
    PathBuf::from(base).join(APP_NAME)
}

/// User settings from `config.toml`; every field is optional.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: String,
    /// The web shortcut the "Search the web" row uses.
    pub search: String,
    /// Added or replaced web shortcuts: key to URL, with `%s` for the words typed.
    pub shortcuts: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Alt+Space".into(),
            search: "g".into(),
            shortcuts: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load() -> io::Result<Self> {
        Self::load_from(&config_dir().join("config.toml"))
    }

    /// A missing file means defaults; a malformed one is an error, never silently ignored.
    pub fn load_from(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
}

/// Logs to `%LOCALAPPDATA%\Seekr\logs\<name>.YYYY-MM-DD.log`, daily rotation, 7 files kept.
/// Keep the returned guard alive for the life of the process or buffered lines are lost.
pub fn init_logging(name: &str) -> Result<WorkerGuard, Box<dyn Error + Send + Sync>> {
    let appender = rolling::Builder::new()
        .rotation(rolling::Rotation::DAILY)
        .filename_prefix(name)
        .filename_suffix("log")
        .max_log_files(7)
        .build(data_dir().join("logs"))?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .try_init()?;
    install_panic_hook(data_dir().join("logs").join(format!("{name}.panic.log")));
    Ok(guard)
}

/// Appends panics to `path` synchronously: with `panic = "abort"` the non-blocking
/// writer dies before flushing, and release builds have no console for stderr.
fn install_panic_hook(path: PathBuf) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("{info}");
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| writeln!(f, "unix {secs}: {info}"));
        default_hook(info);
    }));
}

/// True when Windows says the machine is running on its battery (false on mains or if it can't
/// tell). Bulk embedding waits for mains power.
pub fn on_battery() -> bool {
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status) }.is_ok() && status.ACLineStatus == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, text: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("seekr-test-{}-{name}.toml", std::process::id()));
        fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn missing_file_gives_defaults() {
        let cfg = Config::load_from(Path::new("Z:/does/not/exist.toml")).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn reads_hotkey() {
        let path = write_temp("hotkey", "hotkey = \"Win+Space\"");
        assert_eq!(Config::load_from(&path).unwrap().hotkey, "Win+Space");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_unknown_and_malformed() {
        for (name, text) in [("unknown", "hotky = \"x\""), ("malformed", "hotkey = ")] {
            let path = write_temp(name, text);
            let err = Config::load_from(&path).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn panics_are_written_to_file() {
        let path =
            std::env::temp_dir().join(format!("seekr-test-{}.panic.log", std::process::id()));
        install_panic_hook(path.clone());
        let _ = std::panic::catch_unwind(|| panic!("boom"));
        let _ = std::panic::take_hook();
        let text = fs::read_to_string(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert!(text.contains("boom"), "{text}");
    }

    #[test]
    fn dirs_end_in_app_name() {
        assert!(data_dir().ends_with(APP_NAME));
        assert!(config_dir().ends_with(APP_NAME));
    }
}

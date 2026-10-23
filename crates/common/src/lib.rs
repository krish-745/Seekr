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

/// `%APPDATA%\Seekr`: user config. `SEEKR_CONFIG_DIR` replaces it, as `SEEKR_DATA_DIR` does.
pub fn config_dir() -> PathBuf {
    std::env::var_os("SEEKR_CONFIG_DIR").map_or_else(|| env_dir("APPDATA"), PathBuf::from)
}

fn env_dir(var: &str) -> PathBuf {
    let base = std::env::var_os(var).unwrap_or_else(|| panic!("{var} is not set"));
    PathBuf::from(base).join(APP_NAME)
}

/// User settings from `config.toml`; every field is optional.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: String,
    /// The web shortcut the "Search the web" row uses.
    pub search: String,
    /// Added or replaced web shortcuts: key to URL, with `%s` for the words typed.
    pub shortcuts: BTreeMap<String, String>,
    /// The cloud API for general (`??`) questions: "gemini" or "groq"; empty for whichever key is
    /// set. Questions about files always use the model on this PC.
    pub api: String,
    /// The local model's file name in `llm\models`, without ".gguf"; empty for the default.
    pub ai_model: String,
    /// The cloud API's model; empty for its default.
    pub api_model: String,
    /// Left out of file and content results: full folder paths, or names that match any part
    /// of a path (a folder or file name, such as `node_modules`).
    pub exclude: Vec<String>,
    /// Tools that can be turned off.
    pub tools: Tools,
    /// The words that open a tool's list.
    pub prefixes: Prefixes,
    /// Clipboard history keeps entries this many days, and at most this many (pinned ones stay).
    pub clipboard_days: u32,
    pub clipboard_entries: u32,
    /// The local AI model is unloaded after this many minutes without a question.
    pub ai_idle_minutes: u32,
    /// Index content meaning on battery too; by default a big pass waits for mains power.
    pub embed_on_battery: bool,
    /// How the panel looks.
    pub theme: Theme,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    /// "system", "light" or "dark".
    pub mode: String,
    /// A CSS color for the selection marks and links; empty for Windows' blue.
    pub accent: String,
    /// "mica", "acrylic" or "solid".
    pub backdrop: String,
    /// How much of the panel's own color covers the backdrop, 0 to 100.
    pub opacity: u32,
    /// A font family; empty for Segoe UI.
    pub font: String,
    pub font_size: u32,
    /// "comfortable" or "compact" rows.
    pub density: String,
    /// The panel's size in logical pixels.
    pub width: u32,
    pub height: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            mode: "system".into(),
            accent: String::new(),
            backdrop: "mica".into(),
            opacity: 0,
            font: String::new(),
            font_size: 14,
            density: "comfortable".into(),
            width: 760,
            height: 480,
        }
    }
}

impl Theme {
    fn check(&self) -> Result<(), String> {
        let one_of = |value: &str, name: &str, allowed: &[&str]| {
            if allowed.contains(&value) {
                Ok(())
            } else {
                Err(format!(
                    "theme {name} must be one of {}",
                    allowed.join(", ")
                ))
            }
        };
        one_of(&self.mode, "mode", &["system", "light", "dark"])?;
        one_of(&self.backdrop, "backdrop", &["mica", "acrylic", "solid"])?;
        one_of(&self.density, "density", &["comfortable", "compact"])?;
        let within = |value: u32, name: &str, low: u32, high: u32| {
            if (low..=high).contains(&value) {
                Ok(())
            } else {
                Err(format!("theme {name} must be from {low} to {high}"))
            }
        };
        within(self.opacity, "opacity", 0, 100)?;
        within(self.font_size, "font_size", 10, 24)?;
        within(self.width, "width", 500, 1600)?;
        within(self.height, "height", 300, 1200)?;
        // Both go into CSS, so nothing that could end a declaration or a string.
        let plain = |text: &str| !text.contains([';', '{', '}', '"', '\'', '<', '\\']);
        if !plain(&self.accent) || !plain(&self.font) {
            return Err("theme accent and font can't contain ; { } quotes < or \\".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tools {
    pub calculator: bool,
    pub clipboard: bool,
    pub windows: bool,
    pub system: bool,
    pub web: bool,
    pub emoji: bool,
    pub notes: bool,
    pub ai: bool,
}

impl Default for Tools {
    fn default() -> Self {
        Self {
            calculator: true,
            clipboard: true,
            windows: true,
            system: true,
            web: true,
            emoji: true,
            notes: true,
            ai: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Prefixes {
    pub clipboard: String,
    pub windows: String,
    pub emoji: String,
    pub note: String,
    pub todo: String,
}

impl Default for Prefixes {
    fn default() -> Self {
        Self {
            clipboard: "clip".into(),
            windows: "win".into(),
            emoji: "emoji".into(),
            note: "note".into(),
            todo: "todo".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "Alt+Space".into(),
            search: "g".into(),
            shortcuts: BTreeMap::new(),
            api: String::new(),
            ai_model: String::new(),
            api_model: String::new(),
            exclude: Vec::new(),
            tools: Tools::default(),
            prefixes: Prefixes::default(),
            clipboard_days: 30,
            clipboard_entries: 1_000,
            ai_idle_minutes: 10,
            embed_on_battery: false,
            theme: Theme::default(),
        }
    }
}

impl Config {
    pub fn load() -> io::Result<Self> {
        Self::load_from(&config_dir().join("config.toml"))
    }

    /// A missing file means defaults; a malformed one is an error, never silently ignored.
    pub fn load_from(path: &Path) -> io::Result<Self> {
        let invalid = |e: String| io::Error::new(io::ErrorKind::InvalidData, e);
        let config: Self = match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| invalid(e.to_string()))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        config.check().map_err(invalid)?;
        Ok(config)
    }

    /// What TOML can't say: prefixes are single words, distinct, and counts aren't zero.
    pub fn check(&self) -> Result<(), String> {
        let p = &self.prefixes;
        let words = [&p.clipboard, &p.windows, &p.emoji, &p.note, &p.todo];
        for (i, word) in words.iter().enumerate() {
            if word.is_empty() || word.contains(char::is_whitespace) || word.starts_with(['?', ':'])
            {
                return Err(format!(
                    "prefix {word:?} must be one word, not starting with ? or :"
                ));
            }
            if words[..i].iter().any(|w| w.eq_ignore_ascii_case(word)) {
                return Err(format!("prefix {word:?} is used twice"));
            }
        }
        self.theme.check()?;
        if self.clipboard_days == 0 || self.clipboard_entries == 0 || self.ai_idle_minutes == 0 {
            return Err(
                "clipboard_days, clipboard_entries and ai_idle_minutes must be above 0".into(),
            );
        }
        Ok(())
    }

    /// Whether `path` is inside an excluded folder or has an excluded name as one of its parts.
    // shortcut: plain names and folder paths only, no wildcards; add globs if people ask.
    pub fn excludes(&self, path: &str) -> bool {
        let lower = path.replace('/', "\\").to_lowercase();
        self.exclude.iter().any(|entry| {
            let entry = entry.trim().replace('/', "\\").to_lowercase();
            let entry = entry.trim_end_matches('\\');
            if entry.is_empty() {
                false
            } else if entry.contains('\\') || entry.contains(':') {
                lower == entry || lower.starts_with(&format!("{entry}\\"))
            } else {
                lower.split('\\').any(|part| part == entry)
            }
        })
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
    fn excludes_folders_and_names() {
        let cfg = Config {
            exclude: vec![
                r"C:\Users\me\Big\".into(),
                "node_modules".into(),
                " ".into(),
            ],
            ..Default::default()
        };
        assert!(cfg.excludes(r"C:\Users\me\Big"));
        assert!(cfg.excludes(r"c:\users\me\big\a.txt"));
        assert!(!cfg.excludes(r"C:\Users\me\Bigger\a.txt"));
        assert!(cfg.excludes(r"D:\code\app\node_modules\x\index.js"));
        assert!(!cfg.excludes(r"D:\code\app\node_modules_old\index.js"));
        assert!(!Config::default().excludes(r"C:\anything"));
    }

    #[test]
    fn reads_tables_and_checks_them() {
        let path = write_temp(
            "tables",
            "clipboard_days = 7\n[tools]\nemoji = false\n[prefixes]\nclipboard = \"cb\"",
        );
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(
            (cfg.clipboard_days, cfg.tools.emoji, cfg.tools.web),
            (7, false, true)
        );
        assert_eq!(
            (cfg.prefixes.clipboard.as_str(), cfg.prefixes.todo.as_str()),
            ("cb", "todo")
        );
        for bad in [
            "[theme]\nmode = \"sepia\"",
            "[theme]\nwidth = 100",
            "[theme]\naccent = \"red; background: url(x)\"",
            "[prefixes]\nnote = \"todo\"",
            "[prefixes]\nwindows = \"my win\"",
            "ai_idle_minutes = 0",
        ] {
            fs::write(&path, bad).unwrap();
            assert_eq!(
                Config::load_from(&path).unwrap_err().kind(),
                io::ErrorKind::InvalidData,
                "{bad}"
            );
        }
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

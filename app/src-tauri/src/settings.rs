//! Live settings. config.toml is read at start and again whenever it changes on disk, and what
//! changed is applied without a restart; a file with an error keeps the settings Seekr already
//! has. The panel's settings view edits the file, keeping the user's comments.

use std::{
    fs, io,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex, RwLock},
    time::{Duration, SystemTime},
};

use common::Config;
use serde::Serialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::Shortcut;
use toml_edit::DocumentMut;

use crate::{ai::Ai, indexer::Indexer, web::Web};

static CURRENT: LazyLock<RwLock<Arc<Config>>> =
    LazyLock::new(|| RwLock::new(Arc::new(load().unwrap_or_default())));
/// What is wrong with config.toml, while something is.
static PROBLEM: Mutex<Option<String>> = Mutex::new(None);

/// The settings in force.
pub fn get() -> Arc<Config> {
    CURRENT.read().unwrap().clone()
}

pub fn path() -> PathBuf {
    common::config_dir().join("config.toml")
}

fn load() -> Option<Config> {
    match Config::load() {
        Ok(config) => {
            *PROBLEM.lock().unwrap() = None;
            Some(config)
        }
        Err(e) => {
            tracing::error!("config.toml not applied: {e}");
            *PROBLEM.lock().unwrap() = Some(e.to_string());
            None
        }
    }
}

fn modified() -> Option<SystemTime> {
    fs::metadata(path()).and_then(|m| m.modified()).ok()
}

/// Applies config.toml whenever it changes.
// shortcut: checks the file's time every two seconds; a directory watch would wake less often.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let mut seen = modified();
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let now = modified();
            if now != seen {
                seen = now;
                reload(&app);
            }
        }
    });
}

/// Rereads config.toml and applies what changed.
pub fn reload(app: &AppHandle) {
    let Some(new) = load() else {
        return;
    };
    let old = get();
    if *old == new {
        return;
    }
    tracing::info!("config.toml changed; applying it");
    *CURRENT.write().unwrap() = Arc::new(new.clone());
    if old.hotkey != new.hotkey {
        crate::set_hotkey(app, &old.hotkey, &new.hotkey);
    }
    if (&old.shortcuts, &old.search) != (&new.shortcuts, &new.search)
        && let Some(web) = app.try_state::<Web>()
    {
        web.set_shortcuts(&new.shortcuts, &new.search);
    }
    if let Some(ai) = app.try_state::<Ai>() {
        ai.apply(&new);
    }
    // The indexer reads both at each pass; a full pass drops what is now left out.
    if (&old.exclude, old.embed_on_battery) != (&new.exclude, new.embed_on_battery)
        && let Some(indexer) = app.try_state::<Indexer>()
    {
        indexer.refresh();
    }
}

/// One row of the settings view. `kind` is text, number, toggle, choice, lines (one entry per
/// line) or action (handled by the panel).
#[derive(Serialize)]
pub struct Field {
    key: &'static str,
    group: &'static str,
    label: &'static str,
    kind: &'static str,
    value: Value,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    choices: &'static [&'static str],
    hint: &'static str,
}

#[derive(Serialize)]
pub struct View {
    fields: Vec<Field>,
    /// Why config.toml isn't applied, if it isn't.
    problem: Option<String>,
    path: String,
}

fn field(
    key: &'static str,
    group: &'static str,
    label: &'static str,
    kind: &'static str,
    value: Value,
    hint: &'static str,
) -> Field {
    Field {
        key,
        group,
        label,
        kind,
        value,
        choices: &[],
        hint,
    }
}

fn fields(c: &Config) -> Vec<Field> {
    let t = &c.tools;
    let p = &c.prefixes;
    let shortcuts: Vec<String> = c
        .shortcuts
        .iter()
        .map(|(k, url)| format!("{k} {url}"))
        .collect();
    let folders = common::content::chosen().map_or(0, |f| f.len());
    vec![
        field(
            "hotkey",
            "General",
            "Hotkey",
            "text",
            json!(c.hotkey),
            "Such as Alt+Space or Ctrl+Shift+K",
        ),
        field(
            "folders",
            "Search",
            "Content folders",
            "action",
            json!(format!("{folders} chosen")),
            "The folders Seekr searches inside",
        ),
        field(
            "exclude",
            "Search",
            "Leave out",
            "lines",
            json!(c.exclude.join("\n")),
            "One per line: a folder's full path, or a name such as node_modules",
        ),
        field(
            "search",
            "Web",
            "Search the web with",
            "text",
            json!(c.search),
            "A web shortcut's key: g, ddg, b, or one of yours",
        ),
        field(
            "shortcuts",
            "Web",
            "Your web shortcuts",
            "lines",
            json!(shortcuts.join("\n")),
            "One per line: a key, a space, and the URL with %s where your words go",
        ),
        field(
            "tools.calculator",
            "Tools",
            "Calculator",
            "toggle",
            json!(t.calculator),
            "Math, units, currency, time zones and dates",
        ),
        field(
            "tools.clipboard",
            "Tools",
            "Clipboard history",
            "toggle",
            json!(t.clipboard),
            "Off stops recording what you copy",
        ),
        field(
            "tools.windows",
            "Tools",
            "Window switcher",
            "toggle",
            json!(t.windows),
            "",
        ),
        field(
            "tools.system",
            "Tools",
            "System commands",
            "toggle",
            json!(t.system),
            "Lock, volume, Wi-Fi, Settings pages and the like",
        ),
        field(
            "tools.web",
            "Tools",
            "Web",
            "toggle",
            json!(t.web),
            "Web shortcuts, bookmarks, history and the web search row",
        ),
        field(
            "tools.emoji",
            "Tools",
            "Emoji and symbols",
            "toggle",
            json!(t.emoji),
            "",
        ),
        field(
            "tools.notes",
            "Tools",
            "Notes and to-dos",
            "toggle",
            json!(t.notes),
            "",
        ),
        field(
            "tools.ai",
            "Tools",
            "AI answers",
            "toggle",
            json!(t.ai),
            "? and ??",
        ),
        field(
            "prefixes.clipboard",
            "Prefixes",
            "Clipboard history",
            "text",
            json!(p.clipboard),
            "Type it and a space",
        ),
        field(
            "prefixes.windows",
            "Prefixes",
            "Window switcher",
            "text",
            json!(p.windows),
            "",
        ),
        field(
            "prefixes.emoji",
            "Prefixes",
            "Emoji",
            "text",
            json!(p.emoji),
            ": works too",
        ),
        field(
            "prefixes.note",
            "Prefixes",
            "Notes",
            "text",
            json!(p.note),
            "",
        ),
        field(
            "prefixes.todo",
            "Prefixes",
            "To-dos",
            "text",
            json!(p.todo),
            "",
        ),
        field(
            "clipboard_days",
            "Clipboard",
            "Keep entries for (days)",
            "number",
            json!(c.clipboard_days),
            "Pinned entries stay until unpinned",
        ),
        field(
            "clipboard_entries",
            "Clipboard",
            "Keep at most (entries)",
            "number",
            json!(c.clipboard_entries),
            "",
        ),
        field(
            "ai_idle_minutes",
            "AI",
            "Unload the local model after (idle minutes)",
            "number",
            json!(c.ai_idle_minutes),
            "It hands the graphics memory back",
        ),
        Field {
            choices: &["", "groq", "gemini"],
            ..field(
                "api",
                "AI",
                "Cloud API for ??",
                "choice",
                json!(c.api),
                "Empty: Groq if its key is set, else Gemini",
            )
        },
        field(
            "ai_model",
            "AI",
            "Local model",
            "text",
            json!(c.ai_model),
            r"A .gguf file name in llm\models, without .gguf; empty for Qwen3.5-4B-Q4_K_M",
        ),
        field(
            "api_model",
            "AI",
            "Cloud model",
            "text",
            json!(c.api_model),
            "Empty for the API's default",
        ),
        field(
            "embed_on_battery",
            "Power",
            "Index meaning on battery",
            "toggle",
            json!(c.embed_on_battery),
            "Off: a big indexing pass waits for mains power",
        ),
        field(
            "config",
            "File",
            "Edit config.toml",
            "action",
            json!(path().display().to_string()),
            "Changes there apply within two seconds",
        ),
    ]
}

#[tauri::command]
pub fn settings() -> View {
    View {
        fields: fields(&get()),
        problem: PROBLEM.lock().unwrap().clone(),
        path: path().display().to_string(),
    }
}

/// Changes one setting in config.toml and applies it. A value back at its default is removed,
/// so the file holds only what the user changed.
#[tauri::command]
pub fn save_setting(app: AppHandle, key: String, value: Value) -> Result<View, String> {
    let text = match fs::read_to_string(path()) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => crate::builtin::CONFIG_TEMPLATE.into(),
        Err(e) => return Err(e.to_string()),
    };
    let text = edit(&text, &key, &value)?;
    fs::create_dir_all(common::config_dir()).map_err(|e| e.to_string())?;
    let tmp = path().with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path()).map_err(|e| e.to_string())?;
    tracing::info!("setting {key} changed");
    reload(&app);
    Ok(settings())
}

/// config.toml's text with one setting changed, if the result is a valid config.
fn edit(text: &str, key: &str, value: &Value) -> Result<String, String> {
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("config.toml has an error to fix first: {e}"))?;
    let default = fields(&Config::default())
        .into_iter()
        .find(|f| f.key == key && f.kind != "action")
        .ok_or_else(|| format!("{key} isn't a setting"))?;
    let item = if *value == default.value {
        None
    } else {
        Some(to_toml(key, default.kind, value)?)
    };
    match (key.split_once('.'), item) {
        (Some((table, name)), Some(item)) => {
            if !doc.contains_table(table) {
                doc[table] = toml_edit::table();
            }
            doc[table][name] = item;
        }
        (Some((table, name)), None) => {
            if let Some(t) = doc.get_mut(table).and_then(|t| t.as_table_like_mut()) {
                t.remove(name);
                if t.is_empty() {
                    doc.remove(table);
                }
            }
        }
        (None, Some(item)) => doc[key] = item,
        (None, None) => {
            doc.remove(key);
        }
    }
    let text = doc.to_string();
    let config: Config = toml::from_str(&text).map_err(|e| e.to_string())?;
    config.check()?;
    config
        .hotkey
        .parse::<Shortcut>()
        .map_err(|e| format!("{:?} isn't a hotkey: {e}", config.hotkey))?;
    Ok(text)
}

fn to_toml(key: &str, kind: &str, value: &Value) -> Result<toml_edit::Item, String> {
    let wrong = || format!("{key} needs a {kind} value");
    Ok(match kind {
        "toggle" => toml_edit::value(value.as_bool().ok_or_else(wrong)?),
        "number" => {
            let n = value
                .as_u64()
                .filter(|&n| n <= u64::from(u32::MAX))
                .ok_or_else(wrong)?;
            toml_edit::value(n as i64)
        }
        "lines" if key == "shortcuts" => {
            let mut table = toml_edit::Table::new();
            for line in value.as_str().ok_or_else(wrong)?.lines().map(str::trim) {
                if line.is_empty() {
                    continue;
                }
                let (name, url) = line
                    .split_once(char::is_whitespace)
                    .map(|(k, u)| (k.to_lowercase(), u.trim()))
                    .filter(|(_, u)| u.starts_with("https://") || u.starts_with("http://"))
                    .ok_or_else(|| format!("{line:?} needs a key, a space and a web address"))?;
                table[&name] = toml_edit::value(url);
            }
            toml_edit::Item::Table(table)
        }
        "lines" => {
            let lines = value.as_str().ok_or_else(wrong)?.lines();
            let entries: toml_edit::Array =
                lines.map(str::trim).filter(|l| !l.is_empty()).collect();
            toml_edit::value(entries)
        }
        _ => toml_edit::value(value.as_str().ok_or_else(wrong)?.trim()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_comments_and_drop_defaults() {
        let text = "# mine\nhotkey = \"Ctrl+Space\" # keep\n\n[tools]\nemoji = false\n";
        let text = edit(text, "clipboard_days", &json!(7)).unwrap();
        let text = edit(&text, "tools.web", &json!(false)).unwrap();
        let text = edit(&text, "tools.emoji", &json!(true)).unwrap();
        assert!(
            text.starts_with("# mine\nhotkey = \"Ctrl+Space\" # keep\n"),
            "{text}"
        );
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(config.clipboard_days, 7);
        assert!(!config.tools.web && config.tools.emoji);
        assert!(!text.contains("emoji"), "a default is removed: {text}");
        let text = edit(&text, "tools.web", &json!(true)).unwrap();
        assert!(!text.contains("[tools]"), "an empty table goes too: {text}");
        let text = edit(&text, "hotkey", &json!("Alt+Space")).unwrap();
        assert!(!text.contains("hotkey"), "{text}");
    }

    #[test]
    fn edits_lines_and_refuses_bad_values() {
        let text = edit("", "exclude", &json!("node_modules\n\n C:\\Big \n")).unwrap();
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(config.exclude, ["node_modules", r"C:\Big"]);
        let text = edit(
            "",
            "shortcuts",
            &json!("Jira https://j.example/browse/%s\n"),
        )
        .unwrap();
        let config: Config = toml::from_str(&text).unwrap();
        assert_eq!(config.shortcuts["jira"], "https://j.example/browse/%s");

        assert!(edit("", "shortcuts", &json!("jira")).is_err());
        assert!(edit("", "hotkey", &json!("Banana+Q")).is_err());
        assert!(edit("", "prefixes.note", &json!("todo")).is_err());
        assert!(edit("", "clipboard_days", &json!(0)).is_err());
        assert!(edit("", "clipboard_days", &json!("x")).is_err());
        assert!(edit("", "folders", &json!("x")).is_err());
        assert!(edit("hotkey = ", "hotkey", &json!("Alt+Space")).is_err());
    }

    #[test]
    fn the_template_is_a_valid_start() {
        let text = edit(crate::builtin::CONFIG_TEMPLATE, "api", &json!("groq")).unwrap();
        assert!(text.starts_with("api = \"groq\"\n"), "{text}");
        assert!(text.contains("\n# Seekr settings."), "{text}");
        assert_eq!(toml::from_str::<Config>(&text).unwrap().api, "groq");
    }
}

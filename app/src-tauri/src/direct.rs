//! Typed paths and URLs, offered as the first result when the whole query is one.

use std::path::Path;

use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use windows::Win32::Foundation::HWND;

use crate::{
    clipboard,
    query::{Action, Item},
};

const OPEN: Action = Action {
    id: "open",
    label: "Open",
};
const REVEAL: Action = Action {
    id: "reveal",
    label: "Show in folder",
};
const COPY_PATH: Action = Action {
    id: "copy",
    label: "Copy path",
};
const COPY_URL: Action = Action {
    id: "copy",
    label: "Copy URL",
};

pub fn items(text: &str) -> Vec<Item> {
    if let Some(url) = as_url(text) {
        return vec![Item {
            id: format!("url:{url}"),
            title: url.clone(),
            subtitle: "Open in browser".into(),
            preview: url,
            icon: None,
            actions: vec![OPEN, COPY_URL],
        }];
    }
    let path = expand(text);
    if !Path::new(&path).is_absolute() {
        return Vec::new();
    }
    // Network paths aren't probed: an offline server would stall every keystroke.
    let kind = if path.starts_with(r"\\") {
        "Network path"
    } else {
        match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => "Folder",
            Ok(_) => "File",
            Err(_) => return Vec::new(),
        }
    };
    vec![Item {
        id: format!("path:{path}"),
        title: path.clone(),
        subtitle: kind.into(),
        preview: path,
        icon: None,
        actions: vec![OPEN, REVEAL, COPY_PATH],
    }]
}

pub fn run(app: &AppHandle, item: &str, action: &str, owner: HWND) -> Result<(), String> {
    let opener = app.opener();
    let result = match (item.split_once(':'), action) {
        (Some(("url", url)), "open") => opener.open_url(url, None::<&str>),
        (Some(("path", path)), "open") => opener.open_path(path, None::<&str>),
        (Some(("path", path)), "reveal") => opener.reveal_item_in_dir(path),
        (Some((_, text)), "copy") => return clipboard::set_text(owner, text),
        _ => return Err(format!("unknown action {action} on {item}")),
    };
    result.map_err(|e| e.to_string())
}

fn as_url(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let lower = text.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Some(text.to_string())
    } else if lower.starts_with("www.") && text.len() > 4 && text[4..].contains('.') {
        Some(format!("https://{text}"))
    } else {
        None
    }
}

/// `~` becomes the profile folder and `%NAME%` the environment variable; unknown names stay as typed.
fn expand(text: &str) -> String {
    let text = text.trim().trim_matches('"');
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let text = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['\\', '/']) => format!("{home}{rest}"),
        _ => text.to_string(),
    };
    let mut out = String::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let name = &after[..end];
        match std::env::var(name) {
            Ok(value) if !name.is_empty() => out.push_str(&value),
            _ => {
                out.push('%');
                out.push_str(name);
                out.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_urls() {
        assert_eq!(
            as_url(" https://example.com/a "),
            Some("https://example.com/a".into())
        );
        assert_eq!(
            as_url("www.example.com"),
            Some("https://www.example.com".into())
        );
        assert_eq!(as_url("www."), None);
        assert_eq!(as_url("http is a protocol"), None);
        assert_eq!(as_url("notepad"), None);
    }

    #[test]
    fn expands_home_and_variables() {
        let home = std::env::var("USERPROFILE").unwrap();
        let windir = std::env::var("WINDIR").unwrap();
        assert_eq!(expand(r"~\Documents"), format!(r"{home}\Documents"));
        assert_eq!(expand(r"%WINDIR%\System32"), format!(r"{windir}\System32"));
        assert_eq!(expand("%NO_SUCH_VAR_SEEKR%"), "%NO_SUCH_VAR_SEEKR%");
        assert_eq!(expand("100% sure"), "100% sure");
        assert_eq!(expand("~user"), "~user");
    }

    #[test]
    fn offers_existing_absolute_paths_only() {
        let windir = std::env::var("WINDIR").unwrap();
        let hit = items("%WINDIR%");
        assert_eq!(hit[0].title, windir);
        assert_eq!(hit[0].subtitle, "Folder");
        assert!(items(r"C:\definitely\not\here\seekr").is_empty());
        assert!(items("Windows").is_empty());
        assert_eq!(items(r"\\server\share")[0].subtitle, "Network path");
    }
}

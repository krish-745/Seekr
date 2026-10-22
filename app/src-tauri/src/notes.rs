//! Notes and to-dos, kept as Markdown in `Documents\Seekr Notes` (which content search always
//! reads, so they are searchable by words and meaning). `note <text>` saves a note and `note`
//! lists recent ones, which can be edited in the preview pane; `todo <task> [due]` adds to
//! `To-dos.md` and `todo` shows the checklist.

use std::{
    fs, io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use chrono::{Datelike, Local, NaiveDate, Weekday};
use tauri::AppHandle;

use crate::{
    calc, paste,
    query::{Action, Item},
};

const TODOS: &str = "To-dos.md";
const NOTES_SHOWN: usize = 30;
const DONE_SHOWN: usize = 10;
const TITLE_WORDS: usize = 6;

const SAVE_NOTE: Action = Action {
    id: "noteadd",
    label: "Save note",
};
const ADD_TODO: Action = Action {
    id: "todoadd",
    label: "Add to-do",
};
const OPEN: Action = Action {
    id: "open",
    label: "Open",
};
/// The panel opens its editor for this one.
const EDIT: Action = Action {
    id: "editnote",
    label: "Edit here",
};
const REVEAL: Action = Action {
    id: "reveal",
    label: "Show in folder",
};
const DONE: Action = Action {
    id: "tododone",
    label: "Mark done",
};
const UNDO: Action = Action {
    id: "todoundo",
    label: "Mark not done",
};
const DELETE: Action = Action {
    id: "tododelete",
    label: "Delete",
};
const OPEN_LIST: Action = Action {
    id: "todofile",
    label: "Open To-dos.md",
};

fn dir() -> Result<PathBuf, String> {
    common::content::notes_dir().ok_or_else(|| "no Documents folder".to_string())
}

/// `note` or `note <text>`.
pub fn note_mode<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    word_mode(text, word)
}

/// `todo` or `todo <task>`.
pub fn todo_mode<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    word_mode(text, word)
}

fn word_mode<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.trim_start().strip_prefix(word)?;
    (rest.is_empty() || rest.starts_with(' ')).then(|| rest.trim())
}

// ---- notes ----

pub fn note_items(text: &str) -> Vec<Item> {
    let mut items = Vec::new();
    if !text.is_empty() {
        items.push(Item {
            id: "note:new".into(),
            title: format!("Save note: {}", first_line(text)),
            subtitle: format!(
                "Into {}",
                dir().map(|d| d.display().to_string()).unwrap_or_default()
            ),
            preview: text.to_string(),
            icon: None,
            line: None,
            actions: vec![SAVE_NOTE],
        });
    }
    let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
    let now = SystemTime::now();
    for (path, modified) in recent_notes() {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Typing a new note filters the list to notes it might duplicate.
        let lower = name.to_lowercase();
        if !text.is_empty() && !words.iter().any(|w| lower.contains(w.as_str())) {
            continue;
        }
        let age = now.duration_since(modified).map_or(0, |d| d.as_secs());
        items.push(Item {
            id: format!("path:{}", path.display()),
            title: name,
            subtitle: format!("Note · edited {}", ago(age)),
            preview: String::new(),
            icon: None,
            line: None,
            actions: vec![OPEN, EDIT, REVEAL],
        });
        if items.len() > NOTES_SHOWN {
            break;
        }
    }
    items
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").chars().take(80).collect()
}

fn ago(seconds: u64) -> String {
    match seconds {
        ..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86_400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86_400),
    }
}

/// Markdown notes other than the to-do list, newest first.
fn recent_notes() -> Vec<(PathBuf, SystemTime)> {
    let Ok(entries) = dir().and_then(|d| fs::read_dir(d).map_err(|e| e.to_string())) else {
        return Vec::new();
    };
    let mut notes: Vec<(PathBuf, SystemTime)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".md") && name != TODOS
        })
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .collect();
    notes.sort_by_key(|n| std::cmp::Reverse(n.1));
    notes
}

/// A note's file name without ".md": its date and first words, minus what Windows forbids in
/// names ("2026-10-22 call the bank about").
fn note_stem(text: &str, today: NaiveDate) -> String {
    let words: Vec<String> = first_line(text)
        .split_whitespace()
        .take(TITLE_WORDS)
        .map(|w| w.chars().filter(|c| !r#"<>:"/\|?*"#.contains(*c)).collect())
        .filter(|w: &String| !w.is_empty())
        .collect();
    let stem = format!("{} {}", today.format("%Y-%m-%d"), words.join(" "));
    stem.trim_end_matches(['.', ' ']).to_string()
}

/// Saves a new note, numbering the name rather than overwriting another.
fn save_note(text: &str, today: NaiveDate) -> Result<PathBuf, String> {
    let dir = dir()?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = note_stem(text, today);
    let mut path = dir.join(format!("{stem}.md"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}).md"));
        n += 1;
    }
    fs::write(&path, format!("{}\n", text.trim_end())).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Only a Markdown file directly inside the notes folder may be read or saved from the panel.
fn checked(path: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(path);
    let inside = path
        .parent()
        .map(fs::canonicalize)
        .transpose()
        .ok()
        .flatten()
        == fs::canonicalize(dir()?).ok();
    if !inside || path.extension().is_none_or(|e| e != "md") {
        return Err("only notes in the notes folder can be edited here".into());
    }
    Ok(path)
}

#[tauri::command]
pub fn read_note(path: String) -> Result<String, String> {
    fs::read_to_string(checked(&path)?).map_err(|e| e.to_string())
}

/// Written beside the note and renamed over it, so a failed save never leaves half a note.
#[tauri::command]
pub fn save_note_text(path: String, text: String) -> Result<(), String> {
    let path = checked(&path)?;
    write_whole(&path, &text).map_err(|e| e.to_string())
}

fn write_whole(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("md.tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, path)
}

// ---- to-dos ----

#[derive(Debug, PartialEq)]
struct Todo {
    task: String,
    due: Option<NaiveDate>,
    done: bool,
    /// The line after its checkbox, which identifies it in the file.
    line: String,
}

fn todos_path() -> Result<PathBuf, String> {
    Ok(dir()?.join(TODOS))
}

fn parse_todos(text: &str) -> Vec<Todo> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (done, line) = match (l.strip_prefix("- [ ] "), l.strip_prefix("- [x] ")) {
                (Some(rest), _) => (false, rest),
                (_, Some(rest)) => (true, rest),
                _ => return None,
            };
            let (task, due) = match line.rsplit_once(" (due ") {
                Some((task, date)) => {
                    match NaiveDate::parse_from_str(date.trim_end_matches(')'), "%Y-%m-%d") {
                        Ok(due) => (task, Some(due)),
                        Err(_) => (line, None),
                    }
                }
                None => (line, None),
            };
            Some(Todo {
                task: task.to_string(),
                due,
                done,
                line: line.to_string(),
            })
        })
        .collect()
}

/// "call the bank by friday" is the task "call the bank", due on the coming Friday. Dates are
/// anything the calculator reads (tomorrow, 25 dec, 2026-11-02) or a weekday.
fn split_due(text: &str, today: NaiveDate) -> (String, Option<NaiveDate>) {
    let words: Vec<&str> = text.split_whitespace().collect();
    for take in (1..=3).rev() {
        if words.len() <= take {
            continue;
        }
        let (task, tail) = words.split_at(words.len() - take);
        let mut tail = tail.to_vec();
        let mut task = task.to_vec();
        if matches!(tail[0].to_lowercase().as_str(), "by" | "due" | "on") {
            tail.remove(0);
        } else if let Some(&last) = task.last()
            && matches!(last.to_lowercase().as_str(), "by" | "due" | "on")
        {
            task.pop();
        }
        let phrase = tail.join(" ").to_lowercase();
        if let Some(due) = calc::date(&phrase, today).or_else(|| weekday(&phrase, today))
            && !task.is_empty()
        {
            return (task.join(" "), Some(due));
        }
    }
    (text.trim().to_string(), None)
}

/// The next such weekday after today: "friday", "fri".
fn weekday(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let day: Weekday = text.parse().ok()?;
    let ahead = (7 + day.num_days_from_monday() - today.weekday().num_days_from_monday()) % 7;
    today.checked_add_days(chrono::Days::new(u64::from(if ahead == 0 {
        7
    } else {
        ahead
    })))
}

fn due_text(due: Option<NaiveDate>, today: NaiveDate) -> String {
    let Some(due) = due else {
        return "No due date".into();
    };
    match (due - today).num_days() {
        0 => "Due today".into(),
        1 => "Due tomorrow".into(),
        n if n < 0 => format!("Overdue · was due {}", due.format("%a %-d %b")),
        _ => format!("Due {}", due.format("%a %-d %b")),
    }
}

pub fn todo_items(text: &str) -> Vec<Item> {
    let today = Local::now().date_naive();
    let mut items = Vec::new();
    if !text.is_empty() {
        let (task, due) = split_due(text, today);
        items.push(Item {
            id: "todo:new".into(),
            title: format!("Add to-do: {task}"),
            subtitle: due_text(due, today),
            preview: String::new(),
            icon: None,
            line: None,
            actions: vec![ADD_TODO],
        });
    }
    let all = todos_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|t| parse_todos(&t))
        .unwrap_or_default();
    let (mut open, done): (Vec<&Todo>, Vec<&Todo>) = all.iter().partition(|t| !t.done);
    // Dated ones first, soonest (or most overdue) first; then the rest as written.
    open.sort_by_key(|t| t.due.unwrap_or(NaiveDate::MAX));
    let recent_done = done.iter().rev().take(DONE_SHOWN).rev();
    for t in open.into_iter().chain(recent_done.copied()) {
        items.push(Item {
            id: format!("todo:{}", t.line),
            title: t.task.clone(),
            subtitle: if t.done {
                "Done".into()
            } else {
                due_text(t.due, today)
            },
            preview: String::new(),
            icon: None,
            line: None,
            // Delete stays off Ctrl+Enter, where it would be too easy to hit.
            actions: if t.done {
                vec![UNDO, OPEN_LIST, DELETE]
            } else {
                vec![DONE, OPEN_LIST, DELETE]
            },
        });
    }
    items
}

/// The to-do list with one line changed: `edit` gets the line after its checkbox and returns the
/// new line (with checkbox), or None to delete it.
fn change_todo(text: &str, line: &str, edit: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut found = false;
    let mut out = Vec::new();
    for l in text.lines() {
        let rest = l
            .trim_start()
            .strip_prefix("- [ ] ")
            .or_else(|| l.trim_start().strip_prefix("- [x] "));
        if !found && rest == Some(line) {
            found = true;
            out.extend(edit(l.trim_start()));
        } else {
            out.push(l.to_string());
        }
    }
    found.then(|| out.join("\n") + "\n")
}

fn add_todo(text: &str, today: NaiveDate) -> Result<(), String> {
    let (task, due) = split_due(text, today);
    let path = todos_path()?;
    fs::create_dir_all(dir()?).map_err(|e| e.to_string())?;
    let mut list = fs::read_to_string(&path).unwrap_or_else(|_| "# To-dos\n\n".into());
    if !list.ends_with('\n') {
        list.push('\n');
    }
    let due = due
        .map(|d| format!(" (due {})", d.format("%Y-%m-%d")))
        .unwrap_or_default();
    list.push_str(&format!("- [ ] {task}{due}\n"));
    write_whole(&path, &list).map_err(|e| e.to_string())
}

pub fn run(app: &AppHandle, id: &str, action: &str, query: &str) -> Result<(), String> {
    let today = Local::now().date_naive();
    let prefixes = &crate::settings::get().prefixes;
    match (id, action) {
        ("new", "noteadd") => {
            let text = note_mode(query, &prefixes.note).unwrap_or("");
            if text.is_empty() {
                return Err("nothing to save".into());
            }
            save_note(text, today)?;
            crate::hide_panel(app, true);
            paste::toast("Note saved");
        }
        ("new", "todoadd") => add_todo(todo_mode(query, &prefixes.todo).unwrap_or(""), today)?,
        (_, "todofile") => {
            crate::hide_panel(app, false);
            tauri_plugin_opener::OpenerExt::opener(app)
                .open_path(todos_path()?.display().to_string(), None::<&str>)
                .map_err(|e| e.to_string())?;
        }
        (line, "tododone" | "todoundo" | "tododelete") => {
            let path = todos_path()?;
            let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let changed = change_todo(&text, line, |l| match action {
                "tododone" => Some(l.replacen("- [ ] ", "- [x] ", 1)),
                "todoundo" => Some(l.replacen("- [x] ", "- [ ] ", 1)),
                _ => None,
            })
            .ok_or("that to-do changed in the file; showing it again")?;
            write_whole(&path, &changed).map_err(|e| e.to_string())?;
        }
        _ => return Err(format!("unknown action {action}")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn modes() {
        assert_eq!(note_mode("note", "note"), Some(""));
        assert_eq!(note_mode("note buy milk", "note"), Some("buy milk"));
        assert_eq!(note_mode("notepad", "note"), None);
        assert_eq!(todo_mode("todo ", "todo"), Some(""));
        assert_eq!(todo_mode("todos", "todo"), None);
        assert_eq!(todo_mode("t call", "t"), Some("call"));
    }

    #[test]
    fn due_dates_come_off_the_end() {
        // 22 October 2026 is a Thursday.
        let today = day(2026, 10, 22);
        assert_eq!(
            split_due("call the bank by friday", today),
            ("call the bank".into(), Some(day(2026, 10, 23)))
        );
        assert_eq!(
            split_due("pay rent tomorrow", today),
            ("pay rent".into(), Some(day(2026, 10, 23)))
        );
        assert_eq!(
            split_due("dentist on thu", today),
            ("dentist".into(), Some(day(2026, 10, 29)))
        );
        assert_eq!(
            split_due("submit form due 2 nov", today),
            ("submit form".into(), Some(day(2026, 11, 2)))
        );
        assert_eq!(
            split_due("renew passport 2027-01-15", today),
            ("renew passport".into(), Some(day(2027, 1, 15)))
        );
        assert_eq!(
            split_due("water plants", today),
            ("water plants".into(), None)
        );
        assert_eq!(
            split_due("tomorrow", today),
            ("tomorrow".into(), None),
            "a task needs words"
        );
        assert_eq!(
            due_text(Some(day(2026, 10, 20)), today),
            "Overdue · was due Tue 20 Oct"
        );
    }

    #[test]
    fn checklist_round_trip() {
        let text = "# To-dos\n\n- [ ] call bank (due 2026-10-23)\n- [x] pay rent\nplain line\n- [ ] water plants\n";
        let todos = parse_todos(text);
        assert_eq!(todos.len(), 3);
        assert_eq!(todos[0].task, "call bank");
        assert_eq!(todos[0].due, Some(day(2026, 10, 23)));
        assert!(todos[1].done);
        let done = change_todo(text, "call bank (due 2026-10-23)", |l| {
            Some(l.replacen("- [ ] ", "- [x] ", 1))
        })
        .unwrap();
        assert!(done.contains("- [x] call bank (due 2026-10-23)"));
        assert!(done.contains("plain line"));
        let deleted = change_todo(&done, "water plants", |_| None).unwrap();
        assert!(!deleted.contains("water plants"));
        assert!(change_todo(text, "not there", |_| None).is_none());
    }

    #[test]
    fn notes_are_named_and_never_overwritten() {
        let dir = std::env::temp_dir().join(format!("seekr-notes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let today = day(2026, 10, 22);
        assert_eq!(
            note_stem(
                "Ideas: launcher/themes? and more words here\nsecond line",
                today
            ),
            "2026-10-22 Ideas launcherthemes and more words here"
        );
        assert_eq!(
            note_stem("end with dots...", today),
            "2026-10-22 end with dots"
        );
        let note = dir.join("n.md");
        write_whole(&note, "one").unwrap();
        write_whole(&note, "two").unwrap();
        assert_eq!(fs::read_to_string(&note).unwrap(), "two");
        assert!(!dir.join("n.md.tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}

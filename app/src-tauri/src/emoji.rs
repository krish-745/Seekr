//! Emoji and symbols: `:` or `emoji ` and words ("emoji party", ":rupee") lists matches by name,
//! CLDR keyword and GitHub shortcode. Enter pastes into the window you were in (or copies, for an
//! admin window), Ctrl+Enter copies, and emoji that take skin tones offer them as actions.

use std::{collections::HashMap, sync::OnceLock};

use emojis::{SkinTone, UnicodeVersion};
use tauri::AppHandle;

use crate::{
    clipboard, paste,
    query::{Action, Item},
};

/// Names and keywords from Unicode CLDR, generated from its annotations (see the file's header).
const CLDR: &str = include_str!("../data/cldr-annotations.txt");
/// shortcut: Windows 11's emoji font covers Unicode 16; newer emoji would show as boxes. Raise this
/// when Windows ships a newer Segoe UI Emoji.
const NEWEST: UnicodeVersion = UnicodeVersion::new(16, 0);
const SHOWN: usize = 60;
/// Symbols CLDR doesn't annotate: (text, name, keywords).
const EXTRA: &[(&str, &str, &str)] = &[
    ("½", "one half", "fraction half"),
    ("⅓", "one third", "fraction third"),
    ("⅔", "two thirds", "fraction third"),
    ("¼", "one quarter", "fraction quarter"),
    ("¾", "three quarters", "fraction quarter"),
    ("⅛", "one eighth", "fraction eighth"),
    ("✓", "check mark", "tick done yes"),
    ("✔", "heavy check mark", "tick done yes"),
    ("✗", "ballot x", "cross no wrong"),
    ("✘", "heavy ballot x", "cross no wrong"),
    ("★", "black star", "star rating"),
    ("☆", "white star", "star rating"),
    ("♥", "heart suit", "heart cards"),
    ("☐", "ballot box", "checkbox empty"),
    ("☑", "ballot box with check", "checkbox ticked"),
    ("⌘", "command key", "mac place of interest"),
    ("⌥", "option key", "mac alt"),
    ("⇧", "shift key", "upwards white arrow"),
    ("⏎", "return symbol", "enter key"),
];
const GREEK: &str = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi \
    omicron pi rho sigma tau upsilon phi chi psi omega";

const PASTE: Action = Action {
    id: "paste",
    label: "Paste",
};
const COPY: Action = Action {
    id: "copy",
    label: "Copy",
};
const TONES: [(SkinTone, Action); 5] = [
    (
        SkinTone::Light,
        Action {
            id: "tone1",
            label: "Paste with light skin tone",
        },
    ),
    (
        SkinTone::MediumLight,
        Action {
            id: "tone2",
            label: "Paste with medium-light skin tone",
        },
    ),
    (
        SkinTone::Medium,
        Action {
            id: "tone3",
            label: "Paste with medium skin tone",
        },
    ),
    (
        SkinTone::MediumDark,
        Action {
            id: "tone4",
            label: "Paste with medium-dark skin tone",
        },
    ),
    (
        SkinTone::Dark,
        Action {
            id: "tone5",
            label: "Paste with dark skin tone",
        },
    ),
];

struct Glyph {
    text: String,
    name: String,
    /// What it is, for the subtitle: the emoji group, or "Symbol".
    kind: String,
    /// Lower-case words of its name, keywords and shortcodes.
    words: Vec<String>,
    tones: bool,
}

/// `:words` or `emoji words`.
/// `:words` or `<prefix> words`: `word` is the emoji prefix from config.toml.
pub fn mode<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let text = text.trim_start();
    if let Some(rest) = text.strip_prefix(':') {
        return Some(rest.trim());
    }
    let rest = text.strip_prefix(word)?;
    rest.starts_with(' ').then(|| rest.trim())
}

fn glyphs() -> &'static [Glyph] {
    static ALL: OnceLock<Vec<Glyph>> = OnceLock::new();
    ALL.get_or_init(|| {
        // In file order, so symbols list the same way every time.
        let entries: Vec<(&str, (&str, &str))> = CLDR
            .lines()
            .filter(|l| !l.starts_with('#'))
            .filter_map(|l| {
                let mut parts = l.split('\t');
                Some((parts.next()?, (parts.next()?, parts.next().unwrap_or(""))))
            })
            .collect();
        // Keyed without variation selectors: emoji data and CLDR place U+FE0F differently.
        let cldr: HashMap<String, (&str, &str)> = entries
            .iter()
            .map(|&(text, names)| (plain(text), names))
            .collect();
        let split = |s: &str| -> Vec<String> {
            s.split(|c: char| !c.is_alphanumeric() && c != '+')
                .filter(|w| !w.is_empty())
                .map(str::to_lowercase)
                .collect()
        };
        let mut all = Vec::new();
        for e in emojis::iter().filter(|e| e.unicode_version() <= NEWEST) {
            let keywords = cldr.get(&plain(e.as_str())).map_or("", |&(_, k)| k);
            let mut words = split(e.name());
            words.extend(split(keywords));
            words.extend(e.shortcodes().flat_map(split));
            all.push(Glyph {
                text: e.as_str().to_string(),
                name: e.name().to_string(),
                kind: group(e.group()).to_string(),
                words,
                tones: e.skin_tones().is_some(),
            });
        }
        let is_emoji: std::collections::HashSet<String> =
            emojis::iter().map(|e| plain(e.as_str())).collect();
        for (text, (name, keywords)) in &entries {
            if is_emoji.contains(&plain(text)) {
                continue;
            }
            let mut words = split(name);
            words.extend(split(keywords));
            all.push(symbol(text, name, words));
        }
        for (text, name, keywords) in EXTRA {
            let mut words = split(name);
            words.extend(split(keywords));
            all.push(symbol(text, name, words));
        }
        for (i, letter) in GREEK.split_whitespace().enumerate() {
            let lower = char::from_u32(0x3B1 + i as u32 + u32::from(i >= 17)).unwrap_or(' ');
            for (case, c) in [
                ("small", lower),
                ("capital", lower.to_uppercase().next().unwrap_or(lower)),
            ] {
                let name = format!("greek {case} letter {letter}");
                let mut words = split(&name);
                words.push("greek".into());
                all.push(symbol(&c.to_string(), &name, words));
            }
        }
        all
    })
}

fn group(group: emojis::Group) -> &'static str {
    use emojis::Group::*;
    match group {
        SmileysAndEmotion => "Smileys & emotion",
        PeopleAndBody => "People & body",
        AnimalsAndNature => "Animals & nature",
        FoodAndDrink => "Food & drink",
        TravelAndPlaces => "Travel & places",
        Activities => "Activities",
        Objects => "Objects",
        Symbols => "Symbols",
        Flags => "Flags",
    }
}

fn plain(text: &str) -> String {
    text.replace('\u{fe0f}', "")
}

fn symbol(text: &str, name: &str, words: Vec<String>) -> Glyph {
    Glyph {
        text: text.to_string(),
        name: name.to_string(),
        kind: "Symbol".into(),
        words,
        tones: false,
    }
}

/// Glyphs where every typed word starts one of its words; the exact name, then whole-word hits
/// on the name, rank first.
pub fn items(filter: &str) -> Vec<Item> {
    let typed: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    let mut found: Vec<(u32, usize, &Glyph)> = glyphs()
        .iter()
        .enumerate()
        .filter_map(|(i, g)| {
            let mut score = 0;
            for t in &typed {
                let exact = g.words.iter().any(|w| w == t);
                if !exact && !g.words.iter().any(|w| w.starts_with(t.as_str())) {
                    return None;
                }
                score += u32::from(exact);
                if g.name.to_lowercase().split(' ').any(|w| w == t) {
                    score += 2;
                }
            }
            if g.name.to_lowercase() == typed.join(" ") {
                score += 4;
            }
            Some((score, i, g))
        })
        .collect();
    // Ties go to the shorter name ("fire" before "heart on fire"), then CLDR order.
    let length = |g: &Glyph| g.name.split(' ').count();
    found.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(length(a.2).cmp(&length(b.2)))
            .then(a.1.cmp(&b.1))
    });
    found
        .into_iter()
        .take(SHOWN)
        .map(|(_, _, g)| {
            let mut actions = vec![PASTE, COPY];
            if g.tones {
                actions.extend(TONES.iter().map(|(_, a)| a.clone()));
            }
            Item {
                id: format!("emoji:{}", g.text),
                title: format!("{}   {}", g.text, g.name),
                subtitle: g.kind.clone(),
                preview: format!("{}\n\n{}\nU+{}", g.text, g.name, codepoints(&g.text)),
                icon: None,
                line: None,
                actions,
            }
        })
        .collect()
}

fn codepoints(text: &str) -> String {
    text.chars()
        .map(|c| format!("{:04X}", c as u32))
        .collect::<Vec<_>>()
        .join(" U+")
}

pub fn run(app: &AppHandle, text: &str, action: &str) -> Result<(), String> {
    let text = match TONES.iter().find(|(_, a)| a.id == action) {
        Some((tone, _)) => emojis::get(text)
            .and_then(|e| e.with_skin_tone(*tone))
            .map_or(text, |e| e.as_str()),
        None => text,
    };
    let owner = crate::panel_hwnd(app).ok_or("panel window is gone")?;
    clipboard::set_text(owner, text)?;
    if action == "copy" {
        crate::hide_panel(app, true);
    } else {
        paste::into_previous(app);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(filter: &str) -> String {
        items(filter)
            .first()
            .map(|i| i.id.trim_start_matches("emoji:").to_string())
            .unwrap_or_default()
    }

    #[test]
    fn finds_emoji_and_symbols_by_name_keyword_and_shortcode() {
        assert_eq!(mode(":fire", "emoji"), Some("fire"));
        assert_eq!(mode("emoji party popper", "emoji"), Some("party popper"));
        assert_eq!(mode("emojis", "emoji"), None);
        assert_eq!(first("fire"), "🔥");
        let hearts = items("heart on fire");
        assert_eq!(
            hearts
                .iter()
                .filter(|i| i.title.ends_with("heart on fire"))
                .count(),
            1
        );
        assert_eq!(first("joy"), "😂", "shortcode");
        assert!(
            items("laugh").iter().any(|i| i.id == "emoji:😂"),
            "CLDR keyword"
        );
        assert_eq!(first("thumbs up"), "👍");
        assert_eq!(first("indian rupee"), "₹");
        assert!(items("rupee")[..2].iter().any(|i| i.id == "emoji:₹"));
        assert_eq!(first("rightwards double arrow"), "⇒");
        assert_eq!(first("infinity"), "♾️");
        assert!(items("infinity").iter().any(|i| i.id == "emoji:∞"));
        assert_eq!(first("greek small pi"), "π");
        assert_eq!(first("omega capital"), "Ω");
        assert_eq!(first("half"), "½");
        assert!(items("zzqqxx").is_empty());
        assert_eq!(items("").len(), SHOWN);
    }

    #[test]
    fn skin_tones_only_where_they_apply() {
        let wave = items("waving hand");
        assert!(wave[0].actions.iter().any(|a| a.id == "tone3"));
        let tone = emojis::get("👋")
            .unwrap()
            .with_skin_tone(SkinTone::Medium)
            .unwrap();
        assert_eq!(tone.as_str(), "👋🏽");
        assert!(!items("rupee")[0].actions.iter().any(|a| a.id == "tone1"));
    }
}

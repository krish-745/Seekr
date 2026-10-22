//! Splits extracted text into passages of a few hundred words, each remembering the line it starts
//! on so a result can open the preview there. Breaks fall on blank lines (paragraphs, and the gaps
//! between functions in code) and before Markdown headings, so a passage stays on one topic.

/// Aim for passages about this long, in bytes.
const TARGET: usize = 1200;
/// Never let a passage grow past this; a long run with no blank line breaks at any line.
const MAX: usize = 2400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub text: String,
    /// 1-based line the passage starts on.
    pub line: u32,
}

pub fn chunks(text: &str, markdown: bool) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut start = 1u32;
    let mut flush = |current: &mut String, start: u32| {
        if !current.trim().is_empty() {
            out.push(Chunk {
                text: std::mem::take(current),
                line: start,
            });
        }
        current.clear();
    };
    for (n, line) in text.lines().enumerate() {
        let number = n as u32 + 1;
        let heading = markdown && line.starts_with('#');
        let blank = line.trim().is_empty();
        if !current.is_empty()
            && ((heading && current.len() > TARGET / 4)
                || (blank && current.len() >= TARGET)
                || current.len() + line.len() > MAX)
        {
            flush(&mut current, start);
        }
        if current.is_empty() {
            if blank {
                continue;
            }
            start = number;
        }
        // Minified files put megabytes on one line; cut it into passages of its own.
        let mut rest = line;
        while rest.len() > MAX {
            let cut = (0..=MAX)
                .rev()
                .find(|&i| rest.is_char_boundary(i))
                .unwrap_or(0);
            current.push_str(&rest[..cut]);
            flush(&mut current, number);
            start = number;
            rest = &rest[cut..];
        }
        current.push_str(rest);
        current.push('\n');
    }
    flush(&mut current, start);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breaks_at_paragraphs_once_long_enough() {
        let para = "word ".repeat(300); // 1500 bytes
        let text = format!("{para}\n\n{para}\n\nshort\n");
        let parts = chunks(&text, false);
        let lines: Vec<u32> = parts.iter().map(|c| c.line).collect();
        assert_eq!(lines, [1, 3, 5], "each long paragraph ends a passage");
        assert_eq!(parts[2].text, "short\n");
        let small = chunks("one\n\ntwo\n", false);
        assert_eq!(small.len(), 1, "short paragraphs share a passage");
    }

    #[test]
    fn markdown_headings_start_passages() {
        let body = "text ".repeat(80); // 400 bytes
        let text = format!("# One\n{body}\n# Two\n{body}\n");
        let parts = chunks(&text, true);
        assert_eq!(parts.len(), 2);
        assert!(parts[1].text.starts_with("# Two"));
        assert_eq!(parts[1].line, 3);
    }

    #[test]
    fn cuts_huge_lines_and_skips_blank_text() {
        let line = "x".repeat(MAX * 2 + 10);
        let parts = chunks(&line, false);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|c| c.text.len() <= MAX + 1 && c.line == 1));
        assert!(chunks("\n\n  \n", false).is_empty());
    }
}

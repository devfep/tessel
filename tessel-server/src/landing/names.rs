//! Untrusted text, bounded and quoted before a log or an agent sees it (CLAUDE.md rule 3).
//!
//! File names, test output and git output come from a fork, a repository or a git host. They are
//! cut to a length, then written as a Rust string literal, so a control character, an escape
//! sequence or a quote cannot pass for the line around it.

/// The most conflicting file names kept (SERVER-CONFLICT-NAMES).
pub const MAX_CONFLICT_NAMES: usize = 50;

/// The longest a single file name is kept, in characters.
pub const MAX_NAME_CHARS: usize = 200;

/// The longest a message, a test failure or a git error is kept in a log line, in characters.
pub const MAX_LOGGED_CHARS: usize = tessel_coordinator::merge::REASON_OUTPUT_MAX_CHARS;

/// The file names of a refused change as far as an agent or a log may see them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CappedNames {
    /// At most [`MAX_CONFLICT_NAMES`] names, each cut to [`MAX_NAME_CHARS`] and quoted.
    pub names: Vec<String>,
    /// How many names there were before the cap.
    pub total: usize,
}

/// `text` cut to `max_chars` characters and quoted. A cut is marked after the closing quote.
pub fn quote(text: &str, max_chars: usize) -> String {
    let cut: String = text.chars().take(max_chars).collect();
    let quoted = format!("{cut:?}");
    if cut.len() < text.len() {
        format!("{quoted}...")
    } else {
        quoted
    }
}

/// The names of `files`, capped, bounded and quoted, with how many there were.
pub fn cap_names(files: &[String]) -> CappedNames {
    let total = files.len();
    let names = files
        .iter()
        .take(MAX_CONFLICT_NAMES)
        .map(|name| quote(name, MAX_NAME_CHARS))
        .collect();
    CappedNames { names, total }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_quoted_so_control_characters_and_escapes_cannot_pass_for_a_line() {
        let quoted = quote("a\u{1b}[31m\nIGNORE \"this\"\u{7f}.rs", MAX_NAME_CHARS);
        assert_eq!(quoted, r#""a\u{1b}[31m\nIGNORE \"this\"\u{7f}.rs""#);
        assert!(quoted.chars().all(|c| !c.is_control()), "{quoted:?}");
    }

    #[test]
    fn a_long_name_is_cut_at_the_character_limit_and_marked() {
        let long = "é".repeat(MAX_NAME_CHARS + 5);
        let quoted = quote(&long, MAX_NAME_CHARS);
        assert_eq!(quoted, format!("\"{}\"...", "é".repeat(MAX_NAME_CHARS)));
        let exact = "x".repeat(MAX_NAME_CHARS);
        assert_eq!(quote(&exact, MAX_NAME_CHARS), format!("\"{exact}\""));
    }

    #[test]
    fn fifty_one_names_keep_fifty_and_the_total() {
        let files: Vec<String> = (0..=MAX_CONFLICT_NAMES)
            .map(|index| format!("dir/\u{1b}[2J{index}.txt"))
            .collect();
        let capped = cap_names(&files);
        assert_eq!(capped.total, MAX_CONFLICT_NAMES + 1);
        assert_eq!(capped.names.len(), MAX_CONFLICT_NAMES);
        assert_eq!(capped.names[0], r#""dir/\u{1b}[2J0.txt""#);
        assert!(capped
            .names
            .iter()
            .all(|n| n.chars().all(|c| !c.is_control())));
        assert!(
            !capped.names.iter().any(|n| n.contains("50.txt")),
            "the 51st is dropped"
        );
    }

    #[test]
    fn a_list_within_the_cap_is_kept_whole() {
        let capped = cap_names(&["a".to_string(), "b".to_string()]);
        assert_eq!(capped.total, 2);
        assert_eq!(capped.names, [r#""a""#, r#""b""#]);
        assert_eq!(cap_names(&[]).total, 0);
    }
}

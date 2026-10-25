//! The first failing test, quoted from cargo's output.
//!
//! Output is untrusted data (CLAUDE.md rule 3). This only picks a bounded excerpt for a person or
//! an agent to read, as quoted data. It is never an input to pass or fail.

/// The most bytes of the excerpt.
pub const MAX_FIRST_FAILURE_BYTES: usize = 1500;

/// The first failing test's block, from its `---- <name> stdout ----` line to the next such line or
/// the `failures:` line, without the blank lines that end it. Real output has a blank line right
/// after the header, so the block cannot end at the first blank line.
/// Without a block, the first line that says `FAILED` or `panicked`, or that starts `Diff in `
/// (`cargo fmt --check` names the file it flagged that way). Standard output is searched
/// before standard error. `None` when neither holds anything.
pub fn first_failure(stdout: &str, stderr: &str) -> Option<String> {
    let found = block(stdout)
        .or_else(|| block(stderr))
        .or_else(|| marker_line(stdout))
        .or_else(|| marker_line(stderr))?;
    Some(bounded(found))
}

fn is_header(line: &str) -> bool {
    line.starts_with("---- ") && line.ends_with(" stdout ----")
}

fn block(text: &str) -> Option<String> {
    let mut lines = text.lines().skip_while(|line| !is_header(line)).peekable();
    lines.peek()?;
    let mut kept: Vec<&str> = vec![];
    for (index, line) in lines.enumerate() {
        if index > 0 && (is_header(line) || line == "failures:") {
            break;
        }
        kept.push(line);
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    Some(kept.join("\n"))
}

fn marker_line(text: &str) -> Option<String> {
    text.lines()
        .find(|line| {
            line.contains("FAILED") || line.contains("panicked") || line.starts_with("Diff in ")
        })
        .map(str::to_owned)
}

/// At most [`MAX_FIRST_FAILURE_BYTES`], cut on a character boundary.
fn bounded(mut text: String) -> String {
    if text.len() > MAX_FIRST_FAILURE_BYTES {
        let mut end = MAX_FIRST_FAILURE_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured once from `cargo test` 1.99.1 on a crate with two failing tests, the second of
    /// which prints before it panics.
    const TWO_FAILURES: &str = include_str!("../tests/fixtures/cargo-1.99.1-test-failures.stdout");

    #[test]
    fn the_first_failing_tests_block_is_quoted_and_the_second_is_not() {
        let quoted = first_failure(TWO_FAILURES, "").unwrap();
        assert!(
            quoted.starts_with("---- first_fails stdout ----\n\nthread 'first_fails'"),
            "{quoted}"
        );
        assert!(quoted.ends_with("  left: 2\n right: 3"), "{quoted}");
        assert!(!quoted.contains("second_fails"));
    }

    #[test]
    fn the_last_block_ends_at_the_failures_line() {
        let quoted =
            first_failure(&TWO_FAILURES.replacen("---- first_fails", "-- x", 1), "").unwrap();
        assert!(quoted.starts_with("---- second_fails stdout ----\nprinted by the test"));
        assert!(quoted.contains("second boom\nnote: run with"), "{quoted}");
        assert!(quoted.ends_with("to display a backtrace"), "{quoted}");
    }

    #[test]
    fn without_a_block_the_first_failed_or_panicked_line_is_quoted() {
        let quoted = first_failure("ok\nsomething panicked here\nFAILED later\n", "").unwrap();
        assert_eq!(quoted, "something panicked here");
        assert_eq!(first_failure("", "error: FAILED").unwrap(), "error: FAILED");
    }

    #[test]
    fn a_fmt_diff_header_is_the_first_failure() {
        let fmt =
            "Diff in /tree/src/lib.rs:3:\n-let x=1;\n+let x = 1;\nDiff in /tree/src/b.rs:9:\n";
        assert_eq!(
            first_failure(fmt, "").unwrap(),
            "Diff in /tree/src/lib.rs:3:"
        );
        assert_eq!(
            first_failure("", fmt).unwrap(),
            "Diff in /tree/src/lib.rs:3:"
        );
        assert_eq!(first_failure("a Diff in b\n", ""), None);
    }

    #[test]
    fn a_clean_output_has_no_first_failure() {
        assert_eq!(
            first_failure("test result: ok. 3 passed\n", "warning\n"),
            None
        );
    }

    #[test]
    fn a_long_block_is_cut_at_1500_bytes_on_a_character_boundary() {
        let body = "é".repeat(2000);
        let quoted = first_failure(&format!("---- a stdout ----\n{body}\n"), "").unwrap();
        assert!(quoted.len() <= MAX_FIRST_FAILURE_BYTES);
        assert!(quoted.len() > MAX_FIRST_FAILURE_BYTES - 4);
        assert!(quoted.starts_with("---- a stdout ----\né"));
    }
}

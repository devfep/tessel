//! The first failing test, quoted from cargo's output.
//!
//! Output is untrusted data (CLAUDE.md rule 3). This only picks a bounded excerpt for a person or
//! an agent to read, as quoted data. It is never an input to pass or fail.

/// The most bytes of the excerpt.
pub const MAX_FIRST_FAILURE_BYTES: usize = 1500;

/// The first failing test's block, from its `---- <name> stdout ----` line to the next blank line.
/// Without a block, the first line that says `FAILED` or `panicked`. Standard output is searched
/// before standard error. `None` when neither holds anything.
pub fn first_failure(stdout: &str, stderr: &str) -> Option<String> {
    let found = block(stdout)
        .or_else(|| block(stderr))
        .or_else(|| marker_line(stdout))
        .or_else(|| marker_line(stderr))?;
    Some(bounded(found))
}

fn block(text: &str) -> Option<String> {
    let mut lines = text
        .lines()
        .skip_while(|line| !(line.starts_with("---- ") && line.ends_with(" stdout ----")))
        .peekable();
    lines.peek()?;
    let kept: Vec<&str> = lines.take_while(|line| !line.trim().is_empty()).collect();
    Some(kept.join("\n"))
}

fn marker_line(text: &str) -> Option<String> {
    text.lines()
        .find(|line| line.contains("FAILED") || line.contains("panicked"))
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

    const TWO_FAILURES: &str = "running 2 tests\ntest a ... FAILED\ntest b ... FAILED\n\n\
        failures:\n\n---- a stdout ----\nthread 'a' panicked at src/lib.rs:1:1:\nboom a\n\n\
        ---- b stdout ----\nthread 'b' panicked at src/lib.rs:2:1:\nboom b\n\nfailures:\n";

    #[test]
    fn the_first_failing_tests_block_is_quoted_and_the_second_is_not() {
        let quoted = first_failure(TWO_FAILURES, "").unwrap();
        assert_eq!(
            quoted,
            "---- a stdout ----\nthread 'a' panicked at src/lib.rs:1:1:\nboom a"
        );
    }

    #[test]
    fn without_a_block_the_first_failed_or_panicked_line_is_quoted() {
        let quoted = first_failure("ok\nsomething panicked here\nFAILED later\n", "").unwrap();
        assert_eq!(quoted, "something panicked here");
        assert_eq!(first_failure("", "error: FAILED").unwrap(), "error: FAILED");
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

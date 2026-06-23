//! `StdioPrompter` — implements `Prompter`: interactive drive selection and
//! directory-creation / destructive-action confirmation. `AutoPrompter` bypasses
//! prompts for non-interactive/cron use (`--yes`). See `documentation/01` Phase 1.
//!
//! The actual prompt/parse logic is factored into pure functions over a
//! `BufRead`/`Write` pair so it is unit-testable without a real terminal.

use std::io::{self, BufRead, Write};

use mybtrfs_application::ports::{PortError, Prompter};

/// Interactive [`Prompter`] over stdin/stdout.
pub struct StdioPrompter;

impl StdioPrompter {
    /// Create a prompter that reads stdin and writes stdout.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdioPrompter {
    fn default() -> Self {
        Self::new()
    }
}

impl Prompter for StdioPrompter {
    fn confirm(&self, prompt: &str) -> Result<bool, PortError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        confirm_with(&mut stdin.lock(), &mut stdout.lock(), prompt)
    }

    fn choose(&self, prompt: &str, options: &[String]) -> Result<Option<usize>, PortError> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        choose_with(&mut stdin.lock(), &mut stdout.lock(), prompt, options)
    }
}

/// A non-interactive [`Prompter`] for `--yes`/cron: confirmations are auto-yes,
/// and a choice is auto-resolved only when exactly one option is offered (an
/// ambiguous choice stays `None`, so the caller must fail rather than guess).
pub struct AutoPrompter;

impl Prompter for AutoPrompter {
    fn confirm(&self, _prompt: &str) -> Result<bool, PortError> {
        Ok(true)
    }

    fn choose(&self, _prompt: &str, options: &[String]) -> Result<Option<usize>, PortError> {
        Ok((options.len() == 1).then_some(0))
    }
}

/// Read a single trimmed line from `reader`.
fn read_line(reader: &mut impl BufRead) -> Result<String, PortError> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

/// Write `prompt`, read a line, and return `true` for an affirmative (`y`/`yes`,
/// case-insensitive); anything else — including an empty line — is `false`
/// (the default is No, so a bare Enter never confirms a destructive action).
fn confirm_with(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    prompt: &str,
) -> Result<bool, PortError> {
    write!(writer, "{prompt} [y/N] ")?;
    writer.flush()?;
    let answer = read_line(reader)?.to_ascii_lowercase();
    Ok(matches!(answer.as_str(), "y" | "yes"))
}

/// Write `prompt` and a numbered list of `options`, read a 1-based selection, and
/// return the 0-based index — or `None` for an empty, non-numeric, or
/// out-of-range entry (a cancelled / invalid choice).
fn choose_with(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    prompt: &str,
    options: &[String],
) -> Result<Option<usize>, PortError> {
    writeln!(writer, "{prompt}")?;
    for (index, option) in options.iter().enumerate() {
        writeln!(writer, "  {}) {option}", index + 1)?;
    }
    write!(writer, "select [1-{}]: ", options.len())?;
    writer.flush()?;
    let answer = read_line(reader)?;
    Ok(answer
        .parse::<usize>()
        .ok()
        .filter(|choice| (1..=options.len()).contains(choice))
        .map(|choice| choice - 1))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn confirm(input: &str) -> (bool, String) {
        let mut reader = Cursor::new(input.as_bytes().to_vec());
        let mut out: Vec<u8> = Vec::new();
        let answer = confirm_with(&mut reader, &mut out, "create the directory?").unwrap();
        (answer, String::from_utf8(out).unwrap())
    }

    fn choose(input: &str, options: &[&str]) -> Option<usize> {
        let opts: Vec<String> = options.iter().map(|s| (*s).to_owned()).collect();
        let mut reader = Cursor::new(input.as_bytes().to_vec());
        let mut out: Vec<u8> = Vec::new();
        choose_with(&mut reader, &mut out, "pick a drive", &opts).unwrap()
    }

    #[test]
    fn confirm_accepts_yes_variants() {
        crate::init_test_logger();
        assert!(confirm("y\n").0);
        assert!(confirm("Y\n").0);
        assert!(confirm("yes\n").0);
        assert!(confirm("YES\n").0);
    }

    #[test]
    fn confirm_defaults_to_no() {
        crate::init_test_logger();
        assert!(!confirm("n\n").0);
        assert!(!confirm("\n").0); // bare Enter → No
        assert!(!confirm("nonsense\n").0);
        // The prompt (with the [y/N] default marker) is written.
        assert!(confirm("n\n").1.contains("create the directory? [y/N]"));
    }

    #[test]
    fn choose_returns_zero_based_index_for_valid_input() {
        crate::init_test_logger();
        assert_eq!(choose("1\n", &["a", "b", "c"]), Some(0));
        assert_eq!(choose("3\n", &["a", "b", "c"]), Some(2));
    }

    #[test]
    fn choose_rejects_empty_nonnumeric_and_out_of_range() {
        crate::init_test_logger();
        assert_eq!(choose("\n", &["a", "b"]), None); // empty
        assert_eq!(choose("x\n", &["a", "b"]), None); // non-numeric
        assert_eq!(choose("0\n", &["a", "b"]), None); // below range
        assert_eq!(choose("3\n", &["a", "b"]), None); // above range
    }

    #[test]
    fn auto_prompter_confirms_and_resolves_only_a_single_choice() {
        crate::init_test_logger();
        assert!(AutoPrompter.confirm("anything").unwrap());
        assert_eq!(
            AutoPrompter.choose("pick", &["only".to_owned()]).unwrap(),
            Some(0)
        );
        assert_eq!(
            AutoPrompter
                .choose("pick", &["a".to_owned(), "b".to_owned()])
                .unwrap(),
            None
        );
    }
}

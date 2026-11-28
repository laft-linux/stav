//! The long explanations of diagnostic codes.

use std::collections::BTreeMap;
use std::sync::LazyLock;

/// The error code reference. The documentation shows the same file, so
/// an explanation and its page never differ.
const REFERENCE: &str = include_str!("../error-codes.md");

static ENTRIES: LazyLock<BTreeMap<&'static str, String>> = LazyLock::new(|| entries(REFERENCE));

/// Returns the long explanation of a diagnostic code, such as `E0501` or
/// `W0302`, or `None` for a code that Stav does not report.
///
/// The explanation is Markdown. It says when the code is reported, shows a
/// small program that reports it together with the output of the `stav`
/// command, and says how to fix it.
///
/// ```
/// let text = stav::explain("E0501").expect("E0501 is a code");
/// assert!(text.contains("conflicting values"));
/// assert!(stav::explain("E9999").is_none());
/// ```
pub fn explain(code: &str) -> Option<&'static str> {
    ENTRIES.get(code).map(String::as_str)
}

/// Splits the reference into its entries, one for each `### <code>`
/// heading, without the comments that mark examples for verification.
fn entries(text: &'static str) -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    let mut current: Option<(&'static str, Vec<&'static str>)> = None;
    for line in text.lines() {
        if line.starts_with("## ") || line.starts_with("### ") {
            if let Some((code, lines)) = current.take() {
                out.insert(code, join(&lines));
            }
            current = line
                .strip_prefix("### ")
                .filter(|code| is_code(code))
                .map(|code| (code, Vec::new()));
        } else if let Some((_, lines)) = &mut current {
            let marker = line.starts_with("<!--") && line.ends_with("-->");
            if !marker {
                lines.push(line);
            }
        }
    }
    if let Some((code, lines)) = current {
        out.insert(code, join(&lines));
    }
    out
}

/// Joins lines into text without blank lines at either end or more than
/// one blank line in a row.
fn join(lines: &[&str]) -> String {
    let mut out = String::new();
    let mut blank = false;
    for line in lines {
        if line.trim().is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn is_code(text: &str) -> bool {
    let mut chars = text.chars();
    matches!(chars.next(), Some('E' | 'W')) && text.len() == 5 && chars.all(|c| c.is_ascii_digit())
}

#[cfg(test)]
#[path = "explain_tests.rs"]
mod tests;

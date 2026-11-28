use std::fs;
use std::path::Path;

use super::explain;

#[test]
fn an_entry_has_its_text_and_example_without_markers() {
    let text = explain("E0501").expect("E0501 is explained");
    assert!(text.starts_with("Two or more contributions"), "{text}");
    assert!(text.contains("```stav\n"));
    assert!(text.contains("error[E0501]"));
    assert!(!text.contains("<!--"));
    assert!(!text.contains("\n\n\n"));
    assert!(!text.contains("### "));
}

#[test]
fn unknown_codes_have_no_entry() {
    assert_eq!(explain("E9999"), None);
    assert_eq!(explain("e0501"), None);
    assert_eq!(explain(""), None);
}

/// Collects the codes that the sources under `dir` report, skipping tests
/// and the examples of this module.
fn reported_codes(dir: &Path, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).expect("the source directory is readable") {
        let path = entry.expect("the source directory is readable").path();
        if path.is_dir() {
            reported_codes(&path, out);
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") || name == "explain.rs" {
            continue;
        }
        let text = fs::read_to_string(&path).expect("the source file is readable");
        for (i, _) in text.match_indices('"') {
            let Some(code) = text.get(i + 1..i + 6) else {
                continue;
            };
            if super::is_code(code) && text[i + 6..].starts_with('"') {
                out.push(code.to_owned());
            }
        }
    }
}

#[test]
fn every_reported_code_is_explained() {
    let mut codes = Vec::new();
    reported_codes(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut codes,
    );
    codes.sort();
    codes.dedup();
    assert!(codes.len() > 50, "found only {codes:?}");
    let missing: Vec<&String> = codes.iter().filter(|c| explain(c).is_none()).collect();
    assert!(
        missing.is_empty(),
        "codes without an explanation: {missing:?}"
    );
}

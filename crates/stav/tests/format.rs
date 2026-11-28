//! Formats the fixture programs and checks the layout, stability and
//! meaning of the result.

use std::fs;
use std::path::{Path, PathBuf};

use stav::{SyntaxElement, TokenKind, format, parse};

use self::support::meaning;

mod support;

fn fixtures(dir: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/format")
        .join(dir);
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("the fixture directory exists")
        .map(|entry| entry.expect("the fixture directory is readable").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "stav"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "{} has no fixtures", dir.display());
    paths
}

fn format_checked(path: &Path) -> (String, String) {
    let source = fs::read_to_string(path).expect("the fixture is readable");
    let formatted = format(&source)
        .unwrap_or_else(|diagnostics| panic!("{} has errors: {diagnostics:?}", path.display()));
    assert_eq!(
        meaning(&source),
        meaning(&formatted),
        "formatting changed the meaning of {}",
        path.display()
    );
    let again = format(&formatted).expect("the formatted text has no errors");
    assert_eq!(
        again,
        formatted,
        "formatting {} is not idempotent",
        path.display()
    );
    (source, formatted)
}

#[test]
fn unformatted_fixtures_format_to_their_snapshots() {
    for path in fixtures("unformatted") {
        let (_, formatted) = format_checked(&path);
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("the name is UTF-8");
        insta::assert_snapshot!(name, formatted);
    }
}

#[test]
fn formatted_fixtures_are_left_unchanged() {
    for path in fixtures("formatted") {
        let (source, formatted) = format_checked(&path);
        assert_eq!(formatted, source, "{} changed", path.display());
    }
}

/// Evaluates `source` as the entry file of the root struct `Root` and
/// returns its canonical JSON.
fn evaluate(source: &str) -> String {
    let host = stav::Host::new(stav::MemoryLoader::new().with_file("app/main.stav", source));
    let root = stav::RootSpec::new(
        "Root",
        vec!["app/main.stav".into()],
        vec![stav::ImportRoot::new("app")],
    );
    stav::check(&host, &root)
        .and_then(|checked| checked.eval(&stav::Limits::default()))
        .map(|evaluated| evaluated.to_canonical_json())
        .unwrap_or_else(|diagnostics| panic!("{source:?} fails:\n{diagnostics}"))
}

#[test]
fn formatting_aliases_keeps_the_value() {
    let sources = [
        "type Port =\n  Int where 1..=65535\nstruct Root { p: Port = 80 }\n",
        "struct S { a: Int = 0 }\ntype Cfg =\n  S\nstruct Root { s: Cfg = {} }\n",
        "type H = Fn(\n  Str,\n) -> Bool\nfn run(h: H) -> Bool = h(\"x\")\nstruct Root { ok: Bool = run(fn(s: Str) = s == \"x\") }\n",
        "type Name = Str where matches(r\"[a-z][a-z0-9_-]*[a-z0-9]\") && nonEmpty && isShortEnoughForTheNameField\nfn isShortEnoughForTheNameField(s: Str) -> Bool = len(s) < 20\nstruct Root { n: Name = \"ab\" }\n",
    ];
    for source in sources {
        let formatted = format(source).expect("the source has no errors");
        assert_eq!(evaluate(source), evaluate(&formatted), "{formatted}");
    }
}

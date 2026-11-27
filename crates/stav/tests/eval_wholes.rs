//! Checks dependency graphs and replacement boundaries of late whole values.

use serde_json::{Value, json};
use stav::{Diagnostics, Host, ImportRoot, Limits, MemoryLoader, RootSpec, check};

fn evaluate(source: &str) -> Result<Value, Diagnostics> {
    let host = Host::new(MemoryLoader::new().with_file("app/main.stav", source));
    let root = RootSpec {
        root_struct: "Root".to_owned(),
        entries: vec!["app/main.stav".into()],
        import_roots: vec![ImportRoot::new("app")],
    };
    let checked = check(&host, &root)
        .unwrap_or_else(|diagnostics| panic!("the program must check:\n{diagnostics}"));
    let value = checked.eval(&Limits::default())?;
    Ok(serde_json::from_str(&value.to_canonical_json()).expect("canonical JSON must parse"))
}

fn assert_error(source: &str, code: &str, edges: &[&str]) {
    let error = evaluate(source).expect_err("evaluation must reject the program");
    let codes: Vec<_> = error
        .iter()
        .map(|diagnostic| diagnostic.diagnostic.code)
        .collect();
    assert_eq!(codes, [code], "{error}\n{source}");
    let display = error.to_string();
    for edge in edges {
        assert!(display.contains(edge), "missing edge {edge}:\n{display}");
    }
}

#[test]
fn lower_whole_dependency_cycles_finish_before_evaluation() {
    const CHILD: &str = "STAV_WHOLE_CYCLE_CHILD";
    if std::env::var_os(CHILD).is_some() {
        for (a, b, edges) in [
            ("f(self.m)", "0", ["`a` reads `m`", "`m` reads `a`"]),
            ("0", "f(self.m)", ["`b` reads `m`", "`m` reads `b`"]),
        ] {
            let source = format!(
                "struct Root {{ a: Int = {a}, b: Int = {b}, m: Map<Str, Int> = mk0(self.a) }}\n\
                 fn f(m: Map<Str, Int>) -> Int = 1\n\
                 fn mk0(a: Int) -> Map<Str, Int> = {{ old = a }}\n\
                 fn mk1(b: Int) -> Map<Str, Int> = {{ new = b }}\n\
                 m = mk1(self.b)\n"
            );
            assert_error(&source, "E0607", &edges);
        }
        assert_error(
            r#"
            type Build = cargo(features: List<Str> = []) | meson(options: List<Str> = [])
            struct Root { k: Str = g(self.build), build: Build }
            fn g(build: Build) -> Str = "x"
            fn mkbuild(k: Str) -> Build = cargo()
            build = default(mkbuild(self.k))
            build cargo { features = ["x"] }
            "#,
            "E0607",
            &["`k` reads `build`", "`build` reads `k`"],
        );
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "lower_whole_dependency_cycles_finish_before_evaluation",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the bounded evaluation child starts");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            assert!(
                status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "cycle evaluation did not terminate:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn declared_and_statement_map_wholes_both_supply_entries() {
    let source = r#"
    struct Root { a: Int = 1, b: Int = 2, m: Map<Str, Int> = mk0(self.a) }
    fn mk0(a: Int) -> Map<Str, Int> = { old = a, shared = a }
    fn mk1(b: Int) -> Map<Str, Int> = { new = b, shared = b }
    m = mk1(self.b)
    "#;
    assert_eq!(
        evaluate(source).unwrap()["m"],
        json!({"old": 1, "new": 2, "shared": 2})
    );
}

#[test]
fn higher_wholes_cover_lower_defaults_without_adding_losing_edges() {
    for (ty, declarations, low, high, expected) in [
        ("Int", "", "1 / a", "2", json!(2)),
        ("List<Int>", "", "[1 / a]", "[2]", json!([2])),
        ("Set<Int>", "", "[1 / a]", "[2]", json!([2])),
        (
            "Data",
            "",
            "{ bad = 1 / a }",
            "{ good = 2 }",
            json!({"good": 2}),
        ),
        (
            "C",
            "struct C { n: Int }",
            "C { n = 1 / a }",
            "C { n = 2 }",
            json!({"n": 2}),
        ),
        (
            "Choice",
            "type Choice = one(n: Int) | two",
            "one(n = 1 / a)",
            "two",
            json!("two"),
        ),
        (
            "Option<C>",
            "struct C { n: Int }",
            "C { n = 1 / a }",
            "none",
            Value::Null,
        ),
        (
            "Option<Map<Str, Int>>",
            "",
            "{ bad = 1 / a }",
            "none",
            Value::Null,
        ),
        ("Option<Data>", "", "{ bad = 1 / a }", "none", Value::Null),
    ] {
        let source = format!(
            "{declarations}\nstruct Root {{ a: Int = f(self.c), b: Int = 0, c: {ty} = bad(self.a) }}\n\
             fn f(c: {ty}) -> Int = 1\n\
             fn bad(a: Int) -> {ty} = {low}\n\
             fn good(b: Int) -> {ty} = {high}\n\
             c = good(self.b)\n"
        );
        let value = evaluate(&source).unwrap_or_else(|error| panic!("{error}\n{source}"));
        assert_eq!(value["a"], json!(1));
        assert_eq!(value["c"], expected, "{source}");
    }
}

#[test]
fn higher_option_wholes_never_execute_failing_lower_options() {
    for (ty, low) in [
        ("Download", "Download { version = a, url = \"#{1 / 0}\" }"),
        ("Map<Str, Int>", "{ bad = 1 / 0 }"),
    ] {
        let source = format!(
            "struct Download {{ version: Str, url: Str }}\n\
             struct Root {{ a: Str = \"x\", b: Int = 0, o: Option<{ty}> = opt0(self.a) }}\n\
             fn opt0(a: Str) -> Option<{ty}> = {low}\n\
             fn opt1(b: Int) -> Option<{ty}> = none\n\
             o = opt1(self.b)\n"
        );
        assert_eq!(evaluate(&source).unwrap()["o"], Value::Null);
    }
}

const DOWNLOAD: &str = r#"
struct Download { version: Str, url: Str = "https://example.org/tool-#{self.version}.tar.gz" }
"#;

#[test]
fn higher_struct_none_replaces_lower_optional_whole_parts() {
    let source = format!(
        r#"{DOWNLOAD}
        struct T {{ o: Option<Download> }}
        struct Root {{ k: Str = "x", v: Str = "y", m: Map<Str, T> }}
        fn mk(k: Str) -> Map<Str, T> = {{ t = T {{ o = Download {{ version = "1", url = "u" }} }} }}
        fn mkt(v: Str) -> T = T {{ o = none }}
        m = default(mk(self.k))
        m.t = mkt(self.v)
        m.t.o.version = force("2")
        "#
    );
    assert_eq!(
        evaluate(&source).unwrap()["m"]["t"]["o"],
        json!({"version": "2", "url": "https://example.org/tool-2.tar.gz"})
    );
}

#[test]
fn higher_option_none_replaces_lower_parts_but_keeps_outer_siblings() {
    let source = format!(
        r#"{DOWNLOAD}
        struct Box {{ o: Option<Download>, n: Int = 0 }}
        struct Root {{ k: Str = "x", x: Str = "y", cfg: Box }}
        fn mk(k: Str) -> Box = Box {{ o = Download {{ version = "1", url = "u" }}, n = 3 }}
        fn opt(x: Str) -> Option<Download> = none
        cfg = default(mk(self.k))
        cfg.o = opt(self.x)
        cfg.o.version = force("2")
        "#
    );
    assert_eq!(
        evaluate(&source).unwrap()["cfg"],
        json!({"n": 3, "o": {"version": "2", "url": "https://example.org/tool-2.tar.gz"}})
    );
}

#[test]
fn higher_optional_map_none_discards_lower_present_map_keys() {
    let source = r#"
    struct Box { o: Option<Map<Str, Int>>, n: Int }
    struct Root { k: Int = 1, cfg: Box }
    fn mk(k: Int) -> Box = Box { o = { old = 1 }, n = 3 }
    fn opt(k: Int) -> Option<Map<Str, Int>> = none
    cfg = default(mk(self.k))
    cfg.o = opt(self.k)
    cfg.o.forced = force(2)
    "#;
    assert_eq!(
        evaluate(source).unwrap()["cfg"],
        json!({"n": 3, "o": {"forced": 2}})
    );
}

#[test]
fn whole_map_keys_replace_lower_nested_maps_at_the_same_path() {
    let source = r#"
    struct Root { a: Int = 1, b: Int = 2, m: Map<Str, Map<Str, Int>> = mk0(self.a) }
    fn mk0(a: Int) -> Map<Str, Map<Str, Int>> = { shared = { old = a }, lower = { kept = a } }
    fn mk1(b: Int) -> Map<Str, Map<Str, Int>> = { shared = { new = b } }
    m = mk1(self.b)
    "#;
    assert_eq!(
        evaluate(source).unwrap()["m"],
        json!({"shared": {"new": 2}, "lower": {"kept": 1}})
    );
}

#[test]
fn two_statement_wholes_always_fail_static_validation() {
    for a in [0, 1] {
        for b in [0, 1] {
            for left in ["default(mk0(self.a))", "mk0(self.a)", "force(mk0(self.a))"] {
                for right in ["default(mk1(self.b))", "mk1(self.b)", "force(mk1(self.b))"] {
                    let source = format!(
                        "struct Root {{ a: Int = {a}, b: Int = {b}, m: Map<Str, Int> }}\n\
                         fn mk0(a: Int) -> Map<Str, Int> = if a == 0 {{ {{}} }} else {{ {{ shared = a }} }}\n\
                         fn mk1(b: Int) -> Map<Str, Int> = if b == 0 {{ {{}} }} else {{ {{ shared = b }} }}\n\
                         m = {left}\nm = {right}\n"
                    );
                    assert_error(&source, "E0504", &[]);
                }
            }
        }
    }
}

#[test]
fn replaced_wholes_cannot_supply_required_fields_of_created_options() {
    let source = format!(
        r#"{DOWNLOAD}
        struct Box {{ o: Option<Download>, n: Int = 0 }}
        struct Root {{ k: Str = "x", x: Str = "y", cfg: Box }}
        fn mk(k: Str) -> Box = Box {{ o = Download {{ version = "1", url = "u" }}, n = 3 }}
        fn opt(x: Str) -> Option<Download> = none
        cfg = default(mk(self.k))
        cfg.o = opt(self.x)
        cfg.o.url = force("higher")
        "#
    );
    assert_error(&source, "E0801", &["cfg.o.version"]);
}

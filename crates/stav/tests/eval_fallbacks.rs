//! Exercises defaults in instances created below optional whole values.

use serde_json::{Value, json};
use stav::{Host, ImportRoot, Limits, MemoryLoader, RootSpec, check};

fn checked(source: &str) -> stav::Checked {
    let host = Host::new(MemoryLoader::new().with_file("app/main.stav", source));
    let root = RootSpec::new(
        "Root",
        vec!["app/main.stav".into()],
        vec![ImportRoot::new("app")],
    );
    check(&host, &root)
        .unwrap_or_else(|diagnostics| panic!("the program must check:\n{diagnostics}\n{source}"))
}

fn optional_map_program(size: usize, result: &str, whole: bool) -> String {
    let mut source = format!(
        "struct P {{ a: Int = self.b, b: Int = 1, c: Int = 0 }}\n\
         fn mk(seed: Int) -> Option<Map<Str, P>> = {result}\n\
         struct Root {{ seed: Int = 0, o: Option<Map<Str, P>> }}\n"
    );
    if whole {
        source.push_str("o = mk(self.seed)\n");
    }
    for i in 0..size {
        source.push_str(&format!("o.k{i}.c = force(1)\n"));
    }
    source
}

fn assert_entries(actual: &Value, size: usize, supplied: bool) {
    let entries = actual["o"].as_object().expect("the option must be present");
    assert_eq!(entries.len(), size);
    for i in 0..size {
        let expected = if supplied && i == 0 { 7 } else { 1 };
        assert_eq!(
            entries[&format!("k{i}")],
            json!({"a": expected, "b": expected, "c": 1})
        );
    }
}

#[test]
fn optional_map_created_entries_keep_sibling_defaults() {
    for result in ["{}", "{ k0 = P { b = 7 } }", "none"] {
        let source = optional_map_program(96, result, true);
        let value = checked(&source).eval(&Limits::default()).unwrap();
        let actual: Value = serde_json::from_str(&value.to_canonical_json()).unwrap();
        assert_entries(&actual, 96, result.contains("k0"));
    }
}

#[test]
fn fixed_map_shapes_replace_suspended_optional_defaults_across_entries() {
    let source = r#"
    struct S { v: Int = 0 }
    fn opt(a: Int) -> Option<S> = S { v = a }
    struct P { o: Option<S> = opt(self.a), a: Int = 0 }
    fn base() -> Map<Str, P> = { x = P { o = none }, y = P { o = S { v = 8 } } }
    struct W { m: Map<Str, P> = base() }
    fn mk(seed: Int) -> Map<Str, P> = {}
    struct Root { seed: Int = 0, w: W }
    w.m = mk(self.seed)
    w.m.x.a = force(1)
    w.m.y.a = force(2)
    "#;
    let value = checked(source).eval(&Limits::default()).unwrap();
    let actual: Value = serde_json::from_str(&value.to_canonical_json()).unwrap();
    assert_eq!(
        actual,
        json!({"seed": 0, "w": {"m": {
            "x": {"a": 1, "o": null}, "y": {"a": 2, "o": {"v": 8}}
        }}})
    );
}

#[test]
fn newly_registered_defaults_create_nested_optional_instances() {
    for key in ["x", "\"#{key()}\""] {
        let source = format!(
            "struct P {{ a: Int = self.b, b: Int = 1, c: Int = 0 }}\n\
             fn key() -> Str = \"x\"\n\
             struct W {{ seed: Int = 0, m: Map<Str, Option<P>> = {{ {key} = {{ c = 2 }} }}, \
             n: Option<Map<Str, P>> = {{ y = {{ c = 3 }} }} }}\n\
             fn mk(seed: Int) -> Option<W> = none\n\
             struct Root {{ seed: Int = 0, o: Option<W> }}\n\
             o = mk(self.seed)\no.seed = force(1)"
        );
        let value = checked(&source).eval(&Limits::default()).unwrap();
        let actual: Value = serde_json::from_str(&value.to_canonical_json()).unwrap();
        assert_eq!(
            actual,
            json!({"seed": 0, "o": {"seed": 1,
                "m": {"x": {"a": 1, "b": 1, "c": 2}},
                "n": {"y": {"a": 1, "b": 1, "c": 3}}
            }})
        );
    }
}

#[test]
fn optional_fallback_cycles_are_checked_independently_of_whole_values() {
    for result in ["none", "{ x = P { a = 7 } }"] {
        let source = format!(
            "struct P {{ a: Int = self.b, b: Int = self.a, c: Int = 0 }}\n\
             fn mk(seed: Int) -> Option<Map<Str, P>> = {result}\n\
             struct Root {{ seed: Int = 0, o: Option<Map<Str, P>> }}\n\
             o = mk(self.seed)\no.x.c = force(1)\no.y.c = force(1)"
        );
        let error = checked(&source).eval(&Limits::default()).unwrap_err();
        let diagnostics: Vec<_> = error.iter().collect();
        assert_eq!(diagnostics.len(), 2, "{error}");
        for diagnostic in diagnostics {
            assert_eq!(diagnostic.diagnostic.code, "E0607", "{error}");
            assert_eq!(diagnostic.diagnostic.labels.len(), 2, "{error}");
        }
    }
}

#[test]
#[ignore = "reports checking and evaluation time for optional map fallbacks"]
fn optional_map_fallback_timing() {
    let size = std::env::var("STAV_OPTION_FALLBACK_BENCH_SIZE")
        .map(|value| value.parse::<usize>().expect("the size must be an integer"))
        .unwrap_or(6_400);
    assert!(size > 0, "the size must be positive");
    for size in [(size / 4).max(1), (size / 2).max(1), size] {
        for whole in [false, true] {
            let source = optional_map_program(size, "{}", whole);
            let started = std::time::Instant::now();
            let program = checked(&source);
            let check_time = started.elapsed();
            let started = std::time::Instant::now();
            let value = program.eval(&Limits::default()).unwrap();
            let eval_time = started.elapsed();
            let actual: Value = serde_json::from_str(&value.to_canonical_json()).unwrap();
            assert_entries(&actual, size, false);
            eprintln!("size={size} whole={whole} check={check_time:?} eval={eval_time:?}");
        }
    }
}

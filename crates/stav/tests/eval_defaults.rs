//! Exercises declared defaults after higher-priority contributions are merged.

use serde_json::{Value, json};
use stav::{Diagnostics, Host, ImportRoot, Limits, MemoryLoader, RootSpec, check};

fn checked(source: &str) -> stav::Checked {
    let host = Host::new(MemoryLoader::new().with_file("app/main.stav", source));
    let root = RootSpec {
        root_struct: "Root".to_owned(),
        entries: vec!["app/main.stav".into()],
        import_roots: vec![ImportRoot::new("app")],
    };
    check(&host, &root)
        .unwrap_or_else(|diagnostics| panic!("the program must check:\n{diagnostics}\n{source}"))
}

fn evaluate(source: &str) -> Result<Value, Diagnostics> {
    let value = checked(source).eval(&Limits::default())?;
    Ok(serde_json::from_str(&value.to_canonical_json()).expect("canonical JSON must parse"))
}

fn assert_value(source: &str, expected: Value) {
    let actual = evaluate(source).unwrap_or_else(|diagnostics| {
        panic!("the program must evaluate:\n{diagnostics}\n{source}")
    });
    assert_eq!(actual, expected, "{source}");
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
fn overridden_root_defaults_do_not_execute() {
    for contribution in ["7", "default(7)", "force(7)"] {
        let source = format!("struct Root {{ n: Int = 1 / 0 }}\nn = {contribution}\n");
        assert_value(&source, json!({"n": 7}));
    }
    assert_error("struct Root { n: Int = 1 / 0 }", "E0603", &[]);
}

#[test]
fn created_map_entries_skip_covered_defaults_with_or_without_a_late_whole() {
    for whole in ["", "m = mk(self.k)\n"] {
        let source = format!(
            "struct P {{ a: Int = 1 / 0, b: Int = self.a + self.a }}\n\
             struct Root {{ k: Int = 0, m: Map<Str, P> = {{}} }}\n\
             fn mk(k: Int) -> Map<Str, P> = {{}}\n\
             {whole}m.x.a = force(7)\n"
        );
        assert_value(&source, json!({"k": 0, "m": {"x": {"a": 7, "b": 14}}}));
    }
}

#[test]
fn nested_literal_defaults_keep_uncovered_siblings() {
    for contribution in ["p.inner.a = 7", "p { inner { a = 7 } }"] {
        let source = format!(
            "struct Inner {{ a: Int, b: Int }}\n\
             struct P {{ inner: Inner, c: Int }}\n\
             struct Root {{ p: P = {{ inner = {{ a = 1 / 0, b = 4 }}, c = 5 }} }}\n\
             {contribution}\n"
        );
        assert_value(&source, json!({"p": {"inner": {"a": 7, "b": 4}, "c": 5}}));
    }
    assert_error(
        "struct P { a: Int, b: Int }\n\
         struct Root { p: P = { a = 7, b = 1 / 0 } }\np.a = force(8)",
        "E0603",
        &[],
    );
}

#[test]
fn selected_payload_blocks_skip_losing_declared_defaults() {
    let source = r#"
    type Build = cargo(n: Int = 1 / 0, kept: Int = 4) | meson
    struct Root { build: Build }
    build cargo { n = 7 }
    "#;
    assert_value(source, json!({"build": {"cargo": {"n": 7, "kept": 4}}}));

    let source = r#"
    type Build = cargo(n: Int, kept: Int) | meson
    struct Root { build: Build = cargo(n = 3, kept = 4) }
    build cargo { n = 7 }
    "#;
    assert_value(source, json!({"build": {"cargo": {"n": 7, "kept": 4}}}));
}

#[test]
fn self_defaults_use_their_own_instance_frames_for_repeated_reads() {
    let source = r#"
    struct Inner { v: Int = 0, twice: Int = self.v + self.v }
    struct S {
        seed: Int = 0
        twice: Int = self.seed + self.seed
        inner: Inner = { v = self.seed }
    }
    struct Root {
        seed: Int = 999
        left: S = { seed = 2 }
        right: S = { seed = 3 }
        total: Int = self.left.twice + self.right.twice + self.left.twice
    }
    right.seed = 5
    "#;
    assert_value(
        source,
        json!({
            "seed": 999,
            "left": {"seed": 2, "twice": 4, "inner": {"v": 2, "twice": 4}},
            "right": {"seed": 5, "twice": 10, "inner": {"v": 5, "twice": 10}},
            "total": 18
        }),
    );
}

#[test]
fn repeated_default_dependencies_reuse_the_merged_value() {
    let mut fields: Vec<String> = (0..20)
        .map(|i| format!("f{i}: Int = self.f{} + self.f{}", i + 1, i + 1))
        .collect();
    fields.push("f20: Int = 1 / 0".to_owned());
    let source = format!(
        "struct S {{ {} }}\nstruct Root {{ left: S, right: S }}\n\
         left.f20 = 1\nright.f20 = 2\n",
        fields.join(", ")
    );
    let value = evaluate(&source).unwrap();
    for i in 0..=20 {
        assert_eq!(value["left"][format!("f{i}")], json!(1 << (20 - i)));
        assert_eq!(value["right"][format!("f{i}")], json!(2 << (20 - i)));
    }
}

#[test]
fn enclosing_map_defaults_supply_created_entries_beneath_a_late_whole() {
    let source = r#"
    struct P { a: Int = self.b, b: Int = self.a, c: Int = 0 }
    struct W { m: Map<Str, P> = { x = { a = 1 } } }
    fn mk(k: Int) -> Map<Str, P> = {}
    struct Root { k: Int = 0, w: W }
    w.m = mk(self.k)
    w.m.x.c = force(1)
    "#;
    assert_value(
        source,
        json!({"k": 0, "w": {"m": {"x": {"a": 1, "b": 1, "c": 1}}}}),
    );
}

#[test]
fn recreated_options_recompute_defaults_instead_of_using_enclosing_none() {
    let source = r#"
    struct S { v: Int = 0 }
    fn opt0(a: Int) -> Option<S> = S { v = a }
    struct P { o: Option<S> = opt0(self.a), a: Int = 0 }
    struct W { p: Option<P> = { o = none } }
    fn mkp(k: Int) -> Option<P> = none
    struct Root { k: Int = 0, w: W }
    w.p = mkp(self.k)
    w.p.a = force(1)
    "#;
    assert_value(source, json!({"k": 0, "w": {"p": {"a": 1, "o": {"v": 1}}}}));
}

#[test]
fn optional_wholes_do_not_hide_cycles_in_recreated_instance_defaults() {
    for result in ["none", "P { a = 2, b = 3 }"] {
        let source = format!(
            "struct P {{ a: Int = self.b, b: Int = self.a, c: Int = 0 }}\n\
             struct W {{ p: Option<P> = {{ a = 1 }} }}\n\
             fn mkp(k: Int) -> Option<P> = {result}\n\
             struct Root {{ k: Int = 0, w: W }}\n\
             w.p = mkp(self.k)\nw.p.c = force(1)\n"
        );
        assert_error(
            &source,
            "E0607",
            &["`w.p.a` reads `w.p.b`", "`w.p.b` reads `w.p.a`"],
        );
    }
}

#[test]
fn shape_only_wholes_do_not_remove_reintroduced_default_cycle_edges() {
    let source = r#"
    struct S { a: Int = self.b, b: Int = self.a }
    fn mk() -> S = S { a = 1 }
    struct Root { s: S = mk() }
    s { a = self.b }
    "#;
    assert_error(source, "E0607", &["`s.a` reads `s.b`", "`s.b` reads `s.a`"]);
}

#[derive(Clone, Copy, Debug)]
enum Coverage {
    Complete,
    Partial,
    MixedPriorities,
}

fn wide_program(size: usize, coverage: Coverage) -> (String, Value) {
    let mut fields = vec!["base: Int = 3".to_owned(), "zero: Int = 0".to_owned()];
    let mut statements = vec!["mid.big.base = 3".to_owned(), "mid.big.zero = 0".to_owned()];
    let mut expected = serde_json::Map::new();
    expected.insert("base".to_owned(), json!(3));
    expected.insert("zero".to_owned(), json!(0));
    for i in 0..size {
        let uncovered = matches!(coverage, Coverage::Partial) && i % 2 == 1;
        let default = format!("self.base + {i}");
        fields.push(format!("f{i}: Int = {default}"));
        let value = if uncovered { 3 + i } else { 100 + i };
        if !uncovered {
            let contribution = match coverage {
                Coverage::MixedPriorities => match i % 3 {
                    0 => format!("default({value})"),
                    1 => {
                        statements.push(format!("mid.big.f{i} = default(1 / self.zero)"));
                        value.to_string()
                    }
                    _ => {
                        statements.push(format!("mid.big.f{i} = 1 / self.zero"));
                        format!("force({value})")
                    }
                },
                Coverage::Complete | Coverage::Partial => value.to_string(),
            };
            statements.push(format!("mid.big.f{i} = {contribution}"));
        }
        expected.insert(format!("f{i}"), json!(value));
    }
    let source = format!(
        "struct Big {{ {} }}\n\
         struct Mid {{ big: Big, sibling: Map<Str, Int> = {{ kept = 9 }} }}\n\
         struct Root {{ seed: Int = 0, zero: Int = 0, mid: Mid = mk(self.seed) }}\n\
         fn mk(seed: Int) -> Mid = Mid {{ big = Big {{}} }}\n\
         {}\n\
         mid.sibling.added = 10\n",
        fields.join(", "),
        statements.join("\n")
    );
    (
        source,
        json!({"seed": 0, "zero": 0, "mid": {"big": expected, "sibling": {"kept": 9, "added": 10}}}),
    )
}

#[test]
fn wide_nested_defaults_preserve_siblings_and_priority_winners() {
    for coverage in [
        Coverage::Complete,
        Coverage::Partial,
        Coverage::MixedPriorities,
    ] {
        let (source, expected) = wide_program(192, coverage);
        assert_value(&source, expected);
    }
}

#[test]
fn partial_coverage_does_not_suppress_failing_map_siblings() {
    let (source, _) = wide_program(32, Coverage::Partial);
    let source = source.replace("kept = 9", "kept = 1 / 0");
    assert_error(&source, "E0603", &[]);
}

#[test]
#[ignore = "reports declared-default timing for generated wide nested structs"]
fn wide_nested_defaults_timing() {
    let size = std::env::var("STAV_DECLARED_DEFAULTS_BENCH_SIZE")
        .map(|value| value.parse::<usize>().expect("the size must be an integer"))
        .unwrap_or(2_000);
    assert!(size > 0, "the size must be positive");
    for size in [(size / 4).max(1), (size / 2).max(1), size] {
        for coverage in [
            Coverage::Complete,
            Coverage::Partial,
            Coverage::MixedPriorities,
        ] {
            let (source, expected) = wide_program(size, coverage);
            let started = std::time::Instant::now();
            let program = checked(&source);
            let check_time = started.elapsed();
            let started = std::time::Instant::now();
            let evaluated = program.eval(&Limits::default()).unwrap();
            let eval_time = started.elapsed();
            let actual: Value = serde_json::from_str(&evaluated.to_canonical_json()).unwrap();
            assert_eq!(actual, expected);
            eprintln!("size={size} coverage={coverage:?} check={check_time:?} eval={eval_time:?}");
        }
    }
}

#[test]
fn read_free_fragments_are_cached_across_instances_and_evaluations_are_fresh() {
    let fields = (0..20)
        .map(|i| format!("b{i}: Build"))
        .collect::<Vec<_>>()
        .join(", ");
    let bodies = (0..20)
        .map(|i| format!("items.k{i}.seed = {i}\nb{i} cargo {{}}\n"))
        .collect::<String>();
    let source = format!(
        r#"
    struct S {{
        seed: Int = 0
        value: Int = fold(range(0, 1000), 0, fn(acc: Int, i: Int) = acc + i)
        plus: Int = self.value + self.seed
    }}
    type Build = cargo(value: Int = fold(range(0, 1000), 0, fn(acc: Int, i: Int) = acc + i))
    struct Root {{ items: Map<Str, S> = {{}}, {fields} }}
    {bodies}
    "#
    );
    let program = checked(&source);
    let limits = Limits {
        steps: 30_000,
        ..Limits::default()
    };
    for _ in 0..2 {
        let evaluated = program.eval(&limits).unwrap();
        let actual: Value = serde_json::from_str(&evaluated.to_canonical_json()).unwrap();
        for i in 0..20 {
            assert_eq!(actual["items"][format!("k{i}")]["plus"], json!(499_500 + i));
            assert_eq!(actual[format!("b{i}")]["cargo"]["value"], json!(499_500));
        }
    }
}

#[test]
fn ordinary_losing_statements_still_execute_eagerly() {
    assert_error(
        "struct Root { n: Int }\nn = default(1 / 0)\nn = 7",
        "E0603",
        &[],
    );
}

#[test]
fn late_whole_labels_omit_only_enclosing_presence_markers() {
    let source = "type Level = info | warn\nstruct S { level: Level }\n\
        struct Root { x: Int = 0, s: S }\nfn mk(x: Int) -> S = S { level = info }\n\
        s = mk(self.x)\ns { level warn {} }";
    let error = evaluate(source).unwrap_err();
    let diagnostic = &error.iter().next().unwrap().diagnostic;
    assert_eq!(diagnostic.code, "E0504");
    assert_eq!(diagnostic.labels.len(), 2, "{error}");
    assert!(diagnostic.labels.iter().any(|l| l.message.contains("warn")));

    let source = "struct S { n: Int = 0 }\nstruct Root { x: Int = 0, s: S }\n\
        fn mk(x: Int) -> S = S {}\ns = mk(self.x)\ns {}\ns {}";
    let error = evaluate(source).unwrap_err();
    let diagnostic = &error.iter().next().unwrap().diagnostic;
    assert_eq!(diagnostic.code, "E0504");
    assert_eq!(diagnostic.labels.len(), 3, "{error}");
}

#[test]
fn covered_read_free_sum_defaults_do_not_suspend_selected_payload_defaults() {
    assert_value(
        "type Build = cargo(kept: Int = 4) | meson\n\
         struct Root { build: Build = meson }\nbuild cargo {}\nbuild = cargo()",
        json!({"build": {"cargo": {"kept": 4}}}),
    );
}

#[test]
fn whole_conflict_labels_keep_disjoint_empty_blocks_and_other_files() {
    let source = "struct Root { x: Int = 0, m: Map<Str, Int> }\n\
        fn mk(x: Int) -> Map<Str, Int> = {}\nm = mk(self.x)\nm {}\nm = { a = 2 }";
    let error = evaluate(source).unwrap_err();
    let diagnostic = &error.iter().next().unwrap().diagnostic;
    assert_eq!(diagnostic.code, "E0504");
    assert_eq!(diagnostic.labels.len(), 3, "{error}");
    assert_eq!(
        diagnostic
            .labels
            .iter()
            .filter(|l| l.message.starts_with("present"))
            .count(),
        1
    );

    for include in [false, true] {
        let main = format!(
            "struct Root {{ x: Int = 0, m: Map<Str, Int> }}\n\
             fn mk(x: Int) -> Map<Str, Int> = {{}}\nm = mk(self.x)\nm {{ # {}\n}}\n{}",
            "padding".repeat(50),
            if include {
                "include \"./overlay.stav\"\n"
            } else {
                ""
            }
        );
        let start = main.find("m { #").unwrap();
        let overlay = format!("# {}\nm.b = 2\n", " ".repeat(start + 10));
        let host = Host::new(
            MemoryLoader::new()
                .with_file("app/main.stav", &main)
                .with_file("app/overlay.stav", &overlay),
        );
        let root = RootSpec {
            root_struct: "Root".to_owned(),
            entries: if include {
                vec!["app/main.stav".into()]
            } else {
                vec!["app/main.stav".into(), "app/overlay.stav".into()]
            },
            import_roots: vec![ImportRoot::new("app")],
        };
        let program = check(&host, &root).unwrap();
        let error = program.eval(&Limits::default()).unwrap_err();
        let diagnostic = &error.iter().next().unwrap().diagnostic;
        assert_eq!(diagnostic.code, "E0504");
        assert_eq!(diagnostic.labels.len(), 3, "{error}");
        assert!(
            diagnostic
                .labels
                .iter()
                .any(|l| l.message.starts_with("present")),
            "{error}"
        );
        if include {
            assert!(
                diagnostic
                    .labels
                    .iter()
                    .any(|l| l.message.contains("included from")),
                "{error}"
            );
        }
    }
}

#[test]
fn covered_read_free_struct_wholes_never_execute() {
    let source = "struct S { a: Int, b: Int }\n\
        fn bad() -> S = fail(\"unused whole\")\n\
        struct Root { s: S = bad() }\ns.a = 1\ns.b = 2";
    assert_value(source, json!({"s": {"a": 1, "b": 2}}));
    assert_error(&source.replace("\ns.b = 2", ""), "E0608", &[]);
}

#[test]
fn participating_constructor_defaults_evaluate_their_arguments() {
    assert_error(
        "type Build = cargo(n: Int, kept: Int) | meson\n\
         struct Root { build: Build = cargo(n = 1 / 0, kept = 4) }\n\
         build cargo { n = 7 }",
        "E0603",
        &[],
    );
}

#[test]
fn nonliteral_enclosing_map_defaults_supply_keys_before_cycle_validation() {
    for (whole, a) in [
        ("{}", 1),
        ("{ x = P { a = 2 } }", 2),
        ("{ y = P { a = 3 } }", 1),
    ] {
        let source = format!(
            "struct P {{ a: Int = self.b, b: Int = self.a, c: Int = 0 }}\n\
             fn base() -> Map<Str, P> = {{ x = P {{ a = 1 }} }}\n\
             struct W {{ m: Map<Str, P> = base() }}\n\
             fn mk(k: Int) -> Map<Str, P> = {whole}\n\
             struct Root {{ k: Int = 0, w: W }}\n\
             w.m = mk(self.k)\nw.m.x.c = force(1)"
        );
        let mut entries = json!({"x": {"a": a, "b": a, "c": 1}});
        if whole.contains("y =") {
            entries["y"] = json!({"a": 3, "b": 3, "c": 0});
        }
        assert_value(&source, json!({"k": 0, "w": {"m": entries}}));
    }
    assert_error(
        "struct P { a: Int = self.b, b: Int = self.a, c: Int = 0 }\n\
         fn base() -> Map<Str, P> = {}\nstruct W { m: Map<Str, P> = base() }\n\
         fn mk(k: Int) -> Map<Str, P> = {}\nstruct Root { k: Int = 0, w: W }\n\
         w.m = mk(self.k)\nw.m.x.c = force(1)",
        "E0607",
        &["`w.m.x.a` reads `w.m.x.b`", "`w.m.x.b` reads `w.m.x.a`"],
    );
}

#[test]
fn conditional_read_free_defaults_below_map_wholes_remain_suspended() {
    let source = "struct P { a: Int = 7, b: Int = self.a, c: Int = 0 }\n\
        fn bad() -> P = fail(\"unused entry\")\n\
        struct W { m: Map<Str, P> = { x = bad() } }\n\
        fn mk(k: Int) -> Map<Str, P> = { x = P { a = 9 } }\n\
        struct Root { k: Int = 0, w: W }\nw.m = mk(self.k)\nw.m.x.c = force(1)";
    assert_value(
        source,
        json!({"k": 0, "w": {"m": {"x": {"a": 9, "b": 9, "c": 1}}}}),
    );
    assert_error(&source.replace("{ x = P { a = 9 } }", "{}"), "E0608", &[]);

    let source = "struct P { a: Int = 7, b: Int = self.a, c: Int = 0 }\n\
        fn bad() -> Option<Map<Str, P>> = fail(\"unused optional map\")\n\
        struct W { m: Option<Map<Str, P>> = bad() }\n\
        fn mk(k: Int) -> Option<Map<Str, P>> = none\n\
        struct Root { k: Int = 0, w: W }\nw.m = mk(self.k)\nw.m.x.c = force(1)";
    assert_value(
        source,
        json!({"k": 0, "w": {"m": {"x": {"a": 7, "b": 7, "c": 1}}}}),
    );
}

fn wide_structural_program(size: usize) -> (String, Value) {
    let fields = (0..size)
        .map(|i| format!("f{i}: Item"))
        .collect::<Vec<_>>()
        .join(", ");
    let body = (0..size)
        .map(|i| format!("big.f{i}.v = 1\n"))
        .collect::<String>();
    let expected = (0..size)
        .map(|i| (format!("f{i}"), json!({"v": 1})))
        .collect::<serde_json::Map<_, _>>();
    (
        format!(
            "struct Item {{ v: Int }}\nstruct Big {{ {fields} }}\n\
        fn bad() -> Big = fail(\"unused whole\")\nstruct Root {{ big: Big = bad() }}\n{body}"
        ),
        json!({"big": expected}),
    )
}

#[test]
fn wide_structural_fields_cover_read_free_defaults() {
    let (source, expected) = wide_structural_program(192);
    assert_value(&source, expected);
    assert_error(&source.replace("big.f191.v = 1\n", ""), "E0608", &[]);
}

#[test]
#[ignore = "reports coverage timing for generated wide structs of structs"]
fn wide_structural_defaults_timing() {
    let size = std::env::var("STAV_STRUCTURAL_DEFAULTS_BENCH_SIZE")
        .map(|value| value.parse::<usize>().expect("the size must be an integer"))
        .unwrap_or(3_200);
    assert!(size > 0, "the size must be positive");
    for size in [(size / 4).max(1), (size / 2).max(1), size] {
        let (source, expected) = wide_structural_program(size);
        let started = std::time::Instant::now();
        let program = checked(&source);
        let check_time = started.elapsed();
        let started = std::time::Instant::now();
        let evaluated = program.eval(&Limits::default()).unwrap();
        let eval_time = started.elapsed();
        let actual: Value = serde_json::from_str(&evaluated.to_canonical_json()).unwrap();
        assert_eq!(actual, expected);
        eprintln!("size={size} structural check={check_time:?} eval={eval_time:?}");
    }
}

#[test]
fn exposed_read_free_map_shapes_refresh_optional_fallback_checks() {
    let source = "struct S { v: Int = 0 }\n\
        fn opt(a: Int) -> Option<S> = S { v = a }\n\
        struct P { o: Option<S> = opt(self.a), a: Int = 0 }\n\
        fn base() -> Map<Str, P> = { x = P { o = none } }\n\
        struct W { m: Map<Str, P> = base() }\n\
        fn mk(k: Int) -> Map<Str, P> = {}\n\
        struct Root { k: Int = 0, w: W }\nw.m = mk(self.k)\nw.m.x.a = force(1)";
    assert_value(
        source,
        json!({"k": 0, "w": {"m": {"x": {"a": 1, "o": null}}}}),
    );
}

#[test]
fn optional_wholes_without_created_instances_do_not_register_inner_defaults() {
    for default in ["", " = { a = 1 }"] {
        for (result, expected) in [
            ("none", json!(null)),
            ("P { a = 2, b = 3 }", json!({"a": 2, "b": 3})),
        ] {
            let source = format!(
                "struct P {{ a: Int = self.b, b: Int = self.a }}\n\
                 struct W {{ p: Option<P>{default} }}\n\
                 fn mkp(k: Int) -> Option<P> = {result}\n\
                 struct Root {{ k: Int = 0, w: W }}\nw.p = mkp(self.k)"
            );
            assert_value(&source, json!({"k": 0, "w": {"p": expected}}));
        }
        for result in ["none", "P { a = 2, b = 3 }"] {
            let source = format!(
                "struct P {{ a: Int = self.b, b: Int = self.a, c: Int = 0 }}\n\
                 struct W {{ p: Option<P>{default} }}\n\
                 fn mkp(k: Int) -> Option<P> = {result}\n\
                 struct Root {{ k: Int = 0, w: W }}\n\
                 w.p = mkp(self.k)\nw.p.c = force(1)"
            );
            assert_error(
                &source,
                "E0607",
                &["`w.p.a` reads `w.p.b`", "`w.p.b` reads `w.p.a`"],
            );
        }
    }
}

#[test]
fn conditional_enclosing_map_literals_can_create_fallback_instances() {
    for literal in ["{ a = 1 }", "{}"] {
        let source = format!(
            "struct P {{ a: Int = self.b, b: Int = self.a }}\n\
             struct Box {{ seed: Int = 0, m: Map<Str, P> = {{ x = {literal} }} }}\n\
             fn mk(k: Int) -> Option<Box> = none\n\
             struct Root {{ k: Int = 0, o: Option<Box> }}\n\
             o = mk(self.k)\no.seed = force(1)"
        );
        if literal == "{}" {
            assert_error(
                &source,
                "E0607",
                &["`o.m.x.a` reads `o.m.x.b`", "`o.m.x.b` reads `o.m.x.a`"],
            );
        } else {
            assert_value(
                &source,
                json!({"k": 0, "o": {"seed": 1, "m": {"x": {"a": 1, "b": 1}}}}),
            );
        }
    }
}

#[test]
fn computed_default_map_keys_preserve_literal_fields_and_suspended_values() {
    for key in ["x", "\"#{key()}\""] {
        for value in ["self.n", "1 / 0", "1"] {
            let source = format!(
                "struct S {{ a: Int, b: Int = self.a }}\nfn key() -> Str = \"x\"\n\
                 struct Root {{ n: Int = 1, m: Map<Str, S> = {{ {key} = {{ a = {value} }} }} }}\n\
                 m.x.a = force(2)"
            );
            assert_value(&source, json!({"n": 1, "m": {"x": {"a": 2, "b": 2}}}));
        }
        let source = format!(
            "struct S {{ a: Int, b: Int = self.a }}\nfn key() -> Str = \"x\"\n\
             struct Root {{ n: Int = self.m[\"x\"].b, m: Map<Str, S> = {{ {key} = {{ a = self.n }} }} }}\n\
             m.x.a = force(2)"
        );
        if key == "x" {
            assert_value(&source, json!({"n": 2, "m": {"x": {"a": 2, "b": 2}}}));
        } else {
            assert_error(&source, "E0607", &["`n` reads `m`", "`m` reads `n`"]);
        }
    }
}

#[test]
fn default_map_key_cycles_use_literal_children_or_the_map_field() {
    for optional in [false, true] {
        for key in ["x", "\"x\"", "\"#{key()}\"", "\"#{1 / 0}\""] {
            for sibling in ["", ", y = { a = 3 }"] {
                let (root, whole, path) = if optional {
                    ("k: Int = 0, o: Option<Box>", "o = mk(self.k)\n", "o")
                } else {
                    ("box: Box", "", "box")
                };
                let source = format!(
                    "struct S {{ a: Int, b: Int = self.a }}\nfn key() -> Str = \"x\"\n\
                     struct Box {{ n: Int = self.m[\"x\"].b, m: Map<Str, S> = {{ {key} = {{ a = self.n }}{sibling} }} }}\n\
                     fn mk(k: Int) -> Option<Box> = none\nstruct Root {{ {root} }}\n\
                     {whole}{path}.m.x.a = force(2)"
                );
                if key.contains("#{") {
                    let error =
                        evaluate(&source).expect_err("computed keys must retain map-level reads");
                    let diagnostics: Vec<_> = error.iter().collect();
                    assert_eq!(diagnostics.len(), 1, "{error}");
                    let diagnostic = &diagnostics[0].diagnostic;
                    assert_eq!(diagnostic.code, "E0607", "{error}");
                    let mut labels: Vec<_> = diagnostic
                        .labels
                        .iter()
                        .map(|label| label.message.clone())
                        .collect();
                    labels.sort();
                    let mut expected = vec![
                        format!("`{path}.n` reads `{path}.m`"),
                        format!("`{path}.m` reads `{path}.n`"),
                    ];
                    expected.sort();
                    assert_eq!(labels, expected, "{error}");
                } else {
                    let mut entries = json!({"x": {"a": 2, "b": 2}});
                    if !sibling.is_empty() {
                        entries["y"] = json!({"a": 3, "b": 3});
                    }
                    let expected = if optional {
                        json!({"k": 0, "o": {"n": 2, "m": entries}})
                    } else {
                        json!({"box": {"n": 2, "m": entries}})
                    };
                    assert_value(&source, expected);
                }
            }
        }
    }
}

#[test]
fn mixed_default_map_keys_keep_literal_entry_reads_on_their_children() {
    for optional in [false, true] {
        let (root, whole, path) = if optional {
            ("k: Int = 0, o: Option<Box>", "o = mk(self.k)\n", "o")
        } else {
            ("box: Box", "", "box")
        };
        let source = format!(
            "struct S {{ a: Int, b: Int = self.a }}\nfn key() -> Str = \"y\"\n\
             struct Box {{ n: Int = self.m[\"x\"].b, m: Map<Str, S> = {{ x = {{ a = self.n }}, \"#{{key()}}\" = {{ a = 3 }} }} }}\n\
             fn mk(k: Int) -> Option<Box> = none\nstruct Root {{ {root} }}\n\
             {whole}{path}.m.x.a = force(2)"
        );
        let value = json!({"n": 2, "m": {"x": {"a": 2, "b": 2}, "y": {"a": 3, "b": 3}}});
        assert_value(
            &source,
            if optional {
                json!({"k": 0, "o": value})
            } else {
                json!({"box": value})
            },
        );
    }
}

#[test]
fn computed_default_keys_do_not_hide_known_child_default_cycles() {
    for literal in ["x = {}, ", ""] {
        for optional in [false, true] {
            let map = format!("Map<Str, S> = {{ {literal}\"#{{1 / 0}}\" = {{}} }}");
            let (declarations, whole, path) = if optional {
                (
                    format!(
                        "struct Box {{ m: {map} }}\n\
                         fn mk(k: Int) -> Option<Box> = none\n\
                         struct Root {{ k: Int = 0, o: Option<Box> }}"
                    ),
                    "o = mk(self.k)\n",
                    "o.m",
                )
            } else {
                (format!("struct Root {{ m: {map} }}"), "", "m")
            };
            let forced = if optional || literal.is_empty() {
                format!("{path}.x.c = force(1)")
            } else {
                String::new()
            };
            let source = format!(
                "struct S {{ a: Int = self.b, b: Int = self.a, c: Int = 0 }}\n\
                 {declarations}\n{whole}{forced}"
            );
            let error = evaluate(&source).expect_err("both instances must retain their cycles");
            let diagnostics: Vec<_> = error.iter().collect();
            assert_eq!(diagnostics.len(), 2, "{error}\n{source}");
            for diagnostic in diagnostics {
                assert_eq!(diagnostic.diagnostic.code, "E0607", "{error}");
                assert_eq!(diagnostic.diagnostic.labels.len(), 2, "{error}");
            }
            let display = error.to_string();
            for instance in [format!("{path}.x"), format!("{path}.#0")] {
                for (from, to) in [("a", "b"), ("b", "a")] {
                    let edge = format!("`{instance}.{from}` reads `{instance}.{to}`");
                    assert!(display.contains(&edge), "missing edge {edge}:\n{display}");
                }
            }
        }
    }
}

#[test]
fn known_map_children_keep_covered_defaults_suspended_while_keys_are_unknown() {
    for literal in ["x = { a = 1 }, ", ""] {
        let source = format!(
            "struct S {{ a: Int = 1 / 0, b: Int = self.a, c: Int = 0 }}\n\
             struct Root {{ m: Map<Str, S> = {{ {literal}\"#{{key()}}\" = {{ a = 3 }} }} }}\n\
             fn key() -> Str = \"y\"\nm.x.a = force(2)"
        );
        assert_value(
            &source,
            json!({"m": {"x": {"a": 2, "b": 2, "c": 0}, "y": {"a": 3, "b": 3, "c": 0}}}),
        );
    }
    assert_value(
        "struct S { a: Int = 1 / 0, b: Int = self.a, c: Int = 0 }\n\
         struct Root { m: Map<Str, S> = { \"#{key()}\" = { a = 3 } } }\n\
         fn key() -> Str = \"x\"\nm.x.c = force(1)",
        json!({"m": {"x": {"a": 3, "b": 3, "c": 1}}}),
    );
    assert_error(
        "struct S { a: Int = self.b, b: Int = self.a }\n\
         struct Root { m: Map<Str, S> = { x = { a = 1 }, \"#{1 / 0}\" = {} } }",
        "E0607",
        &["`m.#0.a` reads `m.#0.b`", "`m.#0.b` reads `m.#0.a`"],
    );
}

#[test]
fn computed_fallback_map_values_suspend_children_until_after_merging() {
    for value in ["self.n", "1 / 0", "1"] {
        let source = format!(
            "struct S {{ a: Int, b: Int = self.a }}\nfn key() -> Str = \"x\"\n\
             struct Box {{ n: Int = 1, m: Map<Str, S> = {{ \"#{{key()}}\" = {{ a = {value} }} }} }}\n\
             fn mk(k: Int) -> Option<Box> = none\nstruct Root {{ k: Int = 0, o: Option<Box> }}\n\
             o = mk(self.k)\no.m.x.a = force(2)"
        );
        assert_value(
            &source,
            json!({"k": 0, "o": {"n": 1, "m": {"x": {"a": 2, "b": 2}}}}),
        );
    }
}

#[test]
fn supplied_optional_wholes_skip_losing_computed_default_keys() {
    assert_value(
        "struct S { a: Int, b: Int = self.a }\n\
         struct Box { n: Int = 0, m: Map<Str, S> = { \"#{1 / 0}\" = { a = self.n } } }\n\
         fn mk(k: Int) -> Option<Box> = Box { n = 1, m = {} }\n\
         struct Root { k: Int = 0, o: Option<Box> }\n\
         o = mk(self.k)\no.n = force(2)",
        json!({"k": 0, "o": {"n": 2, "m": {}}}),
    );
}

#[test]
fn replaced_computed_default_keys_never_execute() {
    assert_value(
        "struct Root { m: Option<Map<Str, Int>> = { \"#{1 / 0}\" = 1 } }\nm = none",
        json!({"m": null}),
    );
    for entries in [
        "\"#{key()}\" = 1, \"#{key()}\" = 2",
        "x = 1, \"#{key()}\" = 2",
        "\"#{key()}\" = 1, x = 2",
    ] {
        assert_error(
            &format!(
                "fn key() -> Str = \"x\"\n\
                 struct Root {{ m: Map<Str, Int> = {{ {entries} }} }}"
            ),
            "E0605",
            &[],
        );
    }
}

fn wide_created_program(size: usize, first: bool) -> String {
    let fields = (0..size)
        .map(|i| format!("f{i}: Int = 0"))
        .collect::<Vec<_>>()
        .join(", ");
    let creator = if first { 0 } else { size - 1 };
    format!(
        "struct Big {{ {fields} }}\nstruct Root {{ k: Int = 0, m: Map<Str, Big> = {{}} }}\n\
        fn mk(k: Int) -> Map<Str, Big> = {{}}\nm = mk(self.k)\nm.x.f{creator} = force(1)"
    )
}

#[test]
#[ignore = "reports fallback eligibility timing for wide created map entries"]
fn wide_created_defaults_timing() {
    let size = std::env::var("STAV_CREATED_DEFAULTS_BENCH_SIZE")
        .map(|value| value.parse::<usize>().expect("the size must be an integer"))
        .unwrap_or(3_200);
    assert!(size > 0, "the size must be positive");
    for size in [(size / 4).max(1), (size / 2).max(1), size] {
        for first in [false, true] {
            let source = wide_created_program(size, first);
            let started = std::time::Instant::now();
            let program = checked(&source);
            let check_time = started.elapsed();
            let started = std::time::Instant::now();
            let evaluated = program.eval(&Limits::default()).unwrap();
            let eval_time = started.elapsed();
            let actual: Value = serde_json::from_str(&evaluated.to_canonical_json()).unwrap();
            for i in 0..size {
                let creator = if first { 0 } else { size - 1 };
                assert_eq!(
                    actual["m"]["x"][format!("f{i}")],
                    json!(usize::from(i == creator))
                );
            }
            eprintln!("size={size} first={first} created check={check_time:?} eval={eval_time:?}");
        }
    }
}

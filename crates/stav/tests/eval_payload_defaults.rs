//! Exercises eager variant payload defaults and whole-default coverage.

use serde_json::{Value, json};
use stav::{Diagnostics, Host, ImportRoot, Limits, MemoryLoader, RootSpec, check};

fn evaluate(source: &str) -> Result<Value, Diagnostics> {
    evaluate_with_limits(source, &Limits::default())
}

fn evaluate_with_limits(source: &str, limits: &Limits) -> Result<Value, Diagnostics> {
    let host = Host::new(MemoryLoader::new().with_file("app/main.stav", source));
    let root = RootSpec {
        root_struct: "Root".to_owned(),
        entries: vec!["app/main.stav".into()],
        import_roots: vec![ImportRoot::new("app")],
    };
    let checked = check(&host, &root)
        .unwrap_or_else(|errors| panic!("the program must check:\n{errors}\n{source}"));
    let evaluated = checked.eval(limits)?;
    Ok(serde_json::from_str(&evaluated.to_canonical_json()).expect("canonical JSON must parse"))
}

fn assert_value(source: &str, expected: Value) {
    let value = evaluate(source)
        .unwrap_or_else(|errors| panic!("the program must evaluate:\n{errors}\n{source}"));
    assert_eq!(value, expected, "{source}");
}

fn assert_error(source: &str, code: &str) {
    let errors = evaluate(source).expect_err("evaluation must reject the program");
    let codes: Vec<_> = errors.iter().map(|d| d.diagnostic.code).collect();
    assert_eq!(codes, [code], "{errors}\n{source}");
}

const BUILD: &str = "type Build = cargo(n: Int = 1 / 0, kept: Int = 4) | meson(o: Int = 0)";

#[test]
fn higher_payload_fields_do_not_skip_constructor_defaults() {
    for (declarations, expression) in [
        ("", "cargo(kept = 5)"),
        ("fn mk() -> Build = cargo(kept = 5)", "mk()"),
        ("let built = Build.cargo(kept = 5)", "built"),
        (
            "fn identity(b: Build) -> Build = b",
            "identity(Build.cargo(kept = 5))",
        ),
    ] {
        for (field, statement) in [
            (format!("build: Build = {expression}"), "".to_owned()),
            ("build: Build".to_owned(), format!("build = {expression}")),
            (
                "k: Int = 0, build: Build".to_owned(),
                format!(
                    "build = default(if self.k == 0 {{ {expression} }} else {{ Build.meson() }})"
                ),
            ),
        ] {
            assert_error(
                &format!(
                    "{BUILD}\n{declarations}\nstruct Root {{ {field} }}\n{statement}\nbuild cargo {{ n = 7 }}"
                ),
                "E0603",
            );
        }
    }
}

#[test]
fn omitted_payload_defaults_report_errors_without_higher_contributions() {
    for (declarations, expression) in [
        ("", "cargo(kept = 5)"),
        ("fn mk() -> Build = cargo(kept = 5)", "mk()"),
    ] {
        assert_error(
            &format!("{BUILD}\n{declarations}\nstruct Root {{ build: Build = {expression} }}"),
            "E0603",
        );
    }
}

#[test]
fn explicit_payload_arguments_remain_eager_even_when_replaced_or_ignored() {
    assert_error(
        &format!(
            "{BUILD}\nstruct Root {{ build: Build = Build.cargo(n = 1 / 0, kept = 5) }}\nbuild cargo {{ n = 7 }}"
        ),
        "E0603",
    );
    for expression in ["Build.cargo(n = 1 / 0, kept = 5)", "Build.meson(o = 1 / 0)"] {
        assert_error(
            &format!(
                "{BUILD}\nfn ignore(b: Build) -> Int = 7\nstruct Root {{ n: Int = ignore({expression}) }}"
            ),
            "E0603",
        );
    }
}

#[test]
fn ignored_variant_values_and_payload_fields_still_evaluate_defaults() {
    for expression in [
        "ignore(Build.cargo(kept = 5))",
        "match Build.cargo(kept = 5) { Build.cargo(kept = k, ..) => k, Build.meson(..) => 0 }",
        "match Build.cargo(kept = 5) { Build.cargo(n = unused, kept = k) => k, Build.meson(..) => 0 }",
        "let b = Build.cargo(kept = 5) in ignore(b)",
        "ignore_list([Build.cargo(kept = 5)])",
        "ignore_map({ x = Build.cargo(kept = 5) })",
    ] {
        assert_error(
            &format!(
                "{BUILD}\nfn ignore(b: Build) -> Int = 5\nfn ignore_list(b: List<Build>) -> Int = 5\nfn ignore_map(b: Map<Str, Build>) -> Int = 5\nstruct Root {{ n: Int = {expression} }}"
            ),
            "E0603",
        );
    }
}

#[test]
fn matching_evaluates_selected_constructor_defaults_before_patterns() {
    for expression in [
        "match Build.cargo(kept = 5) { Build.cargo(n, ..) => n, Build.meson(..) => 0 }",
        "match Build.cargo(kept = 5) { Build.cargo(n = 0, ..) => 1, _ => 2 }",
    ] {
        assert_error(
            &format!("{BUILD}\nstruct Root {{ n: Int = {expression} }}"),
            "E0603",
        );
    }
    assert_value(
        &format!(
            "{BUILD}\nstruct Root {{ n: Int = match Build.meson() {{ Build.cargo(n, ..) => n, Build.meson(o) => o }} }}"
        ),
        json!({"n": 0}),
    );
}

#[test]
fn higher_whole_values_skip_constructor_defaults() {
    for (declarations, expression) in [
        ("", "cargo(kept = 5)"),
        ("fn mk() -> Build = cargo(kept = 5)", "mk()"),
    ] {
        assert_value(
            &format!(
                "{BUILD}\n{declarations}\nstruct Root {{ build: Build = {expression} }}\nbuild = meson(o = 9)"
            ),
            json!({"build": {"meson": {"o": 9}}}),
        );
    }
}

#[test]
fn nested_lists_and_maps_contain_complete_variant_payloads() {
    let declarations =
        "type B = item(n: Int = 6, kept: Int = 4)\nstruct Root { values: List<Map<Str, B>> }";
    assert_value(
        &format!("{declarations}\nvalues = [{{ x = B.item(kept = 5) }}]"),
        json!({"values": [{"x": {"item": {"n": 6, "kept": 5}}}]}),
    );
    assert_error(
        &format!(
            "{BUILD}\nstruct Root {{ n: Int = match [{{ x = Build.cargo(kept = 5) }}][0][\"x\"] {{ Build.cargo(kept = k, ..) => k, Build.meson(..) => 0 }} }}"
        ),
        "E0603",
    );
}

#[test]
fn equality_compares_complete_payload_values() {
    assert_value(
        "type B = item(n: Int = 6)\nstruct Root { same: Bool = B.item() == B.item(n = 6), different: Bool = [B.item()] != [B.item(n = 7)] }",
        json!({"same": true, "different": true}),
    );
    assert_error(
        &format!(
            "{BUILD}\nstruct Root {{ same: Bool = Build.cargo(kept = 5) == Build.cargo(n = 6, kept = 5) }}"
        ),
        "E0603",
    );
}

#[test]
fn payload_refinements_run_even_when_fields_are_ignored_or_replaced() {
    let declarations = "type B = item(n: Int where 1..=9 = 0, kept: Int = 5)\nfn keep(b: B) -> Int = match b { B.item(kept = k, ..) => k }";
    for body in [
        "struct Root { n: Int = keep(B.item()) }",
        "struct Root { b: B = B.item() }\nb item { n = 7 }",
        "struct Root { b: B = B.item() }",
        "struct Root { n: Int = keep(B.item(n = 0)) }",
    ] {
        assert_error(&format!("{declarations}\n{body}"), "E0803");
    }
}

#[test]
fn constructor_arguments_keep_their_parameter_order() {
    assert_error(
        "type B = item(a: Int, b: Int)\nstruct Root { b: B }\nb = B.item(b = 1 / 0, a = 9223372036854775807 + 1)",
        "E0601",
    );
}

#[test]
fn ignored_or_replaced_payload_defaults_report_other_runtime_errors() {
    for (default, code) in [("fail(\"unused\")", "E0608"), ("[1][4]", "E0604")] {
        let declarations = format!("type B = item(n: Int = {default}, kept: Int = 5)");
        for body in [
            "struct Root { b: B = B.item() }\nb item { n = 7 }",
            "struct Root { n: Int = match B.item() { B.item(kept = k, ..) => k } }",
        ] {
            assert_error(&format!("{declarations}\n{body}"), code);
        }
    }
}

#[test]
fn collection_membership_compares_complete_payload_values() {
    assert_value(
        "type B = item(n: Int = 6)\nstruct Root { contains: Bool = contains([B.item()], B.item(n = 6)), missing: Bool = contains([B.item()], B.item(n = 7)) }",
        json!({"contains": true, "missing": false}),
    );
    assert_error(
        &format!(
            "{BUILD}\nstruct Root {{ result: Bool = contains([Build.cargo()], Build.cargo(n = 6)) }}"
        ),
        "E0603",
    );
}

#[test]
fn collection_shape_builtins_and_higher_order_calls_evaluate_payload_defaults() {
    for expression in [
        "len([Build.cargo()])",
        "len(keys({ x = Build.cargo() }))",
        "match first([Build.cargo()]) { none => 0, b => match b { Build.cargo(kept = k, ..) => k, Build.meson(..) => 0 } }",
        "fold([Build.cargo()], 0, fn(acc: Int, b: Build) -> Int = acc + 1)",
    ] {
        assert_error(
            &format!("{BUILD}\nstruct Root {{ n: Int = {expression} }}"),
            "E0603",
        );
    }
}

#[test]
fn nested_payload_defaults_are_evaluated_before_matching() {
    let declarations = "type Inner = item(n: Int = 1 / 0, kept: Int = 5)\ntype Outer = wrap(inner: Inner = Inner.item())";
    assert_error(
        &format!(
            "{declarations}\nstruct Root {{ n: Int = match Outer.wrap() {{ Outer.wrap(inner = Inner.item(kept = k, ..)) => k }} }}"
        ),
        "E0603",
    );
}

#[test]
fn constructor_defaults_are_evaluated_before_custom_refinements() {
    for predicate in ["match b { B.item(kept = k, ..) => k == 5 }", "false"] {
        assert_error(
            &format!(
                "type B = item(n: Int = 1 / 0, kept: Int = 5)\nfn good(b: B) -> Bool = {predicate}\nfn keep(b: B where good) -> Int = 7\nstruct Root {{ n: Int = keep(B.item()) }}"
            ),
            "E0603",
        );
    }
}

#[test]
fn read_free_declared_payload_defaults_are_cached_across_constructions() {
    let source = "type B = item(n: Int = len(range(0, 10000)))\nfn read(b: B) -> Int = match b { B.item(n) => n + n + n }\nstruct Root { a: Int = read(B.item()), b: Int = read(B.item()) }";
    let limits = Limits {
        steps: 15000,
        ..Limits::default()
    };
    let value = evaluate_with_limits(source, &limits).unwrap_or_else(|errors| {
        panic!("cached defaults must stay within the step limit:\n{errors}")
    });
    assert_eq!(value, json!({"a": 30000, "b": 30000}));
}

#[test]
fn equal_captured_payload_values_keep_part_inclusions_idempotent() {
    let source = "type B = item(n: Int = 6)\nstruct Root { xs: List<Int> = [] }\nfn p(b: B) -> Part<Root> = part { xs = [1] }\ninclude p(B.item())\ninclude p(B.item(n = 6))\ninclude p(B.item(n = 7))";
    assert_value(source, json!({"xs": [1, 1]}));
}

#[test]
fn optional_payload_defaults_compare_by_their_values_in_part_identities() {
    assert_value(
        "type B = item(n: Option<Int> = none)\nstruct Root { xs: List<Int> = [] }\nfn p(b: B) -> Part<Root> = part { xs = [1] }\ninclude p(B.item())\ninclude p(B.item(n = none))\ninclude p(B.item(n = 7))",
        json!({"xs": [1, 1]}),
    );
}

#[test]
fn closure_captures_use_complete_payload_values_for_part_identities() {
    assert_value(
        "type B = item(n: Int = 6)\nstruct Root { xs: List<Int> = [] }\nfn capture(b: B) -> Fn() -> B = fn() -> B = b\nfn p(f: Fn() -> B) -> Part<Root> = part { xs = [1] }\ninclude p(capture(B.item()))\ninclude p(capture(B.item(n = 6)))\ninclude p(capture(B.item(n = 7)))",
        json!({"xs": [1, 1]}),
    );
}

#[test]
fn references_inside_payload_defaults_are_validated() {
    assert_error(
        "struct Service {}\ntype B = item(target: Ref<Service> = missing)\nstruct Root { services: Map<Str, Service> = {} block service, b: B = B.item() }",
        "E0802",
    );
    assert_value(
        "struct Service {}\ntype B = item(target: Ref<Service> = active)\nstruct Root { services: Map<Str, Service> = {} block service, b: List<B> = [B.item()] }\nservice active {}",
        json!({"services": {"active": {}}, "b": [{"item": {"target": "active"}}]}),
    );
}

#[test]
fn public_values_render_and_deserialize_complete_payloads() {
    let source = "type B = item(n: Int = 6)\nstruct Root { b: List<B> = [B.item()] }";
    let host = Host::new(MemoryLoader::new().with_file("app/main.stav", source));
    let root = RootSpec {
        root_struct: "Root".to_owned(),
        entries: vec!["app/main.stav".into()],
        import_roots: vec![ImportRoot::new("app")],
    };
    let evaluated = check(&host, &root)
        .unwrap()
        .eval(&Limits::default())
        .unwrap();
    let expected = json!({"b": [{"item": {"n": 6}}]});
    let converted: Value = evaluated
        .get(&stav::AttrPath::new(std::iter::empty::<String>()))
        .unwrap();
    assert_eq!(converted, expected);
    for format in [stav::Format::Json, stav::Format::Toml, stav::Format::Yaml] {
        let rendered = evaluated.render(format).unwrap();
        assert!(rendered.contains('6'), "{rendered}");
        assert!(!rendered.contains("null"), "{rendered}");
    }
}

#[test]
fn comparisons_evaluate_defaults_even_when_payloads_cannot_affect_the_result() {
    for expression in [
        "Build.cargo() != Build.meson()",
        "contains(empty, Build.cargo())",
        "[Build.cargo()] == empty",
    ] {
        assert_error(
            &format!(
                "{BUILD}\nlet empty: List<Build> = []\nstruct Root {{ result: Bool = {expression} }}"
            ),
            "E0603",
        );
    }
    assert_error(
        "type B = item(kept: Int = 5, n: Int = 1 / 0)\nstruct Root { same: Bool = B.item() == B.item(kept = 6) }",
        "E0603",
    );
}

#[test]
fn unrelated_function_and_struct_defaults_remain_eager() {
    assert_error(
        "fn ignore(n: Int = 1 / 0) -> Int = 5\nstruct Root { n: Int = ignore() }",
        "E0603",
    );
    assert_error(
        "struct S { n: Int = 1 / 0 }\nfn ignore(s: S) -> Int = 5\nstruct Root { n: Int = ignore(S {}) }",
        "E0603",
    );
}

#[test]
fn unused_top_level_and_body_let_values_evaluate_payload_defaults() {
    for body in [
        "let top = mk()\nstruct Root { n: Int = 5 }",
        "struct Root { n: Int = 5 }\nlet local = mk()",
    ] {
        assert_error(
            &format!("{BUILD}\nfn mk() -> Build = Build.cargo()\n{body}"),
            "E0603",
        );
    }
}

#[test]
fn written_struct_defaults_split_before_higher_fields_are_merged() {
    assert_value(
        "struct S { a: Int = 1 / 0, b: Int = 0 }\nstruct Root { s: S = { b = 5 } }\ns.a = 7",
        json!({"s": {"a": 7, "b": 5}}),
    );
}

#[test]
fn tagged_selections_do_not_cover_whole_constructor_defaults() {
    for selection in ["cargo { n = 7, kept = 9 }", "meson { o = 9 }"] {
        assert_error(
            &format!(
                "{BUILD}\nstruct Root {{ build: Build = cargo(kept = 5) }}\nbuild {selection}"
            ),
            "E0603",
        );
    }
}

#[test]
fn optional_payload_binding_matches_the_completed_none_value() {
    assert_value(
        "type B = item(n: Option<Int> = none)\nstruct Root { result: Int = match B.item() { B.item(n = x) => 1, _ => 2 } }",
        json!({"result": 2}),
    );
}

#[test]
fn equal_completed_payloads_merge_without_conflicting() {
    assert_value(
        "type B = item(n: Int = 6)\nstruct Root { b: B }\nb = B.item()\nb = B.item(n = 6)",
        json!({"b": {"item": {"n": 6}}}),
    );
}

#[test]
fn compound_payload_values_determine_part_identity() {
    assert_value(
        "type B = item(ns: List<Int> = [2])\nstruct Root { xs: List<Int> = [] }\nfn p(b: B) -> Part<Root> = part { xs = [1] }\ninclude p(B.item(ns = [1, 1]))\ninclude p(B.item())\ninclude p(B.item(ns = [2]))",
        json!({"xs": [1, 1]}),
    );
}

#[test]
fn unused_parts_still_evaluate_captured_constructor_defaults() {
    assert_error(
        "type B = item(n: Int = 1 / 0)\nstruct Root { xs: List<Int> = [] }\nfn p(b: B) -> Part<Root> = part { xs = [1] }\nlet unused = p(B.item())",
        "E0603",
    );
}

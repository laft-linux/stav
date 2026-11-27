//! Exercises static dependencies of instances in computed-key defaults.

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
        .unwrap_or_else(|diagnostics| panic!("the program must check:\n{diagnostics}\n{source}"));
    let value = checked.eval(&Limits::default())?;
    Ok(serde_json::from_str(&value.to_canonical_json()).expect("canonical JSON must parse"))
}

fn assert_value(source: &str, expected: Value) {
    let actual = evaluate(source).unwrap_or_else(|diagnostics| {
        panic!("the program must evaluate:\n{diagnostics}\n{source}")
    });
    assert_eq!(actual, expected, "{source}");
}

fn assert_cycle(source: &str, path: &str) {
    let error = evaluate(source).expect_err("evaluation must reject the cycle");
    let diagnostics: Vec<_> = error.iter().collect();
    assert_eq!(diagnostics.len(), 1, "{error}\n{source}");
    let diagnostic = &diagnostics[0].diagnostic;
    assert_eq!(diagnostic.code, "E0607", "{error}\n{source}");
    assert_eq!(diagnostic.labels.len(), 2, "{error}\n{source}");
    let display = error.to_string();
    for edge in [
        format!("`{path}.a` reads `{path}.b`"),
        format!("`{path}.b` reads `{path}.a`"),
    ] {
        assert!(display.contains(&edge), "missing edge {edge}:\n{display}");
    }
    assert!(
        display.contains(&format!("cycle: {path}.a -> {path}.b -> {path}.a")),
        "{display}"
    );
}

const CYCLIC_P: &str = "struct P { a: Int = self.b, b: Int = self.a, c: Int = 0 }";

#[test]
fn computed_entries_check_omitted_defaults_before_resolving_keys() {
    for key in ["\"x\"", "\"#{1 / 0}\""] {
        let source = format!(
            "{CYCLIC_P}\nfn key() -> Str = {key}\n\
             struct Root {{ m: Map<Str, P> = {{ \"#{{key()}}\" = {{ c = 1 }} }} }}"
        );
        assert_cycle(&source, "m.#0");
    }
}

#[test]
fn late_map_results_cannot_hide_computed_entry_default_cycles() {
    for result in ["{}", "{ x = P { a = 1 } }"] {
        let source = format!(
            "{CYCLIC_P}\nfn key() -> Str = \"x\"\n\
             struct W {{ m: Map<Str, P> = {{ \"#{{key()}}\" = {{ c = 1 }} }} }}\n\
             fn mk(k: Int) -> Map<Str, P> = {result}\n\
             struct Root {{ k: Int = 0, w: W }}\nw.m = mk(self.k)"
        );
        assert_cycle(&source, "w.m.#0");
    }
}

#[test]
fn a_present_late_map_result_cannot_hide_computed_entry_default_cycles() {
    let source = format!(
        "{CYCLIC_P}\nfn key() -> Str = \"x\"\n\
         struct W {{ m: Map<Str, P> = {{ \"#{{key()}}\" = {{ c = 1 }} }} }}\n\
         fn mk(k: Int) -> Map<Str, P> = {{ x = P {{ a = 1 }} }}\n\
         struct Root {{ k: Int = 0, w: W }}\nw.m = mk(self.k)"
    );
    assert_cycle(&source, "w.m.#0");
}

#[test]
fn computed_entry_ordinals_ignore_literal_entries() {
    let source = format!(
        "{CYCLIC_P}\nfn key(n: Int) -> Str = \"k#{{n}}\"\n\
         struct Root {{ m: Map<Str, P> = {{\n\
         literal = {{ a = 2 }}, \"#{{key(0)}}\" = {{ a = 3 }},\n\
         other = {{ a = 4 }}, \"#{{key(1)}}\" = {{ c = 1 }}\n}} }}"
    );
    assert_cycle(&source, "m.#1");
}

#[test]
fn an_explicit_key_cannot_override_unknown_placeholder_defaults() {
    for contribution in ["m.x.a = force(7)", "m.x = force({ a = 7, b = 7, c = 7 })"] {
        let source = format!(
            "{CYCLIC_P}\nfn key() -> Str = \"x\"\n\
             struct Root {{ m: Map<Str, P> = {{ \"#{{key()}}\" = {{ c = 1 }} }} }}\n\
             {contribution}"
        );
        assert_cycle(&source, "m.#0");
    }
}

#[test]
fn literal_keys_can_break_cycles_that_unknown_keys_keep() {
    let source = format!(
        "{CYCLIC_P}\n\
         struct Root {{ m: Map<Str, P> = {{ x = {{ c = 1 }} }} }}\n\
         m.x.a = force(7)"
    );
    assert_value(&source, json!({"m": {"x": {"a": 7, "b": 7, "c": 1}}}));
}

#[test]
fn computed_entry_fields_can_break_their_own_default_cycles() {
    for entry in ["{ a = 7 }", "P { a = 7 }", "(P { a = 7 })"] {
        let source = format!(
            "{CYCLIC_P}\nfn key() -> Str = \"x\"\n\
             struct Root {{ m: Map<Str, P> = {{ \"#{{key()}}\" = {entry} }} }}"
        );
        assert_value(&source, json!({"m": {"x": {"a": 7, "b": 7, "c": 0}}}));
    }
}

#[test]
fn placeholder_instances_recurse_through_structs_options_and_maps() {
    for (declaration, value, path) in [
        ("struct H { p: P }", "{ p = { c = 1 } }", "m.#0.p"),
        ("struct H { p: P = { c = 1 } }", "{}", "m.#0.p"),
        ("struct H { p: Option<P> }", "{ p = { c = 1 } }", "m.#0.p"),
        ("struct H { p: Option<P> = { c = 1 } }", "{}", "m.#0.p"),
        (
            "struct H { p: Map<Str, P> }",
            "{ p = { nested = { c = 1 } } }",
            "m.#0.p.nested",
        ),
        (
            "struct H { p: Map<Str, P> = { \"#{key()}\" = { c = 1 } } }",
            "{}",
            "m.#0.p.#0",
        ),
    ] {
        let source = format!(
            "{CYCLIC_P}\nfn key() -> Str = \"x\"\n{declaration}\n\
             struct Root {{ m: Map<Str, H> = {{ \"#{{key()}}\" = {value} }} }}"
        );
        assert_cycle(&source, path);
    }
}

#[test]
fn computed_entries_of_optional_structs_and_nested_maps_have_placeholders() {
    for (ty, entry, path) in [
        ("Option<P>", "{ c = 1 }", "m.#0"),
        ("Map<Str, P>", "{ nested = { c = 1 } }", "m.#0.nested"),
        ("Map<Str, P>", "{ \"#{key()}\" = { c = 1 } }", "m.#0.#0"),
    ] {
        let source = format!(
            "{CYCLIC_P}\nfn key() -> Str = \"x\"\n\
             struct Root {{ m: Map<Str, {ty}> = {{ \"#{{key()}}\" = {entry} }} }}"
        );
        assert_cycle(&source, path);
    }
}

#[test]
fn placeholder_instances_recurse_through_selected_payloads() {
    for (declaration, entry, path) in [
        (
            "type Choice = chosen(p: P) | other\nstruct H { choice: Choice }",
            "{ choice = chosen(p = { c = 1 }) }",
            "m.#0.choice.chosen.p",
        ),
        (
            "type Choice = chosen(p: P = { c = 1 }) | other\nstruct H { choice: Choice = chosen() }",
            "{}",
            "m.#0.choice.chosen.p",
        ),
    ] {
        let source = format!(
            "{CYCLIC_P}\nfn key() -> Str = \"x\"\n{declaration}\n\
             struct Root {{ m: Map<Str, H> = {{ \"#{{key()}}\" = {entry} }} }}"
        );
        assert_cycle(&source, path);
    }
    let source = format!(
        "{CYCLIC_P}\nfn key() -> Str = \"x\"\n\
         type Choice = chosen(p: P) | other\n\
         struct Root {{ m: Map<Str, Choice> = {{ \"#{{key()}}\" = chosen(p = {{ c = 1 }}) }} }}"
    );
    assert_cycle(&source, "m.#0.chosen.p");
}

#[test]
fn a_losing_optional_default_adds_no_placeholder_edges_or_key_evaluation() {
    let source = format!(
        "{CYCLIC_P}\nfn key() -> Str = \"#{{1 / 0}}\"\n\
         struct Root {{ m: Option<Map<Str, P>> = {{ \"#{{key()}}\" = {{ c = 1 }} }} }}\n\
         m = none"
    );
    assert_value(&source, json!({"m": null}));
}

#[test]
fn a_complete_late_struct_excludes_its_losing_computed_default() {
    let source = format!(
        "{CYCLIC_P}\nfn key() -> Str = \"#{{1 / 0}}\"\n\
         struct W {{ m: Map<Str, P> = {{ \"#{{key()}}\" = {{ c = 1 }} }} }}\n\
         fn mk(k: Int) -> W = W {{ m = {{ x = P {{ a = 7 }} }} }}\n\
         struct Root {{ k: Int = 0, w: W }}\nw = mk(self.k)"
    );
    assert_value(
        &source,
        json!({"k": 0, "w": {"m": {"x": {"a": 7, "b": 7, "c": 0}}}}),
    );
}

#[test]
fn reads_written_in_computed_entry_values_still_start_at_the_map() {
    let source = r##"
    struct P { a: Int, b: Int = self.a }
    fn key() -> Str = "x"
    struct Box {
        a: Int = self.m["x"].b
        m: Map<Str, P> = { "#{key()}" = { a = self.a } }
    }
    struct Root { box: Box }
    box.m.x.a = force(2)
    "##;
    let error = evaluate(source).expect_err("the map-level written read must form a cycle");
    let display = error.to_string();
    assert!(display.contains("error[E0607]"), "{display}");
    assert!(display.contains("`box.a` reads `box.m`"), "{display}");
    assert!(display.contains("`box.m` reads `box.a`"), "{display}");
}

#[test]
fn participating_computed_keys_still_evaluate_after_static_validation() {
    let source = r##"
    struct P { a: Int = 1, b: Int = self.a }
    struct Root { m: Map<Str, P> = { "#{1 / 0}" = {} } }
    "##;
    let error = evaluate(source).expect_err("the participating key must fail");
    let codes: Vec<_> = error.iter().map(|d| d.diagnostic.code).collect();
    assert_eq!(codes, ["E0603"], "{error}");
}

#[test]
fn placeholder_names_do_not_collide_with_literal_string_keys() {
    let source = format!(
        "{CYCLIC_P}\nfn key() -> Str = \"x\"\n\
         struct Root {{ m: Map<Str, P> = {{\n\
         \"#0\" = {{ a = 7 }}, \"#{{key()}}\" = {{ c = 1 }}\n}} }}"
    );
    assert_cycle(&source, "m.#0");
}

#[test]
fn opaque_entry_values_keep_the_fields_their_expressions_construct() {
    for (declaration, ty, entry, expected) in [
        ("", "P", "make()", json!({"a": 7, "b": 7, "c": 0})),
        (
            "struct H { p: P }",
            "H",
            "{ p = make() }",
            json!({"p": {"a": 7, "b": 7, "c": 0}}),
        ),
        (
            "type Choice = chosen(p: P) | other",
            "Choice",
            "chosen(p = make())",
            json!({"chosen": {"p": {"a": 7, "b": 7, "c": 0}}}),
        ),
    ] {
        let source = format!(
            "{CYCLIC_P}\nfn key() -> Str = \"x\"\n\
             fn make() -> P = P {{ a = 7 }}\n{declaration}\n\
             struct Root {{ m: Map<Str, {ty}> = {{ \"#{{key()}}\" = {entry} }} }}"
        );
        assert_value(&source, json!({"m": {"x": expected}}));
    }
}

#[test]
fn nested_placeholder_defaults_keep_containment_and_instance_frames() {
    let source = r##"
    struct P { a: Int, b: Int = self.a }
    struct H { a: Int = self.p.b, p: P = { a = self.a } }
    fn key() -> Str = "x"
    struct Root { m: Map<Str, H> = { "#{key()}" = {} } }
    "##;
    let error = evaluate(source).expect_err("nested defaults must form a static cycle");
    let display = error.to_string();
    assert!(display.contains("error[E0607]"), "{display}");
    assert!(display.contains("`m.#0.a` reads `m.#0.p`"), "{display}");
    assert!(display.contains("`m.#0.p.a` reads `m.#0.a`"), "{display}");
}

#[test]
fn explicit_nested_fields_remove_losing_default_cycles() {
    let source = format!(
        "{CYCLIC_P}\nstruct H {{ p: P = {{ c = 1 }} }}\n\
         fn key() -> Str = \"x\"\n\
         struct Root {{ m: Map<Str, H> = {{ \"#{{key()}}\" = {{ p = {{ a = 7 }} }} }} }}"
    );
    assert_value(
        &source,
        json!({"m": {"x": {"p": {"a": 7, "b": 7, "c": 0}}}}),
    );
}

#[test]
fn payload_arguments_do_not_run_before_placeholder_cycle_validation() {
    let source = format!(
        "{CYCLIC_P}\nfn key() -> Str = \"x\"\n\
         type Choice = chosen(p: P, n: Int) | other\n\
         struct Root {{ m: Map<Str, Choice> = {{\n\
         \"#{{key()}}\" = chosen(p = {{ c = 1 }}, n = 1 / 0)\n}} }}"
    );
    assert_cycle(&source, "m.#0.chosen.p");
}

#[test]
fn a_late_struct_replaces_computed_literals_split_from_an_enclosing_default() {
    for key in ["\"x\"", "\"#{1 / 0}\""] {
        let source = format!(
            "{CYCLIC_P}\nstruct W {{ m: Map<Str, P> }}\n\
             fn key() -> Str = {key}\n\
             fn mk(k: Int) -> W = W {{ m = {{}} }}\n\
             struct Root {{ k: Int = 0, w: W = {{ m = {{ \"#{{key()}}\" = {{ c = 1 }} }} }} }}\n\
             w = mk(self.k)"
        );
        assert_value(&source, json!({"k": 0, "w": {"m": {}}}));
    }
}

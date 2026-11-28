//! Checks scalar merging of nullary selections and tagged payload boundaries.

use serde_json::{Value, json};
use stav::{Diagnostics, Host, ImportRoot, Limits, MemoryLoader, RootSpec, check};

const LEVEL: &str = "type Level = debug | info | warn | error";

fn evaluate(source: &str) -> Result<Value, Diagnostics> {
    let host = Host::new(MemoryLoader::new().with_file("app/main.stav", source));
    let root = RootSpec::new(
        "Root",
        vec!["app/main.stav".into()],
        vec![ImportRoot::new("app")],
    );
    let checked = check(&host, &root)
        .unwrap_or_else(|diagnostics| panic!("the program must check:\n{diagnostics}\n{source}"));
    let value = checked.eval(&Limits::default())?;
    Ok(serde_json::from_str(&value.to_canonical_json()).expect("canonical JSON must parse"))
}

fn assert_error(source: &str, code: &str) -> Diagnostics {
    let error = evaluate(source).expect_err("evaluation must reject the program");
    let codes: Vec<_> = error.iter().map(|d| d.diagnostic.code).collect();
    assert_eq!(codes, [code], "{error}\n{source}");
    error
}

#[test]
fn different_nullary_tagged_blocks_conflict_as_scalars() {
    let source = format!(
        "{LEVEL}\nstruct Root {{ level: Level }}\n\
         level info {{}}\nlevel warn {{}}\nlevel error {{}}\n"
    );
    let error = assert_error(&source, "E0501");
    let labels = &error.iter().next().unwrap().diagnostic.labels;
    assert_eq!(labels.len(), 3);
    for (label, variant) in labels.iter().zip(["info", "warn", "error"]) {
        assert!(label.message.contains(variant), "{error}");
        assert!(label.message.contains("plain"), "{error}");
        assert!(source[label.span.start..label.span.end].contains(variant));
    }
}

#[test]
fn equal_nullary_tagged_blocks_merge() {
    let source =
        format!("{LEVEL}\nstruct Root {{ level: Level }}\nlevel warn {{}}\nlevel warn {{}}\n");
    assert_eq!(evaluate(&source).unwrap()["level"], json!("warn"));
}

#[test]
fn nullary_attributes_and_selections_merge_in_both_orders() {
    for attribute_first in [false, true] {
        for (value, conflict) in [("info", true), ("warn", false)] {
            let attribute = format!("level = {value}\n");
            let selection = "level warn {}\n";
            let body = if attribute_first {
                format!("{attribute}{selection}")
            } else {
                format!("{selection}{attribute}")
            };
            let source = format!("{LEVEL}\nstruct Root {{ level: Level }}\n{body}");
            if conflict {
                assert_error(&source, "E0501");
            } else {
                assert_eq!(evaluate(&source).unwrap()["level"], json!("warn"));
            }
        }
    }
}

#[test]
fn nullary_deferred_values_and_selections_merge_in_both_orders() {
    for deferred_first in [false, true] {
        for (value, conflict) in [("info", true), ("warn", false)] {
            let deferred = "level = pick(self.x)\n";
            let selection = "level warn {}\n";
            let body = if deferred_first {
                format!("{deferred}{selection}")
            } else {
                format!("{selection}{deferred}")
            };
            let source = format!(
                "{LEVEL}\nfn pick(x: Int) -> Level = if x == 0 {{ {value} }} else {{ error }}\n\
                 struct Root {{ x: Int = 0, level: Level }}\n{body}"
            );
            if conflict {
                assert_error(&source, "E0501");
            } else {
                assert_eq!(evaluate(&source).unwrap()["level"], json!("warn"));
            }
        }
    }
}

#[test]
fn nullary_selections_use_plain_priority() {
    for (attribute, expected) in [
        ("default(info)", "warn"),
        ("force(info)", "info"),
        ("default(warn)", "warn"),
        ("force(warn)", "warn"),
    ] {
        let source = format!(
            "{LEVEL}\nstruct Root {{ level: Level = error }}\n\
             level warn {{}}\nlevel = {attribute}\n"
        );
        assert_eq!(evaluate(&source).unwrap()["level"], json!(expected));
    }
}

#[test]
fn optional_nullary_selections_merge_deferred_none_and_present_winners() {
    for deferred_first in [false, true] {
        for (value, conflict) in [("none", true), ("info", true), ("warn", false)] {
            let deferred = "level = pick(self.x)\n";
            let selection = "level warn {}\n";
            let body = if deferred_first {
                format!("{deferred}{selection}")
            } else {
                format!("{selection}{deferred}")
            };
            let source = format!(
                "{LEVEL}\nfn pick(x: Int) -> Option<Level> = {value}\n\
                 struct Root {{ x: Int = 0, level: Option<Level> }}\n{body}"
            );
            if conflict {
                assert_error(&source, "E0501");
            } else {
                assert_eq!(evaluate(&source).unwrap()["level"], json!("warn"));
            }
        }
    }
}

#[test]
fn optional_nullary_selections_obey_none_priorities() {
    for (none, expected) in [
        ("default(none)", json!("warn")),
        ("force(none)", Value::Null),
    ] {
        let source = format!(
            "{LEVEL}\nstruct Root {{ level: Option<Level> }}\n\
             level warn {{}}\nlevel = {none}\n"
        );
        assert_eq!(evaluate(&source).unwrap()["level"], expected);
    }
    let source =
        format!("{LEVEL}\nstruct Root {{ level: Option<Level> }}\nlevel warn {{}}\nlevel = none\n");
    assert_error(&source, "E0501");
}

#[test]
fn higher_selections_remove_losing_deferred_reads_and_execution() {
    for ty in ["Level", "Option<Level>"] {
        let source = format!(
            "{LEVEL}\nfn inspect(level: {ty}) -> Int = 0\n\
             fn bad(x: Int) -> {ty} = if 1 / x == 0 {{ info }} else {{ error }}\n\
             struct Root {{ x: Int = inspect(self.level), level: {ty} = bad(self.x) }}\n\
             level warn {{}}\n"
        );
        assert_eq!(evaluate(&source).unwrap()["level"], json!("warn"));
        let source = source.replace(
            &format!("level: {ty} = bad(self.x)"),
            &format!("level: {ty}"),
        ) + "level = default(bad(self.x))\n";
        assert_eq!(evaluate(&source).unwrap()["level"], json!("warn"));
    }
}

#[test]
fn higher_deferred_values_replace_nullary_selections() {
    for (ty, result, expected) in [
        ("Level", "info", json!("info")),
        ("Option<Level>", "info", json!("info")),
        ("Option<Level>", "none", Value::Null),
    ] {
        let source = format!(
            "{LEVEL}\nfn pick(x: Int) -> {ty} = {result}\n\
             struct Root {{ x: Int = 0, level: {ty} }}\n\
             level warn {{}}\nlevel = force(pick(self.x))\n"
        );
        assert_eq!(evaluate(&source).unwrap()["level"], expected);
    }
}

#[test]
fn mixed_payload_sums_keep_tagged_conflicts_for_nullary_variants() {
    for body in [
        "level info {}\nlevel warn {}",
        "level = info\nlevel warn {}",
        "level warn {}\nlevel = info",
    ] {
        let source = format!(
            "type Level = info | warn | detail(n: Int)\n\
             struct Root {{ level: Level }}\n{body}\n"
        );
        assert_error(&source, "E0502");
    }
}

#[test]
fn mixed_payload_sums_keep_late_whole_conflicts() {
    for ty in ["Level", "Option<Level>"] {
        let source = format!(
            "type Level = info | warn | detail(n: Int)\n\
             fn pick(x: Int) -> {ty} = info\n\
             struct Root {{ x: Int = 0, level: {ty} }}\n\
             level = pick(self.x)\nlevel warn {{}}\n"
        );
        assert_error(&source, "E0504");
    }
}

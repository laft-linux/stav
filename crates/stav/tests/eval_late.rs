//! Exercises late whole values, recursive instance fallbacks and merge dependencies
//! through the public checking and evaluation APIs.

use serde_json::{Value, json};
use stav::{Diagnostics, Evaluated, Host, ImportRoot, Limits, MemoryLoader, RootSpec, check};

const DOWNLOAD: &str = r#"
struct Download {
  version: Str
  url: Str = "https://example.org/tool-#{self.version}.tar.gz"
}
"#;

fn checked(source: &str) -> Result<stav::Checked, Diagnostics> {
    let host = Host::new(MemoryLoader::new().with_file("app/main.stav", source));
    let root = RootSpec {
        root_struct: "Root".to_owned(),
        entries: vec!["app/main.stav".into()],
        import_roots: vec![ImportRoot::new("app")],
    };
    check(&host, &root)
}

fn evaluate(source: &str) -> Result<Evaluated, Diagnostics> {
    checked(source)
        .unwrap_or_else(|diagnostics| panic!("the program must check:\n{diagnostics}"))
        .eval(&Limits::default())
}

fn assert_value(source: &str, pointer: &str, expected: Value) {
    let evaluated = evaluate(source)
        .unwrap_or_else(|diagnostics| panic!("the program must evaluate:\n{diagnostics}"));
    let actual: Value =
        serde_json::from_str(&evaluated.to_canonical_json()).expect("canonical JSON must be valid");
    assert_eq!(actual.pointer(pointer), Some(&expected), "{source}");
}

fn assert_eval_error(source: &str, code: &str, path: &str) {
    let diagnostics = evaluate(source).expect_err("evaluation must fail");
    let codes: Vec<_> = diagnostics.iter().map(|d| d.diagnostic.code).collect();
    assert_eq!(codes, [code], "{diagnostics}\n{source}");
    assert!(
        diagnostics.to_string().contains(path),
        "the diagnostic must name {path}:\n{diagnostics}"
    );
}

#[test]
fn late_struct_keeps_computed_declared_defaults() {
    let source = r#"
struct Upstream { url: Str, weight: Int = 10, tags: List<Str> = [] }
struct Root { x: Str = "u", u: Upstream }
fn mk(x: Str) -> Upstream = Upstream { url = x, weight = 20, tags = ["given"] }
u = mk(self.x)
"#;
    assert_value(
        source,
        "/u",
        json!({"url": "u", "weight": 20, "tags": ["given"]}),
    );
}

#[test]
fn late_map_entry_uses_defaults_only_when_the_entry_is_missing() {
    for (entries, url) in [
        ("{}", "https://example.org/tool-2.tar.gz"),
        (r#"{ tool = Download { version = "1", url = "u" } }"#, "u"),
    ] {
        let source = format!(
            "{DOWNLOAD}\nstruct Root {{ k: Str = \"x\", m: Map<Str, Download> }}\n\
             fn mk(k: Str) -> Map<Str, Download> = {entries}\n\
             m = mk(self.k)\nm.tool.version = force(\"2\")\n"
        );
        assert_value(&source, "/m/tool", json!({"version": "2", "url": url}));
    }
}

#[test]
fn missing_map_entry_falls_back_through_multiple_struct_fields() {
    let source = format!(
        r#"{DOWNLOAD}
struct Inner {{ m: Map<Str, Download> }}
struct Box {{ inner: Inner }}
struct Root {{ k: Str = "x", cfg: Box }}
fn mk(k: Str) -> Box = Box {{ inner = Inner {{ m = {{}} }} }}
cfg = mk(self.k)
cfg.inner.m.tool.version = force("2")
"#
    );
    assert_value(
        &source,
        "/cfg/inner/m/tool",
        json!({"version": "2", "url": "https://example.org/tool-2.tar.gz"}),
    );
}

#[test]
fn missing_entry_in_nested_maps_gets_recursive_struct_defaults() {
    let source = format!(
        r#"{DOWNLOAD}
struct Details {{ download: Download }}
struct Root {{ k: Str = "x", m: Map<Str, Map<Str, Details>> }}
fn mk(k: Str) -> Map<Str, Map<Str, Details>> = {{ group = {{}} }}
m = mk(self.k)
m.group.tool.download.version = force("2")
"#
    );
    assert_value(
        &source,
        "/m/group/tool/download",
        json!({"version": "2", "url": "https://example.org/tool-2.tar.gz"}),
    );
}

#[test]
fn late_option_none_creates_an_instance_from_higher_fields() {
    for (value, weight, tags) in [
        ("none", 10, json!([])),
        (
            r#"Upstream { url = "old", weight = 20, tags = ["given"] }"#,
            20,
            json!(["given"]),
        ),
    ] {
        let source = format!(
            r#"
struct Upstream {{ url: Str, weight: Int = 10, tags: List<Str> = [] }}
struct Root {{ x: Str = "x", o: Option<Upstream> }}
fn opt(x: Str) -> Option<Upstream> = {value}
o = opt(self.x)
o.url = force("https://a")
"#
        );
        assert_value(
            &source,
            "/o",
            json!({"url": "https://a", "weight": weight, "tags": tags}),
        );
    }
}

#[test]
fn late_option_none_without_higher_fields_stays_absent() {
    let source = r#"
struct Upstream { url: Str, weight: Int = 10 }
struct Root { x: Str = "x", o: Option<Upstream> }
fn opt(x: Str) -> Option<Upstream> = none
o = opt(self.x)
o = default(none)
"#;
    assert_value(source, "/o", Value::Null);
}

#[test]
fn nested_option_defaults_depend_on_whether_the_whole_supplies_it() {
    for (value, url) in [
        ("none", "https://example.org/tool-2.tar.gz"),
        (r#"Download { version = "1", url = "u" }"#, "u"),
    ] {
        let source = format!(
            r#"{DOWNLOAD}
struct Inner {{ o: Option<Download> }}
struct Box {{ inner: Inner }}
struct Root {{ k: Str = "x", cfg: Box }}
fn mk(k: Str) -> Box = Box {{ inner = Inner {{ o = {value} }} }}
cfg = mk(self.k)
cfg.inner.o.version = force("2")
"#
        );
        assert_value(&source, "/cfg/inner/o", json!({"version": "2", "url": url}));
    }
}

#[test]
fn option_inside_a_supplied_map_entry_can_create_a_missing_instance() {
    let source = format!(
        r#"{DOWNLOAD}
struct Entry {{ o: Option<Download> }}
struct Root {{ k: Str = "x", m: Map<Str, Entry> }}
fn mk(k: Str) -> Map<Str, Entry> = {{ tool = Entry {{ o = none }} }}
m = mk(self.k)
m.tool.o.version = force("2")
"#
    );
    assert_value(
        &source,
        "/m/tool/o",
        json!({"version": "2", "url": "https://example.org/tool-2.tar.gz"}),
    );
}

#[test]
fn map_inside_a_supplied_option_can_create_a_missing_instance() {
    let source = format!(
        r#"{DOWNLOAD}
struct Box {{ m: Map<Str, Download> }}
struct Root {{ k: Str = "x", o: Option<Box> }}
fn mk(k: Str) -> Option<Box> = Box {{ m = {{}} }}
o = mk(self.k)
o.m.tool.version = force("2")
"#
    );
    assert_value(
        &source,
        "/o/m/tool",
        json!({"version": "2", "url": "https://example.org/tool-2.tar.gz"}),
    );
}

#[test]
fn created_map_entry_requires_fields_that_the_whole_does_not_supply() {
    let source = format!(
        r#"{DOWNLOAD}
struct Root {{ k: Str = "x", m: Map<Str, Download> }}
fn mk(k: Str) -> Map<Str, Download> = {{}}
m = mk(self.k)
m.tool.url = force("x")
"#
    );
    assert_eval_error(&source, "E0801", "m.tool.version");
}

#[test]
fn supplied_map_entry_keeps_required_fields_not_set_by_the_overlay() {
    let source = format!(
        r#"{DOWNLOAD}
struct Root {{ k: Str = "x", m: Map<Str, Download> }}
fn mk(k: Str) -> Map<Str, Download> = {{ tool = Download {{ version = "1" }} }}
m = mk(self.k)
m.tool.url = force("x")
"#
    );
    assert_value(&source, "/m/tool", json!({"version": "1", "url": "x"}));
}

#[test]
fn created_nested_option_requires_its_missing_fields() {
    let source = format!(
        r#"{DOWNLOAD}
struct Box {{ o: Option<Download> }}
struct Root {{ k: Str = "x", cfg: Box }}
fn mk(k: Str) -> Box = Box {{ o = none }}
cfg = mk(self.k)
cfg.o.url = force("x")
"#
    );
    assert_eval_error(&source, "E0801", "cfg.o.version");
}

#[test]
fn unused_fallback_defaults_are_not_evaluated() {
    for (entries, overlay) in [
        (r#"{ tool = Item { n = 1 } }"#, "m.tool.required = force(2)"),
        ("{}", "m.tool.required = force(2)\nm.tool.n = force(3)"),
    ] {
        let source = format!(
            r#"
struct Item {{ required: Int = 0, n: Int = 1 / 0 }}
struct Root {{ k: Str = "x", m: Map<Str, Item> }}
fn mk(k: Str) -> Map<Str, Item> = {entries}
m = mk(self.k)
{overlay}
"#
        );
        let n = if entries == "{}" { 3 } else { 1 };
        assert_value(&source, "/m/tool", json!({"required": 2, "n": n}));
    }
}

#[test]
fn late_function_result_ignores_the_explicit_field_record() {
    let source = format!(
        r#"{DOWNLOAD}
struct Root {{ v: Str = "1", d: Download }}
fn mk(v: Str) -> Download = Download {{ version = v }}
d = mk(self.v)
d.version = force("9")
"#
    );
    assert_value(
        &source,
        "/d",
        json!({"version": "9", "url": "https://example.org/tool-1.tar.gz"}),
    );
}

#[test]
fn eager_function_result_recomputes_omitted_fields_after_merging() {
    let source = format!(
        r#"{DOWNLOAD}
struct Root {{ v: Str = "1", d: Download }}
fn mk(v: Str) -> Download = Download {{ version = v }}
d = mk("1")
d.version = force("9")
"#
    );
    assert_value(
        &source,
        "/d",
        json!({"version": "9", "url": "https://example.org/tool-9.tar.gz"}),
    );
}

#[test]
fn higher_nested_whole_map_preserves_lower_whole_keys() {
    let source = r#"
struct S { m: Map<Str, Int> = {} }
struct Root { x: Str = "x", y: Str = "y", s: S }
fn a(x: Str) -> S = S { m = { lower = 1, shared = 1 } }
fn b(y: Str) -> Map<Str, Int> = { higher = 2, shared = 2 }
s = default(a(self.x))
s.m = b(self.y)
"#;
    assert_value(
        source,
        "/s/m",
        json!({"lower": 1, "shared": 2, "higher": 2}),
    );
}

#[test]
fn higher_whole_map_merges_with_a_late_declared_map_default() {
    let source = r#"
fn mk0(a: Str) -> Map<Str, Int> = { lower = 1, shared = 1 }
fn mk1(b: Str) -> Map<Str, Int> = { higher = 2, shared = 2 }
struct Root { a: Str = "a", b: Str = "b", m: Map<Str, Int> = mk0(self.a) }
m = mk1(self.b)
"#;
    assert_value(source, "/m", json!({"lower": 1, "shared": 2, "higher": 2}));
}

#[test]
fn map_layers_use_the_highest_supplied_struct_without_default_leakage() {
    let source = format!(
        r#"{DOWNLOAD}
struct S {{ m: Map<Str, Download> }}
struct Root {{ x: Str = "x", y: Str = "y", s: S }}
fn a(x: Str) -> S = S {{ m = {{
  lower = Download {{ version = "1", url = "lower" }}
  shared = Download {{ version = "1", url = "lower-shared" }}
}} }}
fn b(y: Str) -> Map<Str, Download> = {{ shared = Download {{ version = "2" }} }}
s = default(a(self.x))
s.m = b(self.y)
s.m.shared.version = force("9")
s.m.created.version = force("3")
"#
    );
    assert_value(
        &source,
        "/s/m",
        json!({
            "lower": {"version": "1", "url": "lower"},
            "shared": {"version": "9", "url": "https://example.org/tool-2.tar.gz"},
            "created": {"version": "3", "url": "https://example.org/tool-3.tar.gz"}
        }),
    );
}

#[test]
fn higher_whole_struct_skips_the_failing_lower_declared_whole() {
    let source = r#"
struct C { n: Int, m: Map<Str, Int> }
fn mk0(a: Int) -> C = C { n = a, m = { lower = a } }
fn mk1(b: Int) -> C = C { n = b, m = {} }
struct Root { zero: Int = 0, b: Int = 2, c: C = mk0(1 / self.zero) }
c = mk1(self.b)
"#;
    assert_value(source, "/c", json!({"n": 2, "m": {}}));
}

#[test]
fn recursive_field_coverage_skips_the_failing_lower_whole_struct() {
    let source = r#"
struct Inner { a: Str, b: Str }
struct Source { inner: Inner, sha256: Str }
fn hosted(v: Int) -> Source = Source { inner = { a = "a#{v}", b = "b" }, sha256 = "old" }
struct Root { zero: Int = 0, source: Source = hosted(1 / self.zero) }
source.inner.a = force("a")
source.inner.b = force("b")
source.sha256 = force("abc")
"#;
    assert_value(
        source,
        "/source",
        json!({"inner": {"a": "a", "b": "b"}, "sha256": "abc"}),
    );
}

#[test]
fn map_field_coverage_does_not_skip_a_lower_whole_struct() {
    let source = r#"
struct S { m: Map<Str, Int> }
fn lower(n: Int) -> S = S { m = { lower = n } }
fn higher(n: Int) -> Map<Str, Int> = { higher = n }
struct Root { zero: Int = 0, n: Int = 2, s: S }
s = default(lower(1 / self.zero))
s.m = higher(self.n)
"#;
    let diagnostics = evaluate(source).expect_err("the lower whole must be evaluated");
    assert!(
        diagnostics.to_string().contains("division by zero"),
        "{diagnostics}"
    );
}

#[test]
fn tagged_selection_under_a_whole_struct_keeps_only_matching_payloads() {
    for (build, bins) in [
        (
            r#"cargo(features = ["old"], bins = ["kept"])"#,
            json!(["kept"]),
        ),
        (r#"meson(options = ["discarded"])"#, json!([])),
    ] {
        let source = format!(
            r#"
type Build = cargo(features: List<Str> = [], bins: List<Str> = []) | meson(options: List<Str> = [])
struct Job {{ build: Build }}
struct Root {{ k: Str = "x", job: Job }}
fn mkjob(k: Str) -> Job = Job {{ build = {build} }}
job = default(mkjob(self.k))
job {{ build cargo {{ features = ["x"] }} }}
"#
        );
        assert_value(
            &source,
            "/job/build",
            json!({"cargo": {"features": ["x"], "bins": bins}}),
        );
    }
}

#[test]
fn tagged_selection_at_a_whole_sum_keeps_only_matching_payloads() {
    for (build, bins) in [
        (
            r#"cargo(features = ["old"], bins = ["kept"])"#,
            json!(["kept"]),
        ),
        (r#"meson(options = ["discarded"])"#, json!([])),
    ] {
        let source = format!(
            r#"
type Build = cargo(features: List<Str> = [], bins: List<Str> = []) | meson(options: List<Str> = [])
struct Job {{ build: Build }}
struct Root {{ k: Str = "x", job: Job }}
fn mkbuild(k: Str) -> Build = {build}
job.build = default(mkbuild(self.k))
job {{ build cargo {{ features = ["x"] }} }}
"#
        );
        assert_value(
            &source,
            "/job/build",
            json!({"cargo": {"features": ["x"], "bins": bins}}),
        );
    }
}

#[test]
fn higher_sum_attribute_skips_a_failing_late_whole_sum() {
    let source = r#"
type Build = cargo(features: List<Str> = []) | meson(options: List<Str> = [])
struct Job { build: Build }
struct Root { zero: Int = 0, job: Job }
fn mkbuild(k: Int) -> Build = cargo(features = ["value-#{k}"])
job.build = default(mkbuild(1 / self.zero))
job.build = force(meson(options = []))
"#;
    assert_value(source, "/job/build", json!({"meson": {"options": []}}));
}

#[test]
fn tagged_selection_does_not_cover_a_failing_lower_whole_struct() {
    let source = r#"
type Build = cargo(features: List<Str> = []) | meson(options: List<Str> = [])
struct Job { build: Build }
struct Root { zero: Int = 0, job: Job }
fn mkjob(k: Int) -> Job = Job { build = cargo(features = ["value-#{k}"]) }
job = default(mkjob(1 / self.zero))
job { build cargo { features = ["x"] } }
"#;
    let diagnostics = evaluate(source).expect_err("the tagged selection must not cover the field");
    assert!(
        diagnostics.to_string().contains("division by zero"),
        "{diagnostics}"
    );
}

#[test]
fn different_payload_variant_gets_recursive_struct_field_defaults() {
    let source = format!(
        r#"{DOWNLOAD}
struct Details {{ download: Download }}
type Build = cargo(details: Details) | meson
struct Inner {{ build: Build }}
struct Job {{ inner: Inner }}
struct Root {{ k: Str = "x", job: Job }}
fn mkjob(k: Str) -> Job = Job {{ inner = Inner {{ build = meson }} }}
job = default(mkjob(self.k))
job {{ inner {{ build cargo {{ details.download.version = "2" }} }} }}
"#
    );
    assert_value(
        &source,
        "/job/inner/build/cargo/details/download",
        json!({"version": "2", "url": "https://example.org/tool-2.tar.gz"}),
    );
}

#[test]
fn matching_payload_variant_keeps_nested_computed_defaults() {
    let source = format!(
        r#"{DOWNLOAD}
struct Details {{ download: Download }}
type Build = cargo(details: Details) | meson
struct Job {{ build: Build }}
struct Root {{ k: Str = "x", job: Job }}
fn mkjob(k: Str) -> Job = Job {{
  build = cargo(details = Details {{ download = Download {{ version = "1" }} }})
}}
job = default(mkjob(self.k))
job {{ build cargo {{ details.download.version = "2" }} }}
"#
    );
    assert_value(
        &source,
        "/job/build/cargo/details/download",
        json!({"version": "2", "url": "https://example.org/tool-1.tar.gz"}),
    );
}

#[test]
fn created_payload_requires_fields_not_supplied_by_the_discarded_variant() {
    let source = format!(
        r#"{DOWNLOAD}
type Build = cargo(download: Download) | meson
struct Job {{ build: Build }}
struct Root {{ k: Str = "x", job: Job }}
fn mkjob(k: Str) -> Job = Job {{ build = meson }}
job = default(mkjob(self.k))
job {{ build cargo {{ download.url = "x" }} }}
"#
    );
    assert_eval_error(&source, "E0801", "version");
}

#[test]
fn potential_map_default_cycles_precede_whole_value_evaluation() {
    for entries in ["{}", r#"{ tool = Download { version = "1", url = "u" } }"#] {
        let source = format!(
            r#"{DOWNLOAD}
struct Root {{ zero: Int = 0, m: Map<Str, Download> block download }}
fn mk(k: Int) -> Map<Str, Download> = {entries}
m = default(mk(1 / self.zero))
download tool {{ version = force(self.url) }}
"#
        );
        assert_eval_error(&source, "E0607", "m.tool");
    }
}

#[test]
fn potential_nested_map_default_cycles_precede_deferred_failures() {
    for entries in ["{}", r#"{ tool = Download { version = "1", url = "u" } }"#] {
        let source = format!(
            r#"{DOWNLOAD}
struct Inner {{ m: Map<Str, Download> block download }}
struct Box {{ inner: Inner }}
struct Root {{ zero: Int = 0, cfg: Box }}
fn mk(k: Int) -> Box = Box {{ inner = Inner {{ m = {entries} }} }}
cfg = default(mk(1 / self.zero))
cfg {{ inner {{ download tool {{ version = force(self.url) }} }} }}
"#
        );
        assert_eval_error(&source, "E0607", "cfg.inner.m.tool");
    }
}

#[test]
fn potential_nested_option_default_cycles_precede_deferred_failures() {
    for value in ["none", r#"Download { version = "1", url = "u" }"#] {
        let source = format!(
            r#"{DOWNLOAD}
struct Inner {{ o: Option<Download> }}
struct Box {{ inner: Inner }}
struct Root {{ zero: Int = 0, cfg: Box }}
fn mk(k: Int) -> Box = Box {{ inner = Inner {{ o = {value} }} }}
cfg = default(mk(1 / self.zero))
cfg {{ inner {{ o {{ version = force(self.url) }} }} }}
"#
        );
        assert_eval_error(&source, "E0607", "cfg.inner.o");
    }
}

#[test]
fn potential_payload_struct_default_cycles_precede_deferred_failures() {
    for build in [
        "meson",
        r#"cargo(details = Details { download = Download { version = "1", url = "u" } })"#,
    ] {
        let source = format!(
            r#"{DOWNLOAD}
struct Details {{ download: Download }}
type Build = cargo(details: Details) | meson
struct Job {{ build: Build }}
struct Root {{ zero: Int = 0, job: Job }}
fn mkjob(k: Int) -> Job = Job {{ build = {build} }}
job = default(mkjob(1 / self.zero))
job {{ build cargo {{ details {{ download {{ version = self.url }} }} }} }}
"#
        );
        assert_eval_error(&source, "E0607", "download");
    }
}

#[test]
fn supplied_map_entry_still_has_conservative_default_cycle_edges() {
    let source = format!(
        r#"{DOWNLOAD}
struct Root {{ k: Str = "x", m: Map<Str, Download> block download }}
fn mk(k: Str) -> Map<Str, Download> = {{ tool = Download {{ version = "1", url = "u" }} }}
m = default(mk(self.k))
download tool {{ version = force(self.url) }}
"#
    );
    assert_eval_error(&source, "E0607", "m.tool");
}

#[test]
fn supplied_nested_option_still_has_conservative_default_cycle_edges() {
    let source = format!(
        r#"{DOWNLOAD}
struct Box {{ o: Option<Download> }}
struct Root {{ k: Str = "x", cfg: Box }}
fn mk(k: Str) -> Box = Box {{ o = Download {{ version = "1", url = "u" }} }}
cfg = default(mk(self.k))
cfg {{ o {{ version = force(self.url) }} }}
"#
    );
    assert_eval_error(&source, "E0607", "cfg.o");
}

#[test]
fn created_map_entry_defaults_add_edges_through_struct_fields() {
    let source = format!(
        r#"{DOWNLOAD}
struct Details {{ download: Download }}
struct Inner {{ m: Map<Str, Details> block item }}
struct Root {{ zero: Int = 0, cfg: Inner }}
fn mk(k: Int) -> Inner = Inner {{ m = {{}} }}
cfg = default(mk(1 / self.zero))
cfg {{ item tool {{ download {{ version = force(self.url) }} }} }}
"#
    );
    assert_eval_error(&source, "E0607", "cfg.m.tool.download");
}

#[test]
fn created_option_defaults_add_edges_through_struct_fields() {
    let source = format!(
        r#"{DOWNLOAD}
struct Details {{ download: Download }}
struct Inner {{ o: Option<Details> }}
struct Root {{ zero: Int = 0, cfg: Inner }}
fn mk(k: Int) -> Inner = Inner {{ o = none }}
cfg = default(mk(1 / self.zero))
cfg {{ o {{ download {{ version = force(self.url) }} }} }}
"#
    );
    assert_eval_error(&source, "E0607", "cfg.o.download");
}

#[test]
fn matching_payload_still_has_conservative_struct_default_cycle_edges() {
    let source = format!(
        r#"{DOWNLOAD}
struct Details {{ download: Download }}
type Build = cargo(details: Details) | meson
struct Job {{ build: Build }}
struct Root {{ k: Str = "x", job: Job }}
fn mkjob(k: Str) -> Job = Job {{
  build = cargo(details = Details {{ download = Download {{ version = "1", url = "u" }} }})
}}
job = default(mkjob(self.k))
job {{ build cargo {{ details {{ download {{ version = self.url }} }} }} }}
"#
    );
    assert_eval_error(&source, "E0607", "download");
}

#[test]
fn selected_payload_inside_a_supplied_map_entry_gets_recursive_defaults() {
    let source = format!(
        r#"{DOWNLOAD}
struct Details {{ download: Download }}
type Build = cargo(details: Details) | meson
struct Job {{ build: Build }}
struct Root {{ k: Str = "x", jobs: Map<Str, Job> block job }}
fn mk(k: Str) -> Map<Str, Job> = {{ tool = Job {{ build = meson }} }}
jobs = default(mk(self.k))
job tool {{ build cargo {{ details.download.version = "2" }} }}
"#
    );
    assert_value(
        &source,
        "/jobs/tool/build/cargo/details/download",
        json!({"version": "2", "url": "https://example.org/tool-2.tar.gz"}),
    );
}

#[test]
fn struct_only_paths_do_not_add_unused_declared_default_edges() {
    let source = format!(
        r#"{DOWNLOAD}
struct Inner {{ download: Download }}
struct Box {{ inner: Inner }}
struct Root {{ k: Str = "x", cfg: Box }}
fn mk(k: Str) -> Box = Box {{
  inner = Inner {{ download = Download {{ version = "1", url = "u" }} }}
}}
cfg = default(mk(self.k))
cfg {{ inner {{ download {{ version = force(self.url) }} }} }}
"#
    );
    assert_value(
        &source,
        "/cfg/inner/download",
        json!({"version": "u", "url": "u"}),
    );
}

#[test]
fn higher_field_override_removes_a_potential_fallback_cycle() {
    let source = format!(
        r#"{DOWNLOAD}
struct Inner {{ m: Map<Str, Download> block download }}
struct Root {{ k: Str = "x", cfg: Inner }}
fn mk(k: Str) -> Inner = Inner {{ m = {{}} }}
cfg = default(mk(self.k))
cfg {{ download tool {{ version = force(self.url), url = force("mirror") }} }}
"#
    );
    assert_value(
        &source,
        "/cfg/m/tool",
        json!({"version": "mirror", "url": "mirror"}),
    );
}

#[test]
fn late_whole_option_rejects_equal_and_lower_priority_fields() {
    for assignment in ["o.weight = 5", "o.weight = default(5)"] {
        for value in ["none", r#"Upstream { url = "u" }"#] {
            let source = format!(
                r#"
struct Upstream {{ url: Str, weight: Int = 10 }}
struct Root {{ x: Str = "x", o: Option<Upstream> }}
fn opt(x: Str) -> Option<Upstream> = {value}
o = opt(self.x)
{assignment}
"#
            );
            assert_eval_error(&source, "E0504", "o");
        }
    }
}

#[test]
fn payload_defaults_cannot_read_self() {
    let source = r#"
type Build = cargo(n: Int = self.zero) | meson
struct Root { zero: Int = 0, build: Build = meson }
"#;
    let diagnostics = checked(source).expect_err("payload defaults must reject self");
    let codes: Vec<_> = diagnostics.iter().map(|d| d.diagnostic.code).collect();
    assert_eq!(codes, ["E0606"], "{diagnostics}");
}

#[test]
fn higher_sum_attribute_with_a_discarded_block_skips_the_late_whole() {
    assert_value(
        r##"
        type Build = cargo(features: List<Str> = []) | meson(options: List<Str> = [])
        struct Root { zero: Int = 0, build: Build }
        fn bad(n: Int) -> Build = cargo(features = ["#{1 / n}"])
        build = default(bad(self.zero))
        build cargo {}
        build = force(meson(options = []))
        "##,
        "/build",
        serde_json::json!({"meson": {"options": []}}),
    );
}

#[test]
fn unused_selected_payload_defaults_are_not_evaluated() {
    assert_value(
        r##"
        type Build = cargo(n: Int = 1 / 0) | meson
        struct Root { k: Int = 1, build: Build }
        fn mk(k: Int) -> Build = meson
        build = default(mk(self.k))
        build cargo { n = 7 }
        "##,
        "/build",
        serde_json::json!({"cargo": {"n": 7}}),
    );
}

#[test]
fn fallback_struct_default_keeps_dependencies_beside_partial_blocks() {
    assert_eval_error(
        r##"
        struct Inner { a: Int, b: Int }
        struct Item { seed: Int, inner: Inner = Inner { a = self.seed, b = 1 } }
        struct Root { zero: Int = 0, m: Map<Str, Item> block item }
        fn mk(n: Int) -> Map<Str, Item> = {}
        m = default(mk(1 / self.zero))
        item tool {
          seed = force(self.inner.a)
          inner { b = 2 }
        }
        "##,
        "E0607",
        "m.tool.inner",
    );
}

#[test]
fn optional_composite_none_keeps_only_higher_contributions() {
    for (ty, declarations, overlay, expected) in [
        ("Map<Str, Int>", "", "o.a = force(2)", json!({"a": 2})),
        (
            "Build",
            "type Build = cargo(n: Int = 7)",
            "o cargo {}",
            json!({"cargo": {"n": 7}}),
        ),
        ("Data", "", "o = force({ a = 2 })", json!({"a": 2})),
    ] {
        let source = format!(
            "{declarations}\nstruct Root {{ k: Int = 1, o: Option<{ty}> }}\n\
             fn opt(k: Int) -> Option<{ty}> = none\n\
             o = default(opt(self.k))\no = default(none)\n{overlay}\n"
        );
        assert_value(&source, "/o", expected);
    }
}

#[test]
fn optional_none_presence_is_static_for_every_composite_shape() {
    for (ty, declarations) in [
        ("Map<Str, Int>", ""),
        ("Item", "struct Item { a: Int = 1 }"),
        ("Build", "type Build = cargo(n: Int = 7)"),
        ("Data", ""),
    ] {
        let source = format!(
            "{declarations}\nstruct Root {{ k: Int = 1, o: Option<{ty}> }}\n\
             fn opt(k: Int) -> Option<{ty}> = none\n\
             o = opt(self.k)\no = none\n"
        );
        assert_eval_error(&source, "E0501", "o");
        let absent = source.replace("o = none", "o = default(none)");
        assert_value(&absent, "/o", Value::Null);
    }
}

#[test]
fn fallback_default_chains_finalize_in_dependency_order() {
    let mut fields = String::new();
    for i in (1..300).rev() {
        fields.push_str(&format!("f{i}: Int = self.f{},\n", i - 1));
    }
    let source = format!(
        "struct Item {{ {fields} f0: Int }}\n\
         struct Root {{ k: Int = 1, m: Map<Str, Item> }}\n\
         fn mk(k: Int) -> Map<Str, Item> = {{}}\n\
         m = mk(self.k)\nm.tool.f0 = force(7)\n"
    );
    assert_value(&source, "/m/tool/f299", json!(7));
}

#[test]
fn optional_whole_none_discards_lower_present_defaults() {
    for (overlay, expected) in [
        ("", Value::Null),
        (
            "o.url = force(\"higher\")",
            json!({"url": "higher", "weight": 10}),
        ),
    ] {
        let source = format!(
            r#"
            struct Upstream {{ url: Str, weight: Int = 10 }}
            struct Root {{ k: Int = 1, o: Option<Upstream> = Upstream {{ url = "lower", weight = 99 }} }}
            fn opt(k: Int) -> Option<Upstream> = none
            o = opt(self.k)
            {overlay}
            "#
        );
        assert_value(&source, "/o", expected);
    }
}

#[test]
fn suspended_struct_literals_keep_explicit_fields_out_of_default_cycles() {
    let source = r#"
    struct Inner { a: Int = self.b, b: Int = self.a }
    struct Item { inner: Inner = Inner { a = 1, b = 2 }, flag: Int }
    struct Root { k: Int = 1, m: Map<Str, Item> }
    fn mk(k: Int) -> Map<Str, Item> = {}
    m = mk(self.k)
    m.tool.flag = force(7)
    "#;
    assert_value(source, "/m/tool/inner", json!({"a": 1, "b": 2}));
}

#[test]
fn suspended_literal_siblings_keep_nested_maps_structs_and_options() {
    let source = r#"
    struct Inner { a: Int = self.b, b: Int = self.a }
    struct Nest { child: Inner }
    struct Item {
      map: Map<Str, Inner> = { first = Inner { a = 1, b = 2 } }
      nested: Nest = { child = { a = 3, b = 4 } }
      optional: Option<Inner> = Inner { a = 5, b = 6 }
      absent: Option<Inner> = none
      flag: Int
    }
    struct Root { k: Int = 1, m: Map<Str, Item> }
    fn mk(k: Int) -> Map<Str, Item> = {}
    m = mk(self.k)
    m.tool.flag = force(7)
    "#;
    assert_value(
        source,
        "/m/tool",
        json!({
            "map": {"first": {"a": 1, "b": 2}},
            "nested": {"child": {"a": 3, "b": 4}},
            "optional": {"a": 5, "b": 6},
            "absent": null,
            "flag": 7,
        }),
    );
}

#[test]
fn unused_suspended_literal_children_are_never_evaluated() {
    let source = r#"
    struct Inner { a: Int, b: Int }
    struct Item { inner: Inner = { a = 1 / 0, b = self.seed }, seed: Int }
    struct Root { k: Int = 1, m: Map<Str, Item> }
    fn mk(k: Int) -> Map<Str, Item> = {}
    m = mk(self.k)
    m.tool.seed = force(2)
    m.tool.inner.a = force(7)
    "#;
    assert_value(source, "/m/tool/inner", json!({"a": 7, "b": 2}));
}

#[test]
fn suspended_literal_children_keep_the_declaring_self_frame() {
    let source = r#"
    struct Inner { a: Int, b: Int }
    struct Item { inner: Inner = { a = self.seed, b = 2 }, seed: Int }
    struct Root { k: Int = 1, m: Map<Str, Item> }
    fn mk(k: Int) -> Map<Str, Item> = {}
    m = mk(self.k)
    m.tool.seed = force(3)
    m.tool.inner.b = force(7)
    "#;
    assert_value(source, "/m/tool/inner", json!({"a": 3, "b": 7}));
}

#[test]
fn supplied_structs_do_not_register_unreachable_optional_default_cycles() {
    let source = r#"
    struct Inner { a: Int = self.b, b: Int = self.a }
    struct Item { optional: Option<Inner> = Inner {}, flag: Int }
    struct Root { k: Int = 1, item: Item }
    fn mk(k: Int) -> Item = Item { optional = none, flag = 1 }
    item = mk(self.k)
    "#;
    assert_value(source, "/item", json!({"optional": null, "flag": 1}));
}

#[test]
fn unused_interpolated_fallback_keys_are_not_evaluated() {
    let source = r##"
    struct Item { map: Map<Str, Int> = { "#{1 / 0}" = 1 }, flag: Int }
    struct Root { k: Int = 1, item: Item }
    fn mk(k: Int) -> Item = Item { map = {}, flag = 1 }
    item = mk(self.k)
    "##;
    assert_value(source, "/item", json!({"map": {}, "flag": 1}));
}

#[test]
fn suspended_late_struct_defaults_supply_complete_struct_only_paths() {
    for expression in [
        "make(self.seed)",
        "if self.seed == 1 { make(1) } else { make(2) }",
    ] {
        let source = format!(
            "struct Inner {{ a: Int = self.b, b: Int = self.a }}\n\
             struct Item {{ inner: Inner = {expression}, seed: Int }}\n\
             struct Root {{ k: Int = 1, m: Map<Str, Item> }}\n\
             fn make(k: Int) -> Inner = Inner {{ a = 1, b = 2 }}\n\
             fn mk(k: Int) -> Map<Str, Item> = {{}}\n\
             m = mk(self.k)\nm.tool.seed = force(1)\n"
        );
        assert_value(&source, "/m/tool/inner", json!({"a": 1, "b": 2}));
    }
}

#[test]
fn suspended_late_struct_defaults_keep_their_own_dependency_edges() {
    let source = r#"
    struct Inner { a: Int, b: Int }
    struct Item { inner: Inner = make(self.seed), seed: Int }
    struct Root { k: Int = 1, m: Map<Str, Item> block item }
    fn make(k: Int) -> Inner = Inner { a = 1, b = 2 }
    fn mk(k: Int) -> Map<Str, Item> = {}
    m = default(mk(self.k))
    item tool { seed = self.inner.a }
    "#;
    assert_eval_error(source, "E0607", "m.tool.inner");
}

#[test]
fn required_source_fields_accept_partial_and_complete_overrides() {
    let source = r#"
    struct Source { url: Str, sha256: Str }
    struct Item { version: Str, source: Source = hosted(self.version) }
    struct Root { item: Item }
    fn hosted(version: Str) -> Source = Source { url = version, sha256 = "old" }
    item.version = "origin"
    item.source.sha256 = "abc"
    "#;
    assert_value(
        source,
        "/item/source",
        json!({"url": "origin", "sha256": "abc"}),
    );
    let source = source
        .replace("url = version", "url = \"#{1 / 0}\"")
        .replace(
            "item.source.sha256 = \"abc\"",
            "item.source = force({ url = \"mirror\", sha256 = \"abc\" })",
        );
    assert_value(
        &source,
        "/item/source",
        json!({"url": "mirror", "sha256": "abc"}),
    );
}

#[test]
fn full_upstream_defaults_and_produced_fields_survive_late_merging() {
    for (value, weight, tags, backup) in [
        ("none", 10, json!([]), Value::Null),
        (
            r#"Upstream { url = "https://old", weight = 25, tags = ["a", "b", "a"], backup = "https://backup" }"#,
            25,
            json!(["a", "b"]),
            json!("https://backup"),
        ),
    ] {
        let source = format!(
            r##"
            struct Upstream {{
              url: Str where matches(r"https?://.+")
              weight: Int where 1..=100 = 10
              tags: Set<Str> = []
              backup: Option<Str>
            }}
            struct Root {{ x: Str = "x", u: Upstream, o: Option<Upstream> }}
            fn mk(x: Str) -> Upstream = Upstream {{ url = "https://old", weight = 25, tags = ["a", "b", "a"], backup = "https://backup" }}
            fn opt(x: Str) -> Option<Upstream> = {value}
            u = mk(self.x)
            o = opt(self.x)
            o.url = force("https://a")
            "##
        );
        assert_value(
            &source,
            "/u",
            json!({"url": "https://old", "weight": 25, "tags": ["a", "b"], "backup": "https://backup"}),
        );
        assert_value(
            &source,
            "/o",
            json!({"url": "https://a", "weight": weight, "tags": tags, "backup": backup}),
        );
    }
}

#[test]
fn suspended_optional_and_map_literals_keep_conservative_child_edges() {
    for (ty, default, read) in [
        (
            "Option<Inner>",
            "Inner { a = self.seed, b = 2 }",
            "self.value?.a ?? 0",
        ),
        (
            "Map<Str, Inner>",
            "{ first = Inner { a = self.seed, b = 2 } }",
            "self.value[\"first\"].a",
        ),
    ] {
        let source = format!(
            "struct Inner {{ a: Int, b: Int }}\n\
             struct Item {{ value: {ty} = {default}, seed: Int }}\n\
             struct Root {{ zero: Int = 0, m: Map<Str, Item> block item }}\n\
             fn mk(k: Int) -> Map<Str, Item> = {{}}\n\
             m = default(mk(1 / self.zero))\nitem tool {{ seed = {read} }}\n"
        );
        assert_eval_error(&source, "E0607", "m.tool.value");
    }
}

#[test]
fn higher_supplied_struct_masks_already_generated_lower_map_entries() {
    let source = r#"
    struct Entry { env: Map<Str, Int> }
    struct S { m: Map<Str, Entry> }
    struct Root { x: Int = 0, s: S }
    fn lower(x: Int) -> S = S { m = { shared = Entry { env = { old = 1 } }, lower = Entry { env = { kept = 2 } } } }
    fn higher(x: Int) -> Map<Str, Entry> = { shared = Entry { env = {} }, higher = Entry { env = { new = 3 } } }
    s = default(lower(self.x))
    s.m = higher(self.x)
    s.m.shared.env.forced = force(7)
    "#;
    assert_value(
        source,
        "/s/m",
        json!({
            "shared": {"env": {"forced": 7}},
            "lower": {"env": {"kept": 2}},
            "higher": {"env": {"new": 3}},
        }),
    );
}

#[test]
fn same_path_map_layers_mask_lower_nested_struct_maps() {
    let source = r#"
    struct Entry { env: Map<Str, Int> }
    struct Root { x: Int = 0, m: Map<Str, Entry> = lower(self.x) }
    fn lower(x: Int) -> Map<Str, Entry> = { shared = Entry { env = { old = 1 } }, lower = Entry { env = { kept = 2 } } }
    fn higher(x: Int) -> Map<Str, Entry> = { shared = Entry { env = {} }, higher = Entry { env = { new = 3 } } }
    m = higher(self.x)
    "#;
    assert_value(
        source,
        "/m",
        json!({
            "shared": {"env": {}},
            "lower": {"env": {"kept": 2}},
            "higher": {"env": {"new": 3}},
        }),
    );
}

#[test]
fn late_maps_keep_explicit_none_entries_of_every_optional_shape() {
    for (ty, declarations) in [
        ("Int", ""),
        ("Map<Str, Int>", ""),
        ("Item", "struct Item { n: Int = 1 }"),
        ("Choice", "type Choice = one(n: Int = 1)"),
    ] {
        let source = format!(
            "{declarations}\nstruct Root {{ k: Int = 1, m: Map<Str, Option<{ty}>> }}\n\
             fn mk(k: Int) -> Map<Str, Option<{ty}>> = {{ a = none }}\n\
             m = mk(self.k)\n"
        );
        assert_value(&source, "/m", json!({"a": null}));
    }
}

#[test]
fn absent_unused_fallback_map_keys_do_not_leak_into_supplied_maps() {
    let source = r#"
    struct Item { entries: Map<Str, Option<Int>> = { unused = none }, flag: Int }
    struct Root { k: Int = 1, m: Map<Str, Item> }
    fn mk(k: Int) -> Map<Str, Item> = { tool = Item { entries = {}, flag = 1 } }
    m = mk(self.k)
    m.tool.flag = force(7)
    "#;
    assert_value(source, "/m/tool/entries", json!({}));
}

#[test]
fn higher_structs_do_not_keep_spent_optional_map_keys() {
    let source = r#"
    struct Entry { env: Map<Str, Option<Int>> }
    struct S { m: Map<Str, Entry> }
    struct Root { x: Int = 0, s: S }
    fn lower(x: Int) -> S = S { m = { shared = Entry { env = { old = none } } } }
    fn higher(x: Int) -> Map<Str, Entry> = { shared = Entry { env = {} } }
    s = default(lower(self.x))
    s.m = higher(self.x)
    "#;
    assert_value(source, "/s/m/shared/env", json!({}));
}

#[test]
fn unused_struct_valued_fallback_map_keys_do_not_create_instances() {
    let source = r#"
    struct Leaf { n: Int = 1 }
    struct Item { entries: Map<Str, Leaf> = { unused = Leaf {} }, flag: Int }
    struct Root { k: Int = 1, m: Map<Str, Item> }
    fn mk(k: Int) -> Map<Str, Item> = { tool = Item { entries = {}, flag = 1 } }
    m = mk(self.k)
    m.tool.flag = force(7)
    "#;
    assert_value(source, "/m/tool/entries", json!({}));
}

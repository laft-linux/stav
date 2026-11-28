//! Checks roots with opaque fields, evaluates selected attributes of them,
//! and compares the selected attributes with a second evaluation of the
//! same sources in which the opaque fields have real types.

use serde_json::{Value as Json, json};
use stav::{
    AttrPath, Checked, Diagnostics, Evaluated, FieldName, FileDiagnostic, Host, HostModule,
    HostValues, ImportRoot, Limits, MemoryLoader, RootSpec, Source, Value, check,
};

/// The schema of the root, in the prelude of both phases.
const SCHEMA: &str = r#"
struct Repo { url: Str }
struct View { package: Pkgs = Pkgs {} }
struct System {
  mirror: Str = "https://example.org"
  zero: Int = 0
  repos: Map<Str, Repo> = {} block repo
  package: Pkgs = Pkgs {}
  views: Map<Str, View> = {} block view
}
struct Root { system: System = System {} }
"#;

/// The declarations that only the second phase has.
const PACKAGES: &str = r#"
type Vulkan = all | amd | intel
struct Mesa { x11: Bool = true, vulkan: Vulkan = all, llvm: Bool = true }
struct Pkgs { mesa: Mesa = Mesa {} }
"#;

const CORE: &str = r#"repo core { url = "https://example.org/core" }"#;

/// Returns an entry file that sets packages in every form, with `core` as
/// the last statement of the `system` body and `extra` before it.
fn entry(extra: &str, core: &str) -> String {
    format!(
        "fn no_x11() -> Part<System> = part {{ package {{ mesa {{ x11 = false }} }} }}\n\n\
         system {{\n  package.mesa.x11 = false\n  package {{ mesa {{ vulkan = amd }} }}\n  \
         package.mesa.vulkan = default(amd)\n  view dev {{ package.mesa.llvm = false }}\n  \
         include no_x11()\n{extra}\n  {core}\n}}\n"
    )
}

/// A root to check: the files of the import root `app`, the first one the
/// entry file, and the prelude.
struct Root<'a> {
    files: Vec<(&'a str, String)>,
    prelude: Vec<(&'a str, String)>,
    host_modules: Vec<HostModule>,
    root: &'a str,
}

impl<'a> Root<'a> {
    fn new(main: impl Into<String>) -> Self {
        Self {
            files: vec![("main.stav", main.into())],
            prelude: vec![("schema.stav", SCHEMA.to_owned())],
            host_modules: Vec::new(),
            root: "Root",
        }
    }

    fn file(mut self, name: &'a str, text: impl Into<String>) -> Self {
        self.files.push((name, text.into()));
        self
    }

    fn schema(mut self, text: impl Into<String>) -> Self {
        self.prelude = vec![("schema.stav", text.into())];
        self
    }

    fn check(
        &self,
        opaque: &[(&str, &str)],
        prelude: &[(&'a str, &str)],
    ) -> Result<Checked, Diagnostics> {
        let mut loader = MemoryLoader::new();
        for (name, text) in &self.files {
            loader = loader.with_file(format!("app/{name}"), text.as_str());
        }
        let mut host = Host::new(loader);
        host.prelude = self
            .prelude
            .iter()
            .map(|(name, text)| Source::new(*name, text.as_str()))
            .chain(prelude.iter().map(|(name, text)| Source::new(*name, *text)))
            .collect();
        host.host_modules = self.host_modules.clone();
        let mut root = RootSpec::new(
            self.root,
            vec!["app/main.stav".into()],
            vec![ImportRoot::new("app")],
        );
        root.opaque = opaque
            .iter()
            .map(|(name, field)| FieldName::new(*name, *field))
            .collect();
        check(&host, &root)
    }

    /// Checks the first phase, with `System.package` and `View.package`
    /// opaque.
    fn first(&self) -> Checked {
        self.check(&[("System", "package"), ("View", "package")], &[])
            .unwrap_or_else(|d| panic!("the first phase must check:\n{d}"))
    }

    /// Checks the second phase, which declares the package types.
    fn second(&self) -> Checked {
        self.check(&[], &[("packages.stav", PACKAGES)])
            .unwrap_or_else(|d| panic!("the second phase must check:\n{d}"))
    }
}

fn path(text: &str) -> AttrPath {
    text.parse().expect("a valid attribute path")
}

fn select(checked: &Checked, paths: &[&str]) -> Result<Evaluated, Diagnostics> {
    let paths: Vec<AttrPath> = paths.iter().map(|p| path(p)).collect();
    checked.eval_selected(&Limits::default(), &HostValues::new(), &paths)
}

fn json(evaluated: &Evaluated, at: &str) -> Json {
    let value = evaluated
        .value_at(&path(at))
        .unwrap_or_else(|d| panic!("`{at}` must be in the value:\n{d}"));
    serde_json::from_str(&value.to_canonical_json()).expect("canonical JSON is valid")
}

/// Returns the one error of a failed evaluation.
fn error(result: Result<Evaluated, Diagnostics>) -> (Diagnostics, FileDiagnostic) {
    let diagnostics = result.expect_err("the evaluation must fail");
    let items: Vec<&FileDiagnostic> = diagnostics.iter().collect();
    assert_eq!(items.len(), 1, "one error is reported:\n{diagnostics}");
    let first = items[0].clone();
    (diagnostics, first)
}

/// Returns the source text that a span of a diagnostic covers.
fn text(checked: &Checked, file: Option<stav::FileId>, span: stav::Span) -> &str {
    let file = file.expect("the span names a file");
    &checked.source(file).expect("the file exists")[span.range()]
}

/// Asserts that an evaluation stops with `E0619` at `read`, naming the
/// selected attribute and the opaque field.
fn assert_opaque(
    name: &str,
    checked: &Checked,
    result: Result<Evaluated, Diagnostics>,
    (selected, field): (&str, &str),
    read: &str,
) {
    let (diagnostics, first) = error(result);
    let d = &first.diagnostic;
    let rendered = diagnostics.to_string();
    insta::assert_snapshot!(name, rendered);
    assert_eq!(d.code, "E0619", "{diagnostics}");
    assert!(rendered.contains(&format!("`{selected}`")), "{diagnostics}");
    assert!(d.message.contains(&format!("`{field}`")), "{diagnostics}");
    assert_eq!(text(checked, first.file, d.span), read, "{diagnostics}");
}

#[test]
fn unchecked_contributions_check_and_leave_the_selection_alone() {
    let root = Root::new(entry("", CORE));
    let checked = root.first();
    let found = select(&checked, &["system.repos"]).expect("the repositories evaluate");
    assert_eq!(
        json(&found, "system.repos"),
        json!({"core": {"url": "https://example.org/core"}})
    );
}

#[test]
fn a_missing_part_file_under_an_opaque_field_is_still_loaded() {
    let root = Root::new(entry("  package { include \"./missing.stav\" }", CORE));
    let errors = root
        .check(&[("System", "package"), ("View", "package")], &[])
        .expect_err("the missing part file is reported");
    let codes: Vec<&str> = errors.iter().map(|d| d.diagnostic.code).collect();
    assert_eq!(codes, ["E0701"], "{errors}");
}

#[test]
fn an_opaque_field_that_names_nothing_fails_the_check() {
    let root = Root::new(entry("", CORE));
    for (name, field) in [("System", "pakage"), ("Sytem", "package")] {
        let errors = root
            .check(
                &[("System", "package"), ("View", "package"), (name, field)],
                &[],
            )
            .expect_err("the opaque field names nothing");
        let codes: Vec<&str> = errors.iter().map(|d| d.diagnostic.code).collect();
        assert_eq!(codes, ["E0302"], "{errors}");
        assert!(errors.to_string().contains(name), "{errors}");
    }
}

#[test]
fn reads_that_the_selection_does_not_need_pass() {
    let module = "let base = System {}\n";
    for (extra, files) in [
        (r##"  mirror = force("#{self.package.mesa.x11}")"##, None),
        (
            "  let x11 = self.package.mesa.x11\n  mirror = force(\"#{x11}\")",
            None,
        ),
        ("", Some(module)),
    ] {
        let main = match files {
            Some(_) => format!("import \"./base.stav\" as base\n{}", entry(extra, CORE)),
            None => entry(extra, CORE),
        };
        let mut root = Root::new(main);
        if let Some(module) = files {
            root = root.file("base.stav", module);
        }
        let checked = root.first();
        let found = select(&checked, &["system.repos"])
            .unwrap_or_else(|d| panic!("the repositories evaluate:\n{d}"));
        assert_eq!(
            json(&found, "system.repos.core.url"),
            json!("https://example.org/core")
        );
    }
}

#[test]
fn a_read_that_the_selection_needs_stops_at_the_read() {
    let root = Root::new(entry(
        "",
        r##"repo core { url = "#{self.package.mesa.x11}" }"##,
    ));
    let checked = root.first();
    let result = select(&checked, &["system.repos"]);
    let (diagnostics, first) = error(result);
    insta::assert_snapshot!("opaque_read_needed", diagnostics.to_string());
    assert_eq!(
        text(&checked, first.file, first.diagnostic.span),
        "self.package"
    );
    assert!(
        first
            .diagnostic
            .notes
            .iter()
            .any(|n| n.contains("`system.repos.core.url`")),
        "{diagnostics}"
    );
}

#[test]
fn a_read_through_a_let_points_at_the_use_and_the_let() {
    let root = Root::new(entry(
        "  let p = self.package",
        r##"repo core { url = "#{p.mesa.x11}" }"##,
    ));
    let checked = root.first();
    let (diagnostics, first) = error(select(&checked, &["system.repos"]));
    insta::assert_snapshot!("opaque_read_through_let", diagnostics.to_string());
    assert_eq!(first.diagnostic.code, "E0619", "{diagnostics}");
    assert_eq!(text(&checked, first.file, first.diagnostic.span), "p");
    assert!(
        first
            .diagnostic
            .labels
            .iter()
            .any(|l| text(&checked, l.file.or(first.file), l.span) == "self.package"),
        "{diagnostics}"
    );
}

#[test]
fn a_default_that_reads_an_opaque_field_stops_the_attribute_that_reads_it() {
    let schema = SCHEMA.replace(
        r#"mirror: Str = "https://example.org""#,
        r##"mirror: Str = "#{self.package.mesa.x11}""##,
    );
    let root = Root::new(entry("", "repo core { url = self.mirror }")).schema(schema);
    let checked = root.first();
    assert_opaque(
        "opaque_default_read_through_self",
        &checked,
        select(&checked, &["system.repos"]),
        ("system.repos", "System.package"),
        "self.mirror",
    );
}

#[test]
fn a_whole_value_of_a_struct_with_an_opaque_field_is_a_need() {
    let main = format!(
        "fn pick(v: Map<Str, View>) -> Str = \"#{{len(v)}}\"\n{}",
        entry("", "repo core { url = pick(self.views) }")
    );
    let checked = Root::new(main).first();
    assert_opaque(
        "opaque_whole_view",
        &checked,
        select(&checked, &["system.repos"]),
        ("system.repos", "View.package"),
        "pick(self.views)",
    );
}

#[test]
fn an_eager_call_of_an_unchecked_function_stops_every_evaluation() {
    let main = format!(
        "fn pick() -> Str = \"#{{(System {{}}).package.mesa.x11}}\"\n{}",
        entry("", "repo core { url = pick() }")
    );
    let checked = Root::new(main).first();
    assert_opaque(
        "opaque_eager_call",
        &checked,
        select(&checked, &["system.repos"]),
        ("system.repos", "System.package"),
        "System {}",
    );
}

#[test]
fn self_cycles_outside_the_selection_are_not_reported() {
    let schema = SCHEMA.replace(
        "zero: Int = 0",
        "zero: Int = 0\n  left: Str = \"#{self.right}\"\n  right: Str = \"#{self.left}\"",
    );
    let root = Root::new(entry("", CORE)).schema(schema);
    let found = select(&root.first(), &["system.repos"]).expect("the cycle is not needed");
    assert_eq!(
        json(&found, "system.repos.core.url"),
        json!("https://example.org/core")
    );
    let errors = root
        .second()
        .eval(&Limits::default())
        .expect_err("the cycle is reported");
    let codes: Vec<&str> = errors.iter().map(|d| d.diagnostic.code).collect();
    assert!(codes.contains(&"E0607"), "{errors}");
}

#[test]
fn building_a_value_that_holds_an_opaque_field_is_a_read() {
    let mk = "fn mk() -> System = System { repos = { core = { url = \"https://example.org/core\" } } }\n";
    for (i, main) in [
        format!("{mk}system = mk()\n{}", entry("", CORE)),
        format!("{mk}{}system = mk()\n", entry("", CORE)),
    ]
    .into_iter()
    .enumerate()
    {
        let checked = Root::new(main).first();
        let (diagnostics, first) = error(select(&checked, &["system.repos"]));
        insta::assert_snapshot!(format!("opaque_construction_{i}"), diagnostics.to_string());
        assert_eq!(first.diagnostic.code, "E0619", "{diagnostics}");
        assert!(
            text(&checked, first.file, first.diagnostic.span).starts_with("System {"),
            "{diagnostics}"
        );
        assert!(
            first
                .diagnostic
                .labels
                .iter()
                .any(|l| text(&checked, l.file.or(first.file), l.span) == "mk()"),
            "{diagnostics}"
        );
    }
}

#[test]
fn a_split_literal_contributes_its_entries_one_by_one() {
    let root = Root::new(
        "system = { package = { mesa = { x11 = false } }, repos = { core = { url = \"https://example.org/core\" } } }\n",
    );
    let found = select(&root.first(), &["system.repos"]).expect("the repositories evaluate");
    assert_eq!(
        json(&found, "system.repos"),
        json!({"core": {"url": "https://example.org/core"}})
    );
    let root = Root::new(
        "system = default({ mirror = \"#{self.system.package.mesa.x11}\", repos = { core = { url = \"https://example.org/core\" } } })\n",
    );
    let found = select(&root.first(), &["system.repos"]).expect("only `mirror` is unchecked");
    assert_eq!(
        json(&found, "system.repos"),
        json!({"core": {"url": "https://example.org/core"}})
    );
}

#[test]
fn a_struct_that_holds_an_opaque_field_cannot_be_built() {
    let schema = format!(
        "{SCHEMA}struct Box {{ s: System = System {{}} }}\nstruct Boxed {{ b: Box = Box {{}}, n: Int = 0 }}\n"
    );
    let mut root =
        Root::new("fn count() -> Int = len((Box {}).s.repos)\nn = count()\n").schema(schema);
    root.root = "Boxed";
    let checked = root.first();
    let (diagnostics, first) = error(select(&checked, &["b.s.repos"]));
    assert_eq!(first.diagnostic.code, "E0619", "{diagnostics}");
    assert_eq!(text(&checked, first.file, first.diagnostic.span), "Box {}");

    let mut root = Root::new("").schema(format!(
        "{SCHEMA}struct Box {{ s: System = System {{}} }}\nstruct Boxed {{ b: Box = Box {{}} }}\n"
    ));
    root.root = "Boxed";
    let checked = root.first();
    let found = select(&checked, &["b.s.repos"]).expect("the split default checks");
    assert_eq!(json(&found, "b.s.repos"), json!({}));
}

#[test]
fn a_selection_holds_only_the_selected_attributes() {
    let checked = Root::new(entry("", CORE)).first();
    let found = select(&checked, &["system.repos"]).expect("the repositories evaluate");
    let value: Json = serde_json::from_str(&found.to_canonical_json()).expect("valid JSON");
    assert_eq!(
        value,
        json!({"system": {"repos": {"core": {"url": "https://example.org/core"}}}})
    );
    let missing = found
        .value_at(&path("system.mirror"))
        .expect_err("not selected");
    assert_eq!(
        missing.iter().next().map(|d| d.diagnostic.code),
        Some("E0805")
    );
    assert!(found.spans(&path("system.mirror")).is_empty());
    assert_eq!(found.priority(&path("system.mirror")), None);
    assert!(!found.spans(&path("system.repos.core.url")).is_empty());
    let mut rendered = String::new();
    for bad in ["system.repos.core", "system.nothing", "system.package.mesa"] {
        let (diagnostics, first) = error(select(&checked, &[bad]));
        assert_eq!(first.diagnostic.code, "E0805", "{diagnostics}");
        rendered.push_str(&diagnostics.to_string());
    }
    insta::assert_snapshot!("opaque_selections_name_fields", rendered);
}

#[test]
fn an_attribute_that_nothing_selected_needs_is_never_evaluated() {
    let root = Root::new(entry(r##"  mirror = force("#{1 / self.zero}")"##, CORE));
    let checked = root.first();
    select(&checked, &["system.repos"]).expect("`mirror` is not needed");
    let (diagnostics, first) = error(select(&checked, &["system.mirror"]));
    assert_eq!(first.diagnostic.code, "E0603", "{diagnostics}");
}

#[test]
fn references_are_checked_only_when_the_root_is_selected() {
    let source = "struct Unit { after: List<Ref<Unit>> = [] }\n\
                  struct Root { units: Map<Str, Unit> = {} block unit }\n\
                  unit db {}\nunit api { after = [dbb] }\n";
    let host = Host::new(MemoryLoader::new().with_file("app/main.stav", source));
    let root = RootSpec::new(
        "Root",
        vec!["app/main.stav".into()],
        vec![ImportRoot::new("app")],
    );
    let checked = check(&host, &root).expect("the program checks");
    let found = select(&checked, &["units"]).expect("references are not checked");
    assert_eq!(json(&found, "units.api.after"), json!(["dbb"]));
    let whole = checked.eval_selected(&Limits::default(), &HostValues::new(), &[AttrPath::root()]);
    let (diagnostics, first) = error(whole);
    assert_eq!(first.diagnostic.code, "E0802", "{diagnostics}");
}

#[test]
fn a_whole_evaluation_stops_at_the_first_opaque_field() {
    let checked = Root::new(entry("", CORE)).first();
    let (diagnostics, first) = error(checked.eval(&Limits::default()));
    insta::assert_snapshot!("opaque_whole_evaluation", diagnostics.to_string());
    assert_eq!(first.diagnostic.code, "E0619", "{diagnostics}");
    assert_eq!(
        text(&checked, first.file, first.diagnostic.span),
        "package.mesa.x11 = false"
    );
}

/// The entry files of the tests above that both phases accept.
fn agreeing() -> Vec<Root<'static>> {
    let mut roots = vec![
        Root::new(entry("", CORE)),
        Root::new(entry(
            r##"  mirror = force("#{self.package.mesa.x11}")"##,
            CORE,
        )),
        Root::new(entry(
            "  let x11 = self.package.mesa.x11\n  mirror = force(\"#{x11}\")",
            CORE,
        )),
        Root::new(
            "system = { package = { mesa = { x11 = false } }, repos = { core = { url = \"https://example.org/core\" } } }\n",
        ),
    ];
    roots.push(
        Root::new(format!(
            "import \"./base.stav\" as base\n{}",
            entry("", CORE)
        ))
        .file("base.stav", "let base = System {}\n"),
    );
    roots
}

#[test]
fn both_phases_agree_on_the_selected_attributes() {
    for root in agreeing() {
        let first = root.first();
        let second = root.second();
        let selected = select(&first, &["system.repos"])
            .unwrap_or_else(|d| panic!("the first phase evaluates:\n{d}"));
        let whole = second
            .eval(&Limits::default())
            .unwrap_or_else(|d| panic!("the second phase evaluates:\n{d}"));
        assert_eq!(
            json(&selected, "system.repos"),
            json(&whole, "system.repos")
        );
        let inputs = |c: &Checked| -> Vec<String> {
            c.inputs().map(|(p, _)| p.display().to_string()).collect()
        };
        let mut second_inputs = inputs(&second);
        second_inputs.retain(|p| p != "packages.stav");
        assert_eq!(inputs(&first), second_inputs);
    }
}

/// Returns the fewest steps with which the selection evaluates.
fn fewest_steps(checked: &Checked, paths: &[AttrPath]) -> u64 {
    let fits = |steps: u64| {
        let limits = Limits {
            steps,
            ..Limits::default()
        };
        checked
            .eval_selected(&limits, &HostValues::new(), paths)
            .is_ok()
    };
    let (mut low, mut high) = (0, Limits::default().steps);
    while low < high {
        let middle = low + (high - low) / 2;
        if fits(middle) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    assert!(fits(low) && !fits(low - 1));
    low
}

#[test]
fn a_repeated_selection_gives_the_same_value_and_steps() {
    let checked = Root::new(entry("", CORE)).first();
    let paths = [path("system.repos")];
    let a = select(&checked, &["system.repos"]).expect("the selection evaluates");
    let b = select(&checked, &["system.repos"]).expect("the selection evaluates");
    assert_eq!(a.to_canonical_json(), b.to_canonical_json());
    assert_eq!(
        fewest_steps(&checked, &paths),
        fewest_steps(&checked, &paths)
    );
    let whole = Root::new(entry("", CORE)).second();
    assert!(fewest_steps(&checked, &paths) < fewest_steps(&whole, &[AttrPath::root()]));
}

#[test]
fn a_construction_needed_through_containment_points_at_itself() {
    let schema = "struct R { url: Str, mirror: Str = \"#{self.url}/m\" }\n\
                  struct S { name: Str = \"n\", items: Map<Str, R> = {}, package: P = P {} }\n\
                  struct Root { s: S }\n";
    let main = "fn mk(x: S) -> R = R { url = \"z\" }\n\
                s {\n  items.a = default(mk(S { package = { flag = self.name == \"\" } }))\n  \
                items.a.url = \"y\"\n}\n";
    let root = Root::new(main).schema(schema);
    let checked = root
        .check(&[("S", "package")], &[])
        .unwrap_or_else(|d| panic!("the first phase must check:\n{d}"));
    let (diagnostics, first) = error(select(&checked, &["s.items"]));
    insta::assert_snapshot!("opaque_construction_contained", diagnostics.to_string());
    assert_eq!(first.diagnostic.code, "E0619", "{diagnostics}");
    assert!(
        text(&checked, first.file, first.diagnostic.span).starts_with("S { package"),
        "{diagnostics}"
    );
    let second = root
        .check(&[], &[("p.stav", "struct P { flag: Bool = false }\n")])
        .unwrap_or_else(|d| panic!("the second phase must check:\n{d}"));
    let whole = second
        .eval(&Limits::default())
        .expect("the second phase evaluates");
    assert_eq!(
        json(&whole, "s.items.a"),
        json!({"url": "y", "mirror": "z/m"})
    );
}

const OPTIONAL: &str = "struct T { repos: Map<Str, Str> = {}, package: P }\n\
                        struct Root { k: Int = 1, o: Option<T> }\n";

#[test]
fn a_presence_that_depends_on_an_unchecked_contribution_is_left_open() {
    let root = Root::new("o = none\no { package { x = force(false) } }\n").schema(OPTIONAL);
    let checked = root
        .check(&[("T", "package")], &[])
        .unwrap_or_else(|d| panic!("the first phase must check:\n{d}"));
    let found = select(&checked, &["k"]).expect("no conflict is reported for `o`");
    assert_eq!(json(&found, "k"), json!(1));
    let (diagnostics, first) = error(select(&checked, &["o.repos"]));
    insta::assert_snapshot!("opaque_presence", diagnostics.to_string());
    assert_eq!(first.diagnostic.code, "E0619", "{diagnostics}");
    assert_eq!(text(&checked, first.file, first.diagnostic.span), "none");
    assert!(
        first
            .diagnostic
            .labels
            .iter()
            .any(|l| text(&checked, l.file.or(first.file), l.span).starts_with("package {")),
        "{diagnostics}"
    );
    let second = root
        .check(&[], &[("p.stav", "struct P { x: Bool = true }\n")])
        .unwrap_or_else(|d| panic!("the second phase must check:\n{d}"));
    let whole = second
        .eval(&Limits::default())
        .expect("the second phase evaluates");
    assert_eq!(json(&whole, "o.package"), json!({"x": false}));
}

#[test]
fn defaults_below_an_open_presence_are_not_evaluated() {
    let schema = format!(
        "fn mk() -> Map<Str, Int> = {{ a = 1 / 0 }}\n{}",
        OPTIONAL.replace("package: P }", "package: P, m: Map<Str, Int> = mk() }")
    );
    for (block, absent) in [("x = false", true), ("x = force(false)", false)] {
        let main = format!("o = force(none)\no {{ package {{ {block} }} }}\n");
        let root = Root::new(main).schema(schema.clone());
        let checked = root
            .check(&[("T", "package")], &[])
            .unwrap_or_else(|d| panic!("the first phase must check:\n{d}"));
        let found = select(&checked, &["k"]).expect("`mk()` is not evaluated");
        assert_eq!(json(&found, "k"), json!(1));
        let second = root
            .check(&[], &[("p.stav", "struct P { x: Bool = true }\n")])
            .unwrap_or_else(|d| panic!("the second phase must check:\n{d}"));
        let whole = second.eval(&Limits::default());
        if absent {
            let whole = whole.expect("the second phase finds `o` absent");
            assert_eq!(json(&whole, "o"), json!(null));
        } else {
            let (diagnostics, first) = error(whole);
            assert_eq!(first.diagnostic.code, "E0501", "{diagnostics}");
        }
    }
}

#[test]
fn a_host_value_for_an_unchecked_let_is_not_converted() {
    let mut root = Root::new("import \"app/defaults\" as defaults\n".to_owned() + &entry("", CORE));
    root.host_modules.push(HostModule {
        path: "app/defaults".to_owned(),
        source: "let base: Option<View> = none\n".to_owned(),
    });
    let checked = root.first();
    let view = Value::Struct {
        name: "View".to_owned(),
        fields: vec![("package".to_owned(), Value::Int(3))],
    };
    let values = HostValues::new().with("app/defaults", "base", view);
    let found = checked
        .eval_selected(&Limits::default(), &values, &[path("system.repos")])
        .unwrap_or_else(|d| panic!("the value is not converted:\n{d}"));
    assert_eq!(
        json(&found, "system.repos.core.url"),
        json!("https://example.org/core")
    );
}

#[test]
fn unchecked_definitions_report_nothing_and_add_no_calls() {
    let main = format!(
        "fn a() -> Int = if (System {{}}).zero == 0 {{ b() }} else {{ 0 }}\n\
         fn b() -> Int = a()\n{}",
        entry(
            "  mirror = force(\"#{self.package.mesa.vulkan == Vulkan.amd}#{nothing}\")",
            CORE
        )
    );
    let checked = Root::new(main).first();
    assert!(checked.warnings().is_empty(), "{}", checked.warnings());
    select(&checked, &["system.repos"]).expect("nothing unchecked is needed");
}

#[test]
fn a_declaration_of_the_second_phase_outside_unchecked_code_is_unknown() {
    let main = format!("fn amd() -> Vulkan = Vulkan.amd\n{}", entry("", CORE));
    let errors = Root::new(main)
        .check(&[("System", "package"), ("View", "package")], &[])
        .expect_err("`Vulkan` is declared only in the second phase");
    let codes: Vec<&str> = errors.iter().map(|d| d.diagnostic.code).collect();
    assert!(codes.iter().all(|&c| c == "E0302"), "{errors}");
}

#[test]
fn a_statement_whose_head_reads_an_opaque_field_stops_when_it_runs() {
    for statement in [
        "  if (System {}).zero == 0 { mirror = \"x\" }",
        "  for r in (System {}).repos { mirror = \"x\" }",
    ] {
        let checked = Root::new(entry(statement, CORE)).first();
        let (diagnostics, first) = error(select(&checked, &["system.repos"]));
        assert_eq!(first.diagnostic.code, "E0619", "{diagnostics}");
        assert_eq!(
            text(&checked, first.file, first.diagnostic.span),
            "System {}"
        );
        assert!(
            first.diagnostic.message.contains("a statement"),
            "{diagnostics}"
        );
    }
}

#[test]
fn a_part_statement_that_reads_an_opaque_field_leaves_the_part_checked() {
    let main = format!(
        "fn tagged() -> Part<System> = part {{\n  mirror = \"#{{self.package.mesa.x11}}\"\n  \
         repo extra {{ url = \"https://example.org/extra\" }}\n}}\n{}",
        entry("  include tagged()", CORE)
    );
    let checked = Root::new(main).first();
    let found = select(&checked, &["system.repos"]).expect("the part is checked");
    assert_eq!(
        json(&found, "system.repos"),
        json!({
            "core": {"url": "https://example.org/core"},
            "extra": {"url": "https://example.org/extra"}
        })
    );
    let (diagnostics, first) = error(select(&checked, &["system.mirror"]));
    assert_eq!(first.diagnostic.code, "E0619", "{diagnostics}");
    assert_eq!(
        text(&checked, first.file, first.diagnostic.span),
        "self.package"
    );
}

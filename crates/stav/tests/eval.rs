//! Evaluates the fixture programs and compares the rendered values, or the
//! diagnostics, with their snapshots.
//!
//! A fixture whose name starts with `err_` must check but fail to
//! evaluate, and every other fixture must evaluate. A fixture is one
//! `.stav` file. Without sections it is the entry file `main.stav` of the
//! root struct `Root`. Lines starting with `#-` split it into several
//! sources:
//!
//! - `#- file <path>`: a file of the import root, the first one being the
//!   entry file unless `#! entries` says otherwise,
//! - `#- std <name>`: the module `std/<name>`,
//! - `#- prelude <name>`: a prelude file,
//! - `#- part <name>`: an implicit part,
//! - `#- host <namespace>/<module>`: a host module.
//!
//! Lines starting with `#!` before the first section set options:
//! `#! root <Name>`, `#! entries <path> <path>`,
//! `#! symbol <Name> <constructor> [words] [strings]` for a symbol type
//! whose parser lowercases the text and refuses text with spaces,
//! `#! implicit <Struct>` for implicit reference targets named by the keys
//! of the root field `users`, `#! render <format>...` for the formats of
//! the snapshot, JSON by default, and `#! steps <n>` for the step limit.

use std::fs;
use std::path::{Path, PathBuf};

use stav::{
    Format, Host, HostModule, ImplicitRefs, ImportRoot, Limits, MemoryLoader, RootSpec, Source,
    Span, SymbolType, Value, check,
};

const ROOT_DIR: &str = "app";

struct Fixture {
    host: Host,
    root: RootSpec,
    formats: Vec<Format>,
    limits: Limits,
}

fn lowercase(text: &str) -> Result<String, String> {
    if text.contains(' ') {
        return Err("a symbol has no spaces".to_owned());
    }
    Ok(text.to_lowercase())
}

/// Returns the keys of the root field `users`.
fn user_groups(root: &Value) -> Vec<String> {
    match root.get("users") {
        Some(Value::Map(users)) => users.iter().map(|(name, _)| name.clone()).collect(),
        _ => Vec::new(),
    }
}

fn fixture(text: &str) -> Fixture {
    let mut loader = MemoryLoader::new();
    let mut host_parts = (Vec::new(), Vec::new(), Vec::new());
    let mut symbols = Vec::new();
    let mut implicit = Vec::new();
    let mut formats = Vec::new();
    let mut limits = Limits::default();
    let mut root_struct = "Root".to_owned();
    let mut entries: Vec<PathBuf> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let mut sections: Vec<(String, String, String)> = Vec::new();
    let mut current: Option<(String, String, String)> = None;
    for line in text.split_inclusive('\n') {
        if let Some(header) = line.strip_prefix("#- ") {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            let (kind, name) = header
                .trim()
                .split_once(' ')
                .expect("a section has a kind and a name");
            current = Some((kind.to_owned(), name.to_owned(), String::new()));
            continue;
        }
        match &mut current {
            Some((_, _, body)) => body.push_str(line),
            None => {
                let Some(option) = line.strip_prefix("#! ") else {
                    current = Some(("file".to_owned(), "main.stav".to_owned(), line.to_owned()));
                    continue;
                };
                let words: Vec<&str> = option.split_whitespace().collect();
                match words.as_slice() {
                    ["root", name] => root_struct = (*name).to_owned(),
                    ["entries", paths @ ..] => {
                        entries = paths.iter().map(|p| Path::new(ROOT_DIR).join(p)).collect();
                    }
                    ["symbol", name, constructor, flags @ ..] => symbols.push(SymbolType {
                        name: (*name).to_owned(),
                        words: flags.contains(&"words"),
                        strings: flags.contains(&"strings"),
                        constructor: (*constructor).to_owned(),
                        parse: lowercase,
                    }),
                    ["implicit", target] => implicit.push(ImplicitRefs {
                        target: (*target).to_owned(),
                        labels: user_groups,
                    }),
                    ["render", names @ ..] => {
                        formats = names
                            .iter()
                            .map(|n| match *n {
                                "json" => Format::Json,
                                "toml" => Format::Toml,
                                "yaml" => Format::Yaml,
                                _ => panic!("unknown format {n:?}"),
                            })
                            .collect();
                    }
                    ["steps", n] => limits.steps = n.parse().expect("a step limit"),
                    _ => panic!("unknown option {option:?}"),
                }
            }
        }
    }
    if let Some(section) = current.take() {
        sections.push(section);
    }
    for (kind, name, body) in sections {
        match kind.as_str() {
            "file" => {
                let path = Path::new(ROOT_DIR).join(&name);
                files.push(path.clone());
                loader = loader.with_file(path, body);
            }
            "std" => loader = loader.with_std(name, body),
            "prelude" => host_parts.0.push(Source::new(name, body)),
            "part" => host_parts.1.push(Source::new(name, body)),
            "host" => host_parts.2.push(HostModule {
                path: name,
                source: body,
            }),
            _ => panic!("unknown section kind {kind:?}"),
        }
    }
    if entries.is_empty() {
        entries.extend(files.first().cloned());
    }
    if formats.is_empty() {
        formats.push(Format::Json);
    }
    let mut host = Host::new(loader);
    host.prelude = host_parts.0;
    host.implicit_parts = host_parts.1;
    host.host_modules = host_parts.2;
    host.symbols = symbols;
    host.implicit_refs = implicit;
    Fixture {
        host,
        root: RootSpec {
            root_struct,
            entries,
            import_roots: vec![ImportRoot::new(ROOT_DIR)],
        },
        formats,
        limits,
    }
}

/// Evaluates a fixture and renders the value in its formats, or the
/// diagnostics.
fn run(text: &str) -> (bool, String) {
    let fixture = fixture(text);
    let checked = match check(&fixture.host, &fixture.root) {
        Ok(checked) => checked,
        Err(diagnostics) => panic!("the fixture does not check:\n{diagnostics}"),
    };
    match checked.eval(&fixture.limits) {
        Ok(evaluated) => {
            let mut out = String::new();
            for format in fixture.formats {
                match evaluated.render(format) {
                    Ok(text) => out.push_str(&format!("--- {format}\n{text}")),
                    Err(diagnostics) => {
                        out.push_str(&format!("--- {format} errors\n{diagnostics}"));
                    }
                }
            }
            (true, out)
        }
        Err(diagnostics) => {
            assert!(
                diagnostics.has_errors(),
                "a failed evaluation reports an error"
            );
            (false, format!("--- errors\n{diagnostics}"))
        }
    }
}

fn fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("the fixture directory exists")
        .map(|entry| entry.expect("the fixture directory is readable").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "stav"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "{} has no fixtures", dir.display());
    paths
}

fn name(path: &Path) -> &str {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .expect("the name is UTF-8")
}

#[test]
fn fixtures_evaluate_to_their_snapshots() {
    let mut unexpected = Vec::new();
    for path in fixtures() {
        let text = fs::read_to_string(&path).expect("the fixture is readable");
        let name = name(&path);
        let (ok, output) = run(&text);
        if ok == name.starts_with("err_") {
            unexpected.push(format!("{name}:\n{output}"));
        }
        insta::assert_snapshot!(name, output);
    }
    assert!(
        unexpected.is_empty(),
        "unexpected results:\n{}",
        unexpected.join("\n")
    );
}

#[test]
fn evaluating_twice_gives_identical_canonical_json() {
    for path in fixtures() {
        let text = fs::read_to_string(&path).expect("the fixture is readable");
        let first = canonical(&text);
        assert_eq!(
            first,
            canonical(&text),
            "{} is not deterministic",
            path.display()
        );
    }
}

fn canonical(text: &str) -> Result<String, String> {
    let fixture = fixture(text);
    let checked = check(&fixture.host, &fixture.root).map_err(|d| d.to_string())?;
    checked
        .eval(&fixture.limits)
        .map(|e| e.to_canonical_json())
        .map_err(|d| d.to_string())
}

fn evaluate(text: &str) -> stav::Evaluated {
    let fixture = fixture(text);
    let checked = check(&fixture.host, &fixture.root).expect("the fixture checks");
    checked
        .eval(&fixture.limits)
        .expect("the fixture evaluates")
}

/// Returns JSON data with every number as a float, because canonical JSON
/// writes a float with an integral value as an integer.
fn numeric(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Number(n) => n
            .as_f64()
            .map_or(serde_json::Value::Null, serde_json::Value::from),
        serde_json::Value::Object(map) => {
            serde_json::Value::Object(map.into_iter().map(|(k, v)| (k, numeric(v))).collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(numeric).collect())
        }
        other => other,
    }
}

/// Returns JSON data without the object entries that are `null`, which
/// TOML leaves out.
fn without_nulls(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, without_nulls(v)))
                .collect(),
        ),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(without_nulls).collect())
        }
        other => other,
    }
}

fn yaml_to_json(yaml: &saphyr::Yaml<'_>) -> serde_json::Value {
    use saphyr::{Scalar, Yaml};
    match yaml {
        Yaml::Value(Scalar::Null) => serde_json::Value::Null,
        Yaml::Value(Scalar::Boolean(b)) => serde_json::Value::Bool(*b),
        Yaml::Value(Scalar::Integer(i)) => serde_json::Value::from(*i),
        Yaml::Value(Scalar::FloatingPoint(f)) => serde_json::Value::from(f.into_inner()),
        Yaml::Value(Scalar::String(s)) => serde_json::Value::String(s.to_string()),
        Yaml::Sequence(items) => serde_json::Value::Array(items.iter().map(yaml_to_json).collect()),
        Yaml::Mapping(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = match yaml_to_json(k) {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    };
                    (key, yaml_to_json(v))
                })
                .collect(),
        ),
        other => panic!("unexpected YAML node {other:?}"),
    }
}

#[test]
fn renderings_parse_back_to_the_same_data() {
    use saphyr::LoadableYamlNode;
    for path in fixtures() {
        if name(&path).starts_with("err_") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("the fixture is readable");
        let evaluated = evaluate(&text);
        let expected = numeric(
            serde_json::from_str(&evaluated.to_canonical_json()).expect("canonical JSON parses"),
        );
        let json = numeric(
            serde_json::from_str(&evaluated.render(Format::Json).expect("JSON renders"))
                .expect("JSON parses"),
        );
        assert_eq!(json, expected, "JSON of {}", path.display());
        let yaml = evaluated.render(Format::Yaml).expect("YAML renders");
        let documents = saphyr::Yaml::load_from_str(&yaml).expect("YAML parses");
        assert_eq!(
            numeric(yaml_to_json(&documents[0])),
            expected,
            "YAML of {}:\n{yaml}",
            path.display()
        );
        if let Ok(toml) = evaluated.render(Format::Toml) {
            let parsed = numeric(toml::from_str(&toml).expect("TOML parses"));
            assert_eq!(
                parsed,
                without_nulls(expected),
                "TOML of {}:\n{toml}",
                path.display()
            );
        }
    }
}

#[test]
fn values_convert_to_host_types_through_serde() {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(rename_all = "lowercase")]
    enum Build {
        Native { features: Vec<String> },
        Script { target: String },
    }

    #[derive(Debug, PartialEq, Deserialize)]
    struct Job {
        build: Build,
        timeout: u32,
    }

    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/eval/example_tagged_blocks.stav");
    let evaluated = evaluate(&fs::read_to_string(path).expect("the fixture is readable"));
    let jobs: BTreeMap<String, Job> = evaluated
        .get(&stav::AttrPath::new(["jobs"]))
        .expect("the jobs convert");
    assert_eq!(
        jobs["finder"],
        Job {
            build: Build::Native {
                features: vec!["simd".to_owned()]
            },
            timeout: 600
        }
    );
    assert_eq!(
        jobs["hello"].build,
        Build::Script {
            target: "all".to_owned()
        }
    );
    let error = evaluated
        .get::<u8>(&stav::AttrPath::new(["jobs", "finder", "timeout"]))
        .expect_err("600 does not fit a byte");
    assert_eq!(
        error.iter().next().map(|d| d.diagnostic.code),
        Some("E0805")
    );
    assert!(
        evaluated
            .get::<u32>(&stav::AttrPath::new(["jobs", "missing"]))
            .is_err()
    );
}

#[test]
fn attributes_render_alone_and_errors_name_their_whole_path() {
    let evaluated =
        evaluate("struct Server { port: Int = 80 }\nstruct Root { server: Server = Server {} }\n");
    let server = stav::AttrPath::new(["server"]);
    assert_eq!(
        evaluated
            .render_at(&server, Format::Toml)
            .expect("a table renders as TOML"),
        "port = 80\n"
    );
    let errors = |diagnostics: stav::Diagnostics| -> Vec<String> {
        diagnostics
            .iter()
            .map(|d| format!("{} {}", d.diagnostic.code, d.diagnostic.message))
            .collect()
    };
    let scalar = evaluated.render_at(&server.join("port"), Format::Toml);
    assert_eq!(
        errors(scalar.expect_err("a scalar is not a TOML table")),
        ["E0804 cannot render `server.port` as TOML: TOML needs a table at the top"]
    );
    let missing = evaluated.render_at(&server.join("host"), Format::Json);
    assert_eq!(
        errors(missing.expect_err("there is no host")),
        ["E0805 there is no attribute `server.host`"]
    );
}

#[test]
fn spans_and_symbols_point_at_the_source() {
    let text = "#! symbol Pkg pkg words\n#! entries a.stav b.stav\n#- file a.stav\n\
                struct Root { port: Int = 80, packages: List<Pkg> = [] }\nport = default(8080)\n\
                packages = [Editor]\n#- file b.stav\nport = 443\n";
    let evaluated = evaluate(text);
    let spans = evaluated.spans(&stav::AttrPath::new(["port"]));
    let texts: Vec<(usize, Span)> = spans.iter().map(|s| (s.file.index(), s.span)).collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    let first = spans[0];
    assert_eq!(
        evaluated
            .diagnostics([])
            .source(first.file)
            .map(|s| &s[first.span.range()]),
        Some("443")
    );
    let symbols: Vec<&str> = evaluated.symbols("Pkg").map(|(text, _)| text).collect();
    assert_eq!(symbols, ["editor"]);
}

/// Returns the source text of every span of the attribute at `path`.
fn span_texts(evaluated: &stav::Evaluated, path: &str) -> Vec<String> {
    let path: stav::AttrPath = path.parse().expect("the path parses");
    let sources = evaluated.diagnostics([]);
    evaluated
        .spans(&path)
        .iter()
        .map(|s| {
            let source = sources.source(s.file).expect("the file of a span exists");
            source[s.span.range()].to_owned()
        })
        .collect()
}

#[test]
fn spans_point_at_list_elements_and_literal_entries() {
    let text = "#! entries a.stav b.stav\n#- file a.stav\n\
                struct Server { host: Str, port: Int = 80 }\n\
                struct Root {\n  ports: List<Int> = []\n  zones: Set<Str> = []\n  \
                env: Map<Str, Str> = {}\n  servers: List<Server> = []\n  \
                server: Server = { host = \"a\" }\n  d: Data = none\n}\n\
                fn more() -> List<Int> = [8080]\n\
                ports = [80, 443]\nports = more()\nzones = [\"west\", \"east\"]\n\
                env = { LANG = \"C\", TZ = \"UTC\" }\nservers = [{ host = \"x\" }]\n\
                server = { host = \"b\", port = 8 }\nd = { a = [1] }\n\
                #- file b.stav\nzones = [\"east\"]\n";
    let evaluated = evaluate(text);
    let cases: &[(&str, &[&str])] = &[
        ("ports.0", &["80"]),
        ("ports.1", &["443"]),
        ("ports.2", &["more()"]),
        ("ports.3", &[]),
        ("zones.0", &["\"east\"", "\"east\""]),
        ("zones.1", &["\"west\""]),
        ("env.TZ", &["TZ = \"UTC\""]),
        ("servers.0", &["{ host = \"x\" }"]),
        ("servers.0.host", &["host = \"x\""]),
        ("servers.0.port", &["{ host = \"x\" }"]),
        ("servers.0.nothing", &[]),
        ("server.port", &["port = 8", "80"]),
        ("d.a.0", &["{ a = [1] }", "none"]),
    ];
    for (path, expected) in cases {
        assert_eq!(&span_texts(&evaluated, path), expected, "{path}");
    }
}

#[test]
fn priorities_report_how_attributes_were_set() {
    use stav::Priority::{Declared, Default, Force, Plain};
    let text = "struct Server { host: Str = \"localhost\", port: Int = 80 }\n\
                struct Root {\n  server: Server\n  tags: List<Str> = []\n  \
                env: Map<Str, Str> = {}\n}\n\
                server { port = default(8080) }\nserver.port = force(443)\n\
                tags = default([\"a\"])\nenv.LANG = default(\"C\")\n";
    let evaluated = evaluate(text);
    let cases = [
        ("server", Some(Plain)),
        ("server.port", Some(Force)),
        ("server.host", Some(Declared)),
        ("tags", Some(Default)),
        ("tags.0", Some(Default)),
        ("env", Some(Default)),
        ("env.LANG", Some(Default)),
        ("missing", None),
    ];
    for (path, expected) in cases {
        let path: stav::AttrPath = path.parse().expect("the path parses");
        assert_eq!(evaluated.priority(&path), expected, "{path}");
    }
}

#[test]
fn symbol_paths_name_the_value_that_holds_each_symbol() {
    let text = "#! symbol Pkg pkg words\n\
                type Build = native(deps: List<Pkg> = []) | script\n\
                struct Job { build: Build = script, tool: Option<Pkg> }\n\
                struct Root {\n  packages: List<Pkg> = []\n  extra: Set<Pkg> = []\n  \
                jobs: Map<Str, Job> = {} block job\n}\n\
                packages = [shell, editor]\nextra = [prompt]\n\
                job web { build = native(deps = [compiler]), tool = native }\n";
    let evaluated = evaluate(text);
    let found: Vec<String> = evaluated
        .symbol_paths("Pkg")
        .map(|(text, path, _)| format!("{path} {text}"))
        .collect();
    assert_eq!(
        found,
        [
            "packages.0 shell",
            "packages.1 editor",
            "extra.0 prompt",
            "jobs.web.build.native.deps.0 compiler",
            "jobs.web.tool native",
        ]
    );
    for (_, path, at) in evaluated.symbol_paths("Pkg") {
        assert!(evaluated.value().at(path).is_some(), "{path}");
        assert_eq!(evaluated.spans(path), [at], "{path}");
    }
}

#[test]
fn spans_point_inside_variant_payloads() {
    let text = "struct Target { host: Str, port: Int = 80 }\n\
                type Retry = never | backoff(limit: Int, delays: List<Int> = [])\n\
                type Ready = manual | check(cmd: List<Str>, interval: Int = 5, \
                retry: Retry = never, env: Map<Str, Str> = {}, tags: Set<Str> = [], \
                targets: List<Target> = [])\n\
                type Build = native(features: List<Str> = []) | script\n\
                struct Service { ready: Ready = manual, build: Build = script }\n\
                struct Root { services: Map<Str, Service> = {} block service }\n\
                service x {\n  ready = check(cmd = [\"serve\", \"-p\"], interval = 1, \
                retry = backoff(limit = 3, delays = [1, 2]), env = { A = \"1\" }, \
                tags = [\"b\", \"a\"], targets = [{ host = \"h\" }])\n}\n\
                service y { ready = check([\"probe\"]), build = native(features = [\"x\"]) }\n\
                service w {\n  build native { }\n  build = native(features = [\"v\"])\n}\n";
    let evaluated = evaluate(text);
    let ready = "check(cmd = [\"serve\", \"-p\"], interval = 1, \
                 retry = backoff(limit = 3, delays = [1, 2]), env = { A = \"1\" }, \
                 tags = [\"b\", \"a\"], targets = [{ host = \"h\" }])";
    let x = "services.x.ready.check";
    let cases: &[(&str, &[&str])] = &[
        ("services.x.ready", &[ready, "manual"]),
        (x, &[ready]),
        ("services.x.ready.check.cmd", &["cmd = [\"serve\", \"-p\"]"]),
        ("services.x.ready.check.cmd.1", &["\"-p\""]),
        ("services.x.ready.check.interval", &["interval = 1"]),
        (
            "services.x.ready.check.retry.backoff",
            &["backoff(limit = 3, delays = [1, 2])"],
        ),
        ("services.x.ready.check.retry.backoff.limit", &["limit = 3"]),
        ("services.x.ready.check.retry.backoff.delays.1", &["2"]),
        ("services.x.ready.check.env.A", &["A = \"1\""]),
        ("services.x.ready.check.tags.0", &["\"a\""]),
        ("services.x.ready.check.targets.0.host", &["host = \"h\""]),
        (
            "services.x.ready.check.targets.0.port",
            &["{ host = \"h\" }"],
        ),
        ("services.x.ready.check.nothing", &[]),
        ("services.x.ready.cmd", &[]),
        ("services.y.ready.check.cmd.0", &["\"probe\""]),
        ("services.y.ready.check.interval", &["check([\"probe\"])"]),
        ("services.y.build.native.features.0", &["\"x\""]),
        (
            "services.w.build.native.features",
            &["features = [\"v\"]", "[]"],
        ),
        ("services.w.build.native.features.0", &["\"v\""]),
    ];
    for (path, expected) in cases {
        assert_eq!(&span_texts(&evaluated, path), expected, "{path}");
        let path: stav::AttrPath = path.parse().expect("the path parses");
        assert_eq!(
            evaluated.value().at(&path).is_some(),
            !expected.is_empty(),
            "{path}"
        );
    }
}

/// Returns a program that reads the host module `build/target`, checked
/// once.
fn target_program() -> stav::Checked {
    let main = "import \"build/target\" as target\n\
                struct Root { triple: Str, jobs: Int, features: List<Str> }\n\
                triple   = \"#{target.arch}-#{target.os}\"\n\
                jobs     = target.jobs * 2\n\
                features = target.features\n";
    let module = "type Os = linux | darwin\n\
                  let arch: Str = \"x86_64\"\n\
                  let os: Os = linux\n\
                  let jobs: Int where 1..=64 = 1\n\
                  let features: List<Str> = []\n\
                  let fixed = 1\n";
    let mut host = Host::new(MemoryLoader::new().with_file("app/main.stav", main));
    host.host_modules.push(HostModule {
        path: "build/target".to_owned(),
        source: module.to_owned(),
    });
    host.host_modules.push(HostModule {
        path: "build/unused".to_owned(),
        source: "let level: Int = 1\n".to_owned(),
    });
    let root = RootSpec {
        root_struct: "Root".to_owned(),
        entries: vec!["app/main.stav".into()],
        import_roots: vec![ImportRoot::new(ROOT_DIR)],
    };
    check(&host, &root).expect("the program checks")
}

#[test]
fn one_check_evaluates_with_different_host_values() {
    let checked = target_program();
    let plain = checked
        .eval(&Limits::default())
        .expect("the program evaluates");
    assert_eq!(
        plain.to_canonical_json(),
        r#"{"features":[],"jobs":2,"triple":"x86_64-linux"}"#
    );
    let targets = [
        (
            "aarch64",
            "darwin",
            4,
            r#"{"features":["simd"],"jobs":8,"triple":"aarch64-darwin"}"#,
        ),
        (
            "riscv64",
            "linux",
            16,
            r#"{"features":["simd"],"jobs":32,"triple":"riscv64-linux"}"#,
        ),
    ];
    for (arch, os, jobs, expected) in targets {
        let values = stav::HostValues::new()
            .with("build/target", "arch", Value::Str(arch.to_owned()))
            .with("build/target", "os", Value::Str(os.to_owned()))
            .with("build/target", "jobs", Value::Int(jobs))
            .with(
                "build/target",
                "features",
                Value::List(vec![Value::Str("simd".to_owned())]),
            );
        let evaluated = checked
            .eval_with(&Limits::default(), &values)
            .expect("the program evaluates");
        assert_eq!(evaluated.to_canonical_json(), expected);
    }
}

#[test]
fn host_values_must_fit_their_declarations() {
    let checked = target_program();
    let cases = [
        (
            "arch",
            Value::Int(1),
            "E0614",
            "the value given for `arch` is not a `Str`",
        ),
        (
            "os",
            Value::Str("windows".to_owned()),
            "E0614",
            "`windows` is no variant",
        ),
        ("jobs", Value::Int(100), "E0803", "violates a refinement"),
        (
            "features",
            Value::List(vec![Value::Bool(true)]),
            "E0614",
            "found a `Bool`",
        ),
        ("fixed", Value::Int(2), "E0614", "no annotated `let`"),
        ("missing", Value::Int(2), "E0614", "no annotated `let`"),
    ];
    for (name, value, code, message) in cases {
        let values = stav::HostValues::new().with("build/target", name, value);
        let errors = checked
            .eval_with(&Limits::default(), &values)
            .expect_err("the value is rejected");
        let error = errors.iter().next().expect("there is an error");
        assert_eq!(error.diagnostic.code, code, "{name}");
        assert!(errors.to_string().contains(message), "{name}: {errors}");
    }
    let values = stav::HostValues::new().with("build/other", "arch", Value::Int(1));
    let errors = checked
        .eval_with(&Limits::default(), &values)
        .expect_err("an unknown module is rejected");
    assert!(
        errors.to_string().contains("which is no host module"),
        "{errors}"
    );
}

#[test]
fn host_values_for_modules_the_program_does_not_import_are_ignored() {
    let checked = target_program();
    let values = stav::HostValues::new()
        .with("build/unused", "level", Value::Str("not an Int".to_owned()))
        .with("build/unused", "missing", Value::Int(1));
    let evaluated = checked
        .eval_with(&Limits::default(), &values)
        .expect("the values are ignored");
    assert_eq!(
        evaluated.to_canonical_json(),
        r#"{"features":[],"jobs":2,"triple":"x86_64-linux"}"#
    );
}

/// Returns a program whose host module `build/target` has a struct input
/// with defaulted fields, checked once.
fn toolchain_program() -> stav::Checked {
    let main = "import \"build/target\" as target\n\
                struct Root { toolchain: target.Toolchain }\n\
                toolchain = target.toolchain\n";
    let module = "struct Toolchain {\n\
                  cc: Str\n\
                  jobs: Int where 1..=64 = 2\n\
                  flags: List<Str> = [\"-j#{self.jobs}\"]\n\
                  linker: Option<Str>\n\
                  }\n\
                  let toolchain: Toolchain = Toolchain { cc = \"cc\" }\n";
    let mut host = Host::new(MemoryLoader::new().with_file("app/main.stav", main));
    host.host_modules.push(HostModule {
        path: "build/target".to_owned(),
        source: module.to_owned(),
    });
    let root = RootSpec {
        root_struct: "Root".to_owned(),
        entries: vec!["app/main.stav".into()],
        import_roots: vec![ImportRoot::new(ROOT_DIR)],
    };
    check(&host, &root).expect("the program checks")
}

fn toolchain(fields: &[(&str, Value)]) -> stav::HostValues {
    let fields = fields
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.clone()))
        .collect();
    stav::HostValues::new().with("build/target", "toolchain", Value::Map(fields))
}

#[test]
fn host_struct_values_take_the_defaults_of_omitted_fields() {
    let checked = toolchain_program();
    let cases = [
        (
            toolchain(&[("cc", Value::Str("clang".to_owned()))]),
            r#"{"toolchain":{"cc":"clang","flags":["-j2"],"jobs":2,"linker":null}}"#,
        ),
        (
            toolchain(&[
                ("cc", Value::Str("clang".to_owned())),
                ("jobs", Value::Int(8)),
                ("linker", Value::Str("lld".to_owned())),
            ]),
            r#"{"toolchain":{"cc":"clang","flags":["-j8"],"jobs":8,"linker":"lld"}}"#,
        ),
    ];
    for (values, expected) in cases {
        let evaluated = checked
            .eval_with(&Limits::default(), &values)
            .expect("the program evaluates");
        assert_eq!(evaluated.to_canonical_json(), expected);
    }
}

#[test]
fn host_struct_values_must_fit_their_fields() {
    let checked = toolchain_program();
    let cases = [
        (
            toolchain(&[("jobs", Value::Int(8))]),
            "E0614",
            "the field `cc` is missing",
        ),
        (
            toolchain(&[("cc", Value::Int(1))]),
            "E0614",
            "expected `Str`, found an `Int`",
        ),
        (
            toolchain(&[("cc", Value::Str("cc".to_owned())), ("jobs", Value::Int(0))]),
            "E0803",
            "violates a refinement",
        ),
    ];
    for (values, code, message) in cases {
        let errors = checked
            .eval_with(&Limits::default(), &values)
            .expect_err("the value is rejected");
        let error = errors.iter().next().expect("there is an error");
        assert_eq!(error.diagnostic.code, code, "{message}");
        assert!(errors.to_string().contains(message), "{errors}");
    }
}

/// Asserts that a program evaluates within a few seconds, far above what
/// its size needs when no step is quadratic.
fn evaluates_quickly(text: &str) {
    let start = std::time::Instant::now();
    evaluate(text);
    let elapsed = start.elapsed();
    assert!(elapsed < std::time::Duration::from_secs(10), "{elapsed:?}");
}

#[test]
fn large_maps_evaluate_in_linear_time() {
    let n = 100_000;
    evaluates_quickly(&format!(
        "struct Root {{ m: Map<Str, Int> = {{}} }}\nm = {{for i in range(0, {n}) -> \"k#{{i}}\" = i}}\n"
    ));
    let entries: Vec<String> = (0..n).map(|i| format!("k{i} = {i}")).collect();
    evaluates_quickly(&format!(
        "struct Root {{ m: Map<Str, Int> = {{}} }}\nm = {{ {} }}\n",
        entries.join(", ")
    ));
    evaluates_quickly(&format!(
        "struct Root {{ d: Data = none }}\n\
         d = {{for i in range(0, {n}) -> \"a#{{i}}\" = i}}\n\
         d = {{for i in range(0, {n}) -> \"b#{{i}}\" = i}}\n"
    ));
}

#[test]
fn long_scopes_evaluate_in_linear_time() {
    let n = 150_000;
    let bindings: Vec<String> = (1..n).map(|i| format!("v{i} = v0")).collect();
    evaluates_quickly(&format!(
        "struct Root {{ a: Int = 0 }}\na = (let v0 = 1, {} in v{})\n",
        bindings.join(", "),
        n - 1
    ));
}

#[test]
fn deeply_nested_evaluation_stops_without_overflowing_the_stack() {
    let mut functions = String::from("fn f0(x: Int) -> Int = x + 1\n");
    for i in 1..2000 {
        functions.push_str(&format!("fn f{i}(x: Int) -> Int = f{}(x) + 1\n", i - 1));
    }
    let deep = format!("struct Root {{ n: Int = 0 }}\n{functions}n = f1999(0)\n");
    let data = "struct Root { d: Data = none }\n\
                d = fold(range(0, 1000), [], fn(acc: Data, i: Int) -> Data = [acc])\n"
        .to_owned();
    for text in [deep, data] {
        let result = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let fixture = fixture(&text);
                let checked = check(&fixture.host, &fixture.root).expect("the program checks");
                checked
                    .eval(&Limits::default())
                    .map(|_| ())
                    .map_err(|d| d.to_string())
            })
            .expect("the thread starts")
            .join()
            .expect("evaluation does not overflow the stack");
        let error = result.expect_err("evaluation stops");
        assert!(
            error.contains("E0611") || error.contains("E0612"),
            "{error}"
        );
    }
}

#[test]
fn long_chains_of_equal_parts_merge_once_without_overflowing_the_stack() {
    let text = "struct Root { tags: List<Str> = [] }\n\
                fn chain(n: Int) -> Part<Root> = fold(range(0, n), part { tags = [\"x\"] },\n\
                  fn(acc: Part<Root>, i: Int) -> Part<Root> = part { include acc })\n\
                include chain(2000)\ninclude chain(2000)\n";
    let json = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || evaluate(text).to_canonical_json())
        .expect("the thread starts")
        .join()
        .expect("evaluation does not overflow the stack");
    assert_eq!(json, r#"{"tags":["x"]}"#);
}

mod random {
    use proptest::prelude::*;
    use proptest::sample::select;

    use super::*;

    /// Integer expressions over the given names.
    fn int(depth: u32, names: &'static [&'static str]) -> BoxedStrategy<String> {
        let leaf = prop_oneof![
            (0i64..100).prop_map(|i| i.to_string()),
            select(names.to_vec()).prop_map(str::to_owned),
            Just("len(\"abc\")".to_owned()),
        ];
        if depth == 0 {
            return leaf.boxed();
        }
        let inner = || int(depth - 1, names);
        prop_oneof![
            leaf,
            (inner(), select(vec!["+", "-", "*", "/", "%"]), inner())
                .prop_map(|(a, op, b)| format!("({a} {op} {b})")),
            (inner(), inner(), inner())
                .prop_map(|(c, a, b)| format!("(if {c} > 10 {{ {a} }} else {{ {b} }})")),
            (inner(), inner()).prop_map(|(n, v)| format!(
                "fold(range(0, {n}), {v}, fn(acc: Int, i: Int) = acc + i)"
            )),
            inner().prop_map(|v| format!("(let y = {v} in y * y)")),
        ]
        .boxed()
    }

    /// Names an eager expression may use.
    const EAGER: &[&str] = &["x", "1"];
    /// Names a deferred expression may use as well.
    const LATE: &[&str] = &["x", "self.bounded"];

    fn text(depth: u32) -> BoxedStrategy<String> {
        prop_oneof![
            select(vec!["\"a\"", "\"b c\"", "\"\"", "self.s"]).prop_map(str::to_owned),
            int(depth, LATE).prop_map(|i| format!("\"n#{{{i}}}\"")),
            (int(depth, EAGER), int(depth, LATE))
                .prop_map(|(a, b)| format!("join([toStr({a}), toStr({b})], \"-\")")),
        ]
        .boxed()
    }

    fn statement() -> impl Strategy<Value = String> {
        let prio = select(vec!["", "default", "force"]);
        prop_oneof![
            (prio.clone(), int(2, LATE)).prop_map(|(p, v)| format!("n = {p}({v})")),
            (prio, int(1, EAGER)).prop_map(|(p, v)| format!("s = {p}(\"#{{{v}}}\")")),
            (int(1, EAGER), text(1)).prop_map(|(a, b)| format!("tags = [toStr({a}), {b}]")),
            int(1, LATE).prop_map(|v| format!("list = [{v}]")),
            (select(vec!["a", "b", "c"]), int(1, EAGER), text(1))
                .prop_map(|(k, v, w)| format!("item {k} {{ v = {v}\n w = {w} }}")),
            (select(vec!["a", "b"]), int(1, EAGER))
                .prop_map(|(k, v)| format!("items.{k}.v = force({v})")),
            (int(1, EAGER), int(1, LATE))
                .prop_map(|(c, v)| format!("if {c} > 5 {{ list = [{v}] }}")),
            (0u8..4).prop_map(|k| format!("for i in range(0, {k}) {{ tags = [\"#{{i}}\"] }}")),
            int(1, EAGER).prop_map(|v| format!("include part {{ list = [{v}] }}")),
            int(1, EAGER).prop_map(|v| format!("bounded = {v}")),
        ]
    }

    fn program() -> impl Strategy<Value = String> {
        (prop::collection::vec(statement(), 0..8), int(1, &["1"])).prop_map(|(statements, x)| {
            format!(
                "#! steps 20000\nstruct Item {{ v: Int = 0, w: Str = \"#{{self.v}}\" }}\n\
                 struct Root {{\n  n: Int = 0\n  s: Str = \"\"\n  tags: Set<Str> = []\n  \
                 list: List<Int> = []\n  items: Map<Str, Item> = {{}} block item\n  \
                 bounded: Int where 0..100 = 1\n}}\nlet x = {x}\n{}\n",
                statements.join("\n")
            )
        })
    }

    fn outcome(text: &str) -> Option<String> {
        let fixture = fixture(text);
        let checked = check(&fixture.host, &fixture.root).ok()?;
        Some(match checked.eval(&fixture.limits) {
            Ok(evaluated) => evaluated.to_canonical_json(),
            Err(diagnostics) => diagnostics.to_string(),
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn evaluation_never_panics_and_is_deterministic(text in program()) {
            let first = outcome(&text);
            prop_assert_eq!(&first, &outcome(&text));
        }

        #[test]
        fn merging_does_not_depend_on_the_order_of_entry_files(
            entries in prop::collection::vec(
                (prop::collection::vec("[a-c]{1,2}", 0..3), prop::collection::btree_map("[d-f]", 0i64..9, 0..3)),
                1..4,
            ),
            order in Just(()).prop_perturb(|_, mut rng| rng.random::<u64>()),
        ) {
            let files: Vec<String> = entries
                .iter()
                .enumerate()
                .map(|(i, (tags, counts))| {
                    let tags: Vec<String> = tags.iter().map(|t| format!("\"{t}\"")).collect();
                    let counts: Vec<String> = counts
                        .iter()
                        .map(|(k, v)| format!("counts.f{i}{k} = {v}"))
                        .collect();
                    format!("tags = [{}]\n{}\n", tags.join(", "), counts.join("\n"))
                })
                .collect();
            let source = |permutation: &[usize]| {
                let names: Vec<String> = permutation.iter().map(|i| format!("e{i}.stav")).collect();
                let mut text = format!("#! entries root.stav {}\n#- file root.stav\n\
                    struct Root {{ tags: Set<Str> = [], counts: Map<Str, Int> = {{}} }}\n", names.join(" "));
                for &i in permutation {
                    text.push_str(&format!("#- file e{i}.stav\n{}", files[i]));
                }
                text
            };
            let forward: Vec<usize> = (0..files.len()).collect();
            let mut shuffled = forward.clone();
            let mut seed = order;
            for i in (1..shuffled.len()).rev() {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                let j = usize::try_from(seed >> 33).unwrap_or(0) % (i + 1);
                shuffled.swap(i, j);
            }
            prop_assert_eq!(outcome(&source(&forward)), outcome(&source(&shuffled)));
        }
    }
}

#[test]
fn late_whole_values_reject_equal_or_lower_priority_competitors() {
    for (ty, value, path, rhs) in [
        ("Map<Str, Int>", "{ a = n }", "m.a", "2"),
        ("Item", "Item { a = n }", "m.a", "2"),
        ("Data", "{ a = n }", "m", "{ b = 2 }"),
        ("Option<Map<Str, Int>>", "{ a = n }", "m.a", "2"),
        ("Option<Item>", "Item { a = n }", "m.a", "2"),
        ("Option<Data>", "{ a = n }", "m", "{ b = 2 }"),
    ] {
        for (whole, other) in [
            ("make(self.n)", rhs.to_owned()),
            ("make(self.n)", format!("default({rhs})")),
            ("force(make(self.n))", rhs.to_owned()),
            ("force(make(self.n))", format!("force({rhs})")),
            ("default(make(self.n))", format!("default({rhs})")),
        ] {
            let text = format!(
                "struct Item {{ a: Int = 0, b: Int = 3 }}\n\
                 struct Root {{ n: Int = 1, m: {ty} }}\n\
                 fn make(n: Int) -> {ty} = {value}\n\
                 m = {whole}\n{path} = {other}\n"
            );
            assert_late_whole_error(&text);
        }
    }
}

fn assert_late_whole_error(text: &str) {
    let fixture = fixture(text);
    let checked = check(&fixture.host, &fixture.root).expect("the program checks");
    let diagnostics = checked.eval(&fixture.limits).expect_err(text);
    let codes: Vec<_> = diagnostics.iter().map(|d| d.diagnostic.code).collect();
    assert_eq!(codes, ["E0504"], "{text}\n{diagnostics}");
    let diagnostic = &diagnostics
        .iter()
        .next()
        .expect("E0504 has a diagnostic")
        .diagnostic;
    assert!(diagnostic.labels.len() >= 2);
}

#[test]
fn late_whole_values_allow_only_higher_priority_contributions() {
    for (ty, value, path, rhs, expected) in [
        (
            "Map<Str, Int>",
            "{ a = n, c = n }",
            "m.a",
            "2",
            r#"{"a":2,"c":1}"#,
        ),
        ("Item", "Item { a = n }", "m.a", "2", r#"{"a":2,"b":3}"#),
        ("Data", "{ a = n }", "m", "{ b = 2 }", r#"{"b":2}"#),
        (
            "Option<Map<Str, Int>>",
            "{ a = n, c = n }",
            "m.a",
            "2",
            r#"{"a":2,"c":1}"#,
        ),
        (
            "Option<Item>",
            "Item { a = n }",
            "m.a",
            "2",
            r#"{"a":2,"b":3}"#,
        ),
        ("Option<Data>", "{ a = n }", "m", "{ b = 2 }", r#"{"b":2}"#),
    ] {
        for (whole, other) in [
            ("default(make(self.n))", rhs.to_owned()),
            ("default(make(self.n))", format!("force({rhs})")),
            ("make(self.n)", format!("force({rhs})")),
        ] {
            let text = format!(
                "struct Item {{ a: Int = 0, b: Int = 3 }}\n\
                 struct Root {{ n: Int = 1, m: {ty} }}\n\
                 fn make(n: Int) -> {ty} = {value}\n\
                 m = {whole}\n{path} = {other}\n"
            );
            let value: serde_json::Value =
                serde_json::from_str(&canonical(&text).expect(&text)).unwrap();
            assert_eq!(
                value["m"],
                serde_json::from_str::<serde_json::Value>(expected).unwrap(),
                "{text}"
            );
        }
    }
}

#[test]
fn late_whole_values_ignore_lower_priority_declared_defaults() {
    let text = "struct Root { n: Int = 1, m: Map<Str, Int> = {} }\n\
                fn make(n: Int) -> Map<Str, Int> = { a = n }\n\
                m = make(self.n)\n";
    assert_eq!(canonical(text).unwrap(), r#"{"m":{"a":1},"n":1}"#);
}

#[test]
fn optional_none_replaces_a_lower_priority_late_whole_without_evaluating_it() {
    let text = "struct Root { zero: Int = 0, m: Option<Map<Str, Int>> }\n\
                fn make(n: Int) -> Map<Str, Int> = { a = 1 / n }\n\
                m = make(self.zero)\nm = force(none)\n";
    assert_eq!(canonical(text).unwrap(), r#"{"m":null,"zero":0}"#);
}

#[test]
fn late_whole_defaults_do_not_compete_with_their_own_child_defaults() {
    let text = "struct Item { a: Int = 0, b: Int = 3 }\n\
                struct Root { n: Int = 1, m: Item = make(self.n) }\n\
                fn make(n: Int) -> Item = Item { a = n }\n";
    assert_eq!(canonical(text).unwrap(), r#"{"m":{"a":1,"b":3},"n":1}"#);
}

#[test]
fn late_whole_defaults_accept_explicit_default_contributions() {
    let text = "struct Item { a: Int = 0, b: Int = 3 }\n\
                struct Root { n: Int = 1, m: Item = make(self.n) }\n\
                fn make(n: Int) -> Item = Item { a = n }\n\
                m = default({ a = 2 })\n";
    assert_eq!(canonical(text).unwrap(), r#"{"m":{"a":2,"b":3},"n":1}"#);
}

#[test]
fn late_whole_static_errors_do_not_depend_on_produced_keys_or_cycles() {
    for n in [0, 1, 2] {
        for declared in ["", " = {}"] {
            let cases = [
                format!(
                    "struct Root {{ n: Int = {n}, k: Int = 2, m: Map<Str, Map<Str, Int>>{declared} }}\n\
                fn outer(n: Int) -> Map<Str, Map<Str, Int>> = if n == 1 {{ {{ x = {{ a = n }} }} }} else {{ {{}} }}\n\
                fn inner(k: Int) -> Map<Str, Int> = {{ b = k }}\n\
                m = force(outer(self.n))\nm.x = inner(self.k)\n"
                ),
                format!(
                    "struct Root {{ k: Int = {n}, n: Int = 0, m: Map<Str, Int>{declared} }}\n\
                fn make(k: Int) -> Map<Str, Int> = {{ a = k }}\n\
                m = force(make(self.k))\nm.a = self.n\nn = self.m[\"a\"]\n"
                ),
                format!(
                    "struct Root {{ k: Int = {n}, zero: Int = 0, m: Map<Str, Int>{declared} }}\n\
                fn make(k: Int) -> Map<Str, Int> = if k == 1 {{ {{}} }} else {{ {{ a = k }} }}\n\
                m = make(self.k)\nm.a = default(1 / self.zero)\n"
                ),
            ];
            for text in cases {
                assert_late_whole_error(&text);
            }
        }
    }
}
#[test]
fn late_whole_source_defaults_accept_subfields_and_complete_literals() {
    let prefix = "struct Source { url: Str, sha256: Str = \"\" }\n\
                  fn hosted(v: Str) -> Source = Source { url = \"u/#{v}\" }\n\
                  struct Pkg { version: Str, source: Source = hosted(self.version) }\n\
                  struct Root { pkgs: Map<Str, Pkg> = {} block pkg }\n";
    assert_eq!(
        canonical(&format!(
            "{prefix}pkg a {{ version = \"1\", source.sha256 = \"abc\" }}\n"
        ))
        .unwrap(),
        r#"{"pkgs":{"a":{"source":{"sha256":"abc","url":"u/1"},"version":"1"}}}"#
    );
    let failing = prefix.replace("url = \"u/#{v}\"", "url = \"#{1 / 0}/#{v}\"");
    assert_eq!(canonical(&format!("{failing}pkg a {{ version = \"1\", source = force({{ url = \"mirror\", sha256 = \"abc\" }}) }}\n")).unwrap(),
        r#"{"pkgs":{"a":{"source":{"sha256":"abc","url":"mirror"},"version":"1"}}}"#);
}

#[test]
fn nested_higher_late_wholes_do_not_validate_emitted_values() {
    for n in [0, 1] {
        let text = format!(
            "struct Root {{ n: Int = {n}, k: Int = 2, m: Map<Str, Map<Str, Int>> }}\n\
            fn outer(n: Int) -> Map<Str, Map<Str, Int>> = if n == 1 {{ {{ x = {{ a = n }}, y = {{ c = n }} }} }} else {{ {{}} }}\n\
            fn inner(k: Int) -> Map<Str, Int> = {{ b = k }}\n\
            m = default(outer(self.n))\nm.x = inner(self.k)\n"
        );
        let expected = if n == 1 {
            r#"{"k":2,"m":{"x":{"a":1,"b":2},"y":{"c":1}},"n":1}"#
        } else {
            r#"{"k":2,"m":{"x":{"b":2}},"n":0}"#
        };
        assert_eq!(canonical(&text).expect(&text), expected);
    }
}

#[test]
fn late_whole_values_find_competitors_from_entries_and_includes() {
    for other in ["#! entries main.stav overlay.stav\n", ""] {
        let text = format!(
            "{other}#- file main.stav\n\
             struct Root {{ n: Int = 1, m: Map<Str, Int> = {{}} }}\n\
             fn make(n: Int) -> Map<Str, Int> = {{ a = n }}\n\
             m = make(self.n)\n{}\
             #- file overlay.stav\nm.b = 2\n",
            if other.is_empty() {
                "include \"./overlay.stav\"\n"
            } else {
                ""
            }
        );
        let error = canonical(&text).expect_err("contributions across files also compete");
        assert!(error.contains("E0504"), "{error}");
        assert!(error.contains("app/main.stav:3:"), "{error}");
        assert!(error.contains("app/overlay.stav:1:"), "{error}");
        if other.is_empty() {
            assert!(error.contains("included from"), "{error}");
        }
    }
}

#[test]
fn map_values_and_field_default_expression_lets_keep_their_self_reads() {
    let text = "struct Root { key: Str = \"a\", n: Int = 1, m: Map<Str, Str> = {}, doubled: Int = let x = self.n in x + x }\n\
                n = force(4)\n\
                m = { fixed = self.key, \"constant\" = self.key }\n";
    assert_eq!(
        canonical(text).unwrap(),
        r#"{"doubled":8,"key":"a","m":{"constant":"a","fixed":"a"},"n":4}"#
    );
}

#[test]
fn late_map_and_struct_literals_remain_allowed_with_competitors() {
    let text = "struct Item { a: Int = 0, b: Int = 3 }\n\
                struct Root { n: Int = 1, m: Map<Str, Int> = {}, item: Item = {} }\n\
                m = ({ a = self.n })\nm.b = 2\n\
                item = (Item { a = self.n })\nitem.b = 2\n\
";
    assert_eq!(
        canonical(text).unwrap(),
        r#"{"item":{"a":1,"b":2},"m":{"a":1,"b":2},"n":1}"#
    );
}

#[test]
fn late_whole_values_check_other_contributions_at_the_whole_path() {
    for rhs in [
        "make(self.n)",
        "(make(self.n))",
        "let x = self.n in make(x)",
        "if self.n == 1 { make(1) } else { make(2) }",
    ] {
        let text = format!(
            "struct Root {{ n: Int = 1, m: Map<Str, Int> = {{}} }}\n\
             fn make(n: Int) -> Map<Str, Int> = {{ a = n }}\n\
             m = {rhs}\nm = {{ b = 2 }}\n"
        );
        let error = canonical(&text).expect_err("the whole path has another contribution");
        assert!(error.contains("E0504"), "{error}");
    }
}

#[test]
fn late_data_literals_are_whole_values_regardless_of_syntax() {
    for rhs in [
        "{ a = self.n }",
        "({ a = self.n })",
        "[self.n]",
        "\"#{self.n}\"",
    ] {
        let text = format!("struct Root {{ n: Int = 1, d: Data }}\nd = {rhs}\nd = {{ b = 2 }}\n");
        assert_late_whole_error(&text);
    }
}

#[test]
fn losing_late_scalar_is_not_evaluated() {
    let text = "struct Root { zero: Int = 0, x: Int }\n\
                x = default(1 / self.zero)\nx = 7\n";
    assert_eq!(canonical(text).unwrap(), r#"{"x":7,"zero":0}"#);
}

#[test]
fn losing_late_literal_children_are_not_evaluated() {
    for (ty, rhs) in [
        ("Item", "{ a = 1 / self.zero }"),
        ("Item", "(Item { a = 1 / self.zero })"),
        ("Map<Str, Int>", "({ a = 1 / self.zero })"),
    ] {
        let text = format!(
            "struct Item {{ a: Int }}\nstruct Root {{ zero: Int = 0, cfg: {ty} }}\n\
             cfg = default({rhs})\ncfg.a = 7\n"
        );
        assert_eq!(
            canonical(&text).expect(&text),
            r#"{"cfg":{"a":7},"zero":0}"#
        );
    }
}

#[test]
fn late_literals_preserve_nested_siblings_and_declared_defaults() {
    for (ty, rhs) in [
        ("Item", "{ a = 1 / self.zero, b = self.n }"),
        ("Item", "Item { a = 1 / self.zero, b = self.n }"),
        ("Map<Str, Int>", "{ a = 1 / self.zero, b = self.n }"),
    ] {
        for declared in [false, true] {
            let declaration = if declared {
                format!(" = {{ inner = {rhs} }}")
            } else {
                String::new()
            };
            let assignment = if declared {
                String::new()
            } else {
                format!("cfg = default({{ inner = {rhs} }})\n")
            };
            let text = format!(
                "struct Item {{ a: Int = 11, b: Int = 12 }}\nstruct Outer {{ inner: {ty} }}\n\
                 struct Root {{ zero: Int = 0, n: Int = 3, cfg: Outer{declaration} }}\n\
                 {assignment}cfg.inner.a = 7\n"
            );
            assert_eq!(
                canonical(&text).expect(&text),
                r#"{"cfg":{"inner":{"a":7,"b":3}},"n":3,"zero":0}"#
            );
        }
    }
}

#[test]
fn optional_none_discards_late_literal_children() {
    for ty in ["Item", "Map<Str, Int>"] {
        let text = format!(
            "struct Item {{ a: Int }}\nstruct Root {{ zero: Int = 0, cfg: Option<{ty}> }}\n\
             cfg = default({{ a = 1 / self.zero }})\ncfg = none\n"
        );
        assert_eq!(canonical(&text).expect(&text), r#"{"cfg":null,"zero":0}"#);
    }
}

#[test]
fn winning_late_children_and_losing_eager_children_still_error() {
    for (rhs, competitor) in [
        ("{ a = 1 / self.zero }", ""),
        ("{ a = 1 / 0 }", "cfg.a = 7"),
        ("{ a = 1 / 0, b = self.zero }", "cfg.a = 7"),
    ] {
        let text = format!(
            "struct Root {{ zero: Int = 0, cfg: Map<Str, Int> }}\n\
             cfg = default({rhs})\n{competitor}\n"
        );
        let error = canonical(&text).expect_err(&text);
        assert!(error.contains("division by zero"), "{error}");
    }
}

#[test]
fn data_keeps_whole_value_priority_for_late_literals() {
    let text = "struct Root { zero: Int = 0, cfg: Data }\n\
                cfg = default({ a = 1 / self.zero, b = 3 })\ncfg = { a = 7 }\n";
    assert_eq!(canonical(text).unwrap(), r#"{"cfg":{"a":7},"zero":0}"#);
}

#[test]
fn losing_literal_child_reads_do_not_add_cross_entry_cycles() {
    let text = "#! entries main.stav overlay.stav\n\
                #- file main.stav\n\
                struct Item { a: Int = 0, b: Int = 0 }\n\
                struct Root { n: Int = 1, seed: Int = 3, cfg: Item = {} }\n\
                cfg = default({ a = self.n, b = self.seed })\n\
                #- file overlay.stav\n\
                n = self.cfg.a\ncfg.a = 7\n";
    assert_eq!(
        canonical(text).unwrap(),
        r#"{"cfg":{"a":7,"b":3},"n":7,"seed":3}"#
    );
}

#[test]
fn winning_literal_child_reads_still_add_cross_entry_cycles() {
    let text = "#! entries main.stav overlay.stav\n\
                #- file main.stav\n\
                struct Item { a: Int = 0, b: Int = 0 }\n\
                struct Root { n: Int = 1, cfg: Item = {} }\n\
                cfg = { a = self.n }\n\
                #- file overlay.stav\nn = self.cfg.a\n";
    let error = canonical(text).expect_err("the winning child participates in the cycle");
    assert!(error.contains("E0607"), "{error}");
}

#[test]
fn late_literal_children_retain_their_entry_spans_and_priorities() {
    let evaluated = evaluate(
        "struct Root { zero: Int = 0, n: Int = 3, cfg: Map<Str, Int> = { a = 1 / self.zero, b = self.n } }\n\
         cfg.a = 7\n",
    );
    assert_eq!(span_texts(&evaluated, "cfg.a"), ["7", "a = 1 / self.zero"]);
    assert_eq!(span_texts(&evaluated, "cfg.b"), ["b = self.n"]);
    for (path, priority) in [
        ("cfg.a", stav::Priority::Plain),
        ("cfg.b", stav::Priority::Declared),
    ] {
        assert_eq!(evaluated.priority(&path.parse().unwrap()), Some(priority));
    }
}

#[test]
fn late_literal_child_conflicts_keep_include_provenance() {
    let text = "#- file main.stav\n\
                struct Root { n: Int = 1, cfg: Map<Str, Int> = {} }\n\
                include \"./other.stav\"\ncfg.a = 2\n\
                #- file other.stav\ncfg = { a = self.n }\n";
    let error = canonical(text).expect_err("the two plain values conflict");
    assert!(error.contains("E0501"), "{error}");
    assert!(error.contains("app/other.stav:1:9"), "{error}");
    assert!(error.contains("included from app/main.stav:2:1"), "{error}");
}

#[test]
fn losing_literal_children_are_deferred_through_late_bindings() {
    let text = "struct Root { zero: Int = 0, cfg: Map<Str, Int> }\n\
                let bad = 1 / self.zero\n\
                cfg = default({ a = bad, b = self.zero })\ncfg.a = 7\n";
    assert_eq!(
        canonical(text).unwrap(),
        r#"{"cfg":{"a":7,"b":0},"zero":0}"#
    );
}

#[test]
fn late_declared_literal_defaults_keep_each_instances_self_frame() {
    let text = "struct Item { n: Int, cfg: Map<Str, Int> = { a = 1 / self.n, b = self.n } }\n\
                struct Root { items: Map<Str, Item> = {} block item }\n\
                item first { n = 0, cfg.a = 7 }\n\
                item second { n = 2, cfg.a = 8 }\n";
    assert_eq!(
        canonical(text).unwrap(),
        r#"{"items":{"first":{"cfg":{"a":7,"b":0},"n":0},"second":{"cfg":{"a":8,"b":2},"n":2}}}"#
    );
}

#[test]
fn same_body_self_cycle_checks_with_both_edge_spans() {
    let text = "struct Root { version: Str, tag: Str }\n\
                version = \"#{self.tag}\"\ntag = \"v#{self.version}\"\n";
    let fixture = fixture(text);
    let checked = check(&fixture.host, &fixture.root)
        .expect("a provisional body cycle does not prevent checking");
    assert!(!checked.warnings().has_errors());
    let warnings: Vec<_> = checked.warnings().iter().collect();
    assert_eq!(warnings.len(), 1);
    let warning = &warnings[0].diagnostic;
    assert_eq!(warning.code, "W0607");
    assert_eq!(warning.severity, stav::Severity::Warning);
    let spans: Vec<_> = warning
        .labels
        .iter()
        .map(|label| &text[label.span.range()])
        .collect();
    assert_eq!(spans, ["\"#{self.tag}\"", "\"v#{self.version}\""]);
}

#[test]
fn same_body_self_cycle_errors_with_both_winning_edges() {
    let text = "struct Root { version: Str, tag: Str }\n\
                version = \"#{self.tag}\"\ntag = \"v#{self.version}\"\n";
    let fixture = fixture(text);
    let checked = check(&fixture.host, &fixture.root).expect("the body cycle only warns");
    let errors = checked
        .eval(&fixture.limits)
        .expect_err("the winning edges form a cycle");
    let errors: Vec<_> = errors.iter().collect();
    assert_eq!(errors.len(), 1);
    let error = &errors[0].diagnostic;
    assert_eq!(error.code, "E0607");
    assert_eq!(error.severity, stav::Severity::Error);
    let spans: Vec<_> = error
        .labels
        .iter()
        .map(|label| &text[label.span.range()])
        .collect();
    assert_eq!(spans, ["\"#{self.tag}\"", "\"v#{self.version}\""]);
    assert_eq!(error.labels[0].message, "`version` reads `tag`");
    assert_eq!(error.labels[1].message, "`tag` reads `version`");
}

#[test]
fn forced_entry_breaks_same_body_self_cycles() {
    for (fields, body, overlay, expected) in [
        (
            "version: Str, tag: Str",
            "version = \"#{self.tag}\"\ntag = \"v#{self.version}\"",
            "tag = force(\"v1\")",
            r#"{"tag":"v1","version":"v1"}"#,
        ),
        (
            "a: Int, b: Int",
            "a = self.b\nb = self.a",
            "a = force(1)",
            r#"{"a":1,"b":1}"#,
        ),
    ] {
        let text = format!(
            "#! entries main.stav overlay.stav\n#- file main.stav\n\
             struct Root {{ {fields} }}\n{body}\n#- file overlay.stav\n{overlay}\n"
        );
        let fixture = fixture(&text);
        let checked = check(&fixture.host, &fixture.root).expect("the body cycle only warns");
        assert!(
            checked
                .warnings()
                .iter()
                .any(|d| d.diagnostic.code == "W0607")
        );
        assert_eq!(
            canonical(&text).expect("the winning graph is acyclic"),
            expected
        );
    }
}

#[test]
fn self_cycles_in_parts_warn_and_can_be_broken_by_force() {
    for part in [
        "let cyclic: Part<Root> = part { a = self.b\nb = self.a }\ninclude cyclic",
        "include \"./cycle.stav\"\n#- file cycle.stav\na = self.b\nb = self.a",
    ] {
        let text = format!("struct Root {{ a: Int, b: Int }}\na = force(1)\n{part}\n");
        let fixture = fixture(&text);
        let checked = check(&fixture.host, &fixture.root).expect("a part body cycle only warns");
        let warnings: Vec<_> = checked.warnings().iter().collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].diagnostic.code, "W0607");
        assert_eq!(warnings[0].diagnostic.labels.len(), 2);
        assert_eq!(canonical(&text).unwrap(), r#"{"a":1,"b":1}"#);
    }
}

#[test]
fn decomposed_map_literals_reject_duplicate_computed_keys() {
    for value in ["self.n", "1"] {
        let text = format!(
            "struct Root {{ n: Int = 1, m: Map<Str, Int> }}\n\
             let key = \"a\"\n\
             m = {{ \"#{{key}}\" = {value}, a = {value} }}\n"
        );
        let error = canonical(&text).expect_err("entries of one literal must have unique keys");
        assert!(error.contains("E0605"), "{error}");
    }
    let text = "struct Root { n: Int = 1, m: Map<Str, List<Int>> }\n\
                let key = \"a\"\n\
                m = { \"#{key}\" = [self.n], a = [self.n] }\n";
    let error = canonical(text).expect_err("duplicate literal entries cannot concatenate");
    assert!(error.contains("E0605"), "{error}");
}

#[test]
fn fully_overridden_late_whole_defaults_do_not_add_instance_cycle_edges() {
    let text = "#! entries main.stav overlay.stav\n\
                #- file main.stav\n\
                struct Item { a: Int, b: Int }\n\
                struct Root { n: Int = 1, cfg: Item = make(self.n) }\n\
                fn make(n: Int) -> Item = Item { a = n, b = n }\n\
                n = self.cfg.a\n\
                #- file overlay.stav\ncfg.a = 7\ncfg.b = 9\n";
    assert_eq!(canonical(text).unwrap(), r#"{"cfg":{"a":7,"b":9},"n":7}"#);
}

#[test]
fn late_whole_conflicts_count_empty_present_markers() {
    let text = "struct Root { n: Int = 1, m: Map<Str, Int> }\n\
                fn make(n: Int) -> Map<Str, Int> = { a = n }\n\
                m = make(self.n)\nm = {}\n";
    assert_late_whole_error(text);
}

#[test]
fn late_whole_conflict_labels_do_not_repeat_present_value_spans() {
    let text = "struct Root { n: Int = 1, m: Map<Str, Int> }\n\
                fn make(n: Int) -> Map<Str, Int> = { a = n }\n\
                m = force(make(self.n))\nm = if true { { a = 2 } } else { {} }\n";
    let fixture = fixture(text);
    let checked = check(&fixture.host, &fixture.root).expect("the program checks");
    let diagnostics = checked
        .eval(&fixture.limits)
        .expect_err("the lower value competes");
    let diagnostic = &diagnostics.iter().next().unwrap().diagnostic;
    assert_eq!(diagnostic.code, "E0504");
    for label in &diagnostic.labels {
        if label.message.starts_with("present") {
            assert!(
                !diagnostic.labels.iter().any(|other| {
                    !other.message.starts_with("present")
                        && other.file == label.file
                        && other.span == label.span
                }),
                "{diagnostics}"
            );
        }
    }
}

#[test]
fn higher_map_and_partial_struct_literals_preserve_unmentioned_whole_fields() {
    let text = "struct Source { url: Str, sha256: Str = \"\" }\n\
                struct Root { n: Int = 1, m: Map<Str, Int>, source: Source = hosted(self.n) }\n\
                fn hosted(n: Int) -> Source = Source { url = \"u/#{n}\" }\n\
                fn make(n: Int) -> Map<Str, Int> = { a = n, b = n }\n\
                m = default(make(self.n))\nm = force({ a = 2 })\n\
                source = force({ sha256 = \"abc\" })\n";
    assert_eq!(
        canonical(text).unwrap(),
        r#"{"m":{"a":2,"b":1},"n":1,"source":{"sha256":"abc","url":"u/1"}}"#
    );
}

#[test]
fn declared_late_wholes_accept_every_explicit_priority_across_container_types() {
    for (ty, value, path, rhs, expected) in [
        (
            "Map<Str, Int>",
            "{ a = n, b = n }",
            "m.a",
            "2",
            r#"{"a":2,"b":1}"#,
        ),
        ("Item", "Item { a = n }", "m.a", "2", r#"{"a":2,"b":3}"#),
        ("Data", "{ a = n }", "m", "{ b = 2 }", r#"{"b":2}"#),
        (
            "Option<Map<Str, Int>>",
            "{ a = n, b = n }",
            "m.a",
            "2",
            r#"{"a":2,"b":1}"#,
        ),
        (
            "Option<Item>",
            "Item { a = n }",
            "m.a",
            "2",
            r#"{"a":2,"b":3}"#,
        ),
        ("Option<Data>", "{ a = n }", "m", "{ b = 2 }", r#"{"b":2}"#),
    ] {
        for other in [
            format!("default({rhs})"),
            rhs.to_owned(),
            format!("force({rhs})"),
        ] {
            let text = format!(
                "struct Item {{ a: Int = 0, b: Int = 3 }}\n\
                struct Root {{ n: Int = 1, m: {ty} = make(self.n) }}\n\
                fn make(n: Int) -> {ty} = {value}\n{path} = {other}\n"
            );
            let value: serde_json::Value =
                serde_json::from_str(&canonical(&text).expect(&text)).unwrap();
            assert_eq!(
                value["m"],
                serde_json::from_str::<serde_json::Value>(expected).unwrap(),
                "{text}"
            );
        }
    }
}

#[test]
fn higher_none_cannot_hide_static_equal_priority_whole_conflicts() {
    let text = "struct Root { n: Int = 1, m: Option<Map<Str, Int>> }\n\
                fn make(n: Int) -> Map<Str, Int> = { a = n }\n\
                m = make(self.n)\nm.a = 2\nm = force(none)\n";
    assert_late_whole_error(text);
}

#[test]
fn enclosing_absent_options_do_not_hide_static_whole_conflicts() {
    let text = "struct Box { m: Map<Str, Int> }\n\
                struct Root { n: Int = 1, cfg: Option<Box> }\n\
                fn make(n: Int) -> Map<Str, Int> = { a = n }\n\
                cfg.m = make(self.n)\ncfg.m.a = 2\ncfg = force(none)\n";
    assert_late_whole_error(text);
}

#[test]
fn nested_declared_defaults_do_not_add_losing_edges_before_wholes_settle() {
    let text = "struct Inner { a: Int = self.b, b: Int = 0 }\n\
                struct Outer { inner: Inner }\n\
                struct Root { seed: Int = 1, cfg: Outer }\n\
                fn make(n: Int) -> Outer = Outer { inner = Inner { a = n, b = n } }\n\
                cfg = default(make(self.seed))\n\
                cfg { inner { b = self.a } }\n";
    assert_eq!(
        canonical(text).unwrap(),
        r#"{"cfg":{"inner":{"a":1,"b":1}},"seed":1}"#
    );
}

#[test]
fn empty_struct_fields_do_not_vacuously_override_late_wholes() {
    let text = "struct Empty {}\nstruct Item { e: Empty }\n\
                struct Root { n: Int = 1, m: Item }\n\
                fn make(n: Int) -> Item = Item { e = Empty {} }\n\
                m = make(self.n)\n";
    assert_eq!(canonical(text).unwrap(), r#"{"m":{"e":{}},"n":1}"#);
}

#[test]
fn optional_none_at_equal_priority_reports_presence_conflicts() {
    for ty in ["Map<Str, Int>", "Item", "Data"] {
        let value = if ty == "Item" {
            "Item { a = n }"
        } else {
            "{ a = n }"
        };
        let text = format!(
            "struct Item {{ a: Int }}\n\
            struct Root {{ n: Int = 1, m: Option<{ty}> }}\n\
            fn make(n: Int) -> {ty} = {value}\n\
            m = make(self.n)\nm = none\n"
        );
        let error = canonical(&text).unwrap_err();
        assert!(error.contains("E0501"), "{error}");
        assert!(!error.contains("E0504"), "{error}");
    }
}

#[test]
fn higher_nested_wholes_keep_omitted_defaults_before_outer_wholes_settle() {
    let text = "struct Item { a: Int, b: Int = 3 }\n\
                struct Root { n: Int = 1, k: Int = 2, m: Map<Str, Map<Str, Item>> }\n\
                fn outer(n: Int) -> Map<Str, Map<Str, Item>> = { x = { old = Item { a = n } } }\n\
                fn inner(k: Int) -> Map<Str, Item> = { new = Item { a = k } }\n\
                m = default(outer(self.n))\nm.x = inner(self.k)\n";
    assert_eq!(
        canonical(text).unwrap(),
        r#"{"k":2,"m":{"x":{"new":{"a":2,"b":3},"old":{"a":1,"b":3}}},"n":1}"#
    );
}

#[test]
fn higher_nested_map_does_not_cover_a_failing_outer_whole() {
    let text = "struct Item { a: Int, b: Int = 3 }\n\
                struct Outer { inner: Map<Str, Item>, n: Int }\n\
                struct Root { zero: Int = 0, cfg: Outer = outer(self.zero) }\n\
                fn outer(n: Int) -> Outer = Outer { inner = {}, n = 1 / n }\n\
                fn inner(k: Int) -> Map<Str, Item> = { new = Item { a = k } }\n\
                cfg.inner = inner(self.zero)\ncfg.n = 7\n";
    assert!(canonical(text).unwrap_err().contains("E0603"));
}

#[test]
fn settling_outer_wholes_reaches_newly_installed_pending_defaults() {
    let text = "fn make_map(n: Int) -> Map<Str, Int> = { a = n }\n\
                struct Cfg { seed: Int, m: Map<Str, Int> = make_map(self.seed), result: Int }\n\
                struct Root { n: Int = 1, cfg: Cfg }\n\
                fn make_cfg(n: Int) -> Cfg = Cfg { seed = n, result = 0 }\n\
                cfg = default(make_cfg(self.n))\ncfg { result = self.m[\"a\"] }\n";
    assert_eq!(
        canonical(text).unwrap(),
        r#"{"cfg":{"m":{"a":1},"result":1,"seed":1},"n":1}"#
    );
}

#[test]
fn enclosing_pending_wholes_ignore_declared_default_competitors() {
    let text = "struct Box { m: Map<Str, Int> = {} }\n\
                struct Root { n: Int = 1, cfg: Box }\n\
                fn make_box(n: Int) -> Box = Box {}\n\
                fn make_map(n: Int) -> Map<Str, Int> = { a = n }\n\
                cfg = default(make_box(self.n))\ncfg.m = make_map(self.n)\n";
    assert_eq!(canonical(text).unwrap(), r#"{"cfg":{"m":{"a":1}},"n":1}"#);
}

#[test]
fn unused_defaults_never_execute() {
    for default in ["{}", "make(self.n)"] {
        let text = format!(
            "fn bad() -> Int = 1 / 0\n\
            struct Item {{ a: Int = bad() }}\n\
            struct Root {{ n: Int = 1, cfg: Item = {default} }}\n\
            fn make(n: Int) -> Item = Item {{ a = n }}\ncfg.a = 7\n"
        );
        assert_eq!(canonical(&text).unwrap(), r#"{"cfg":{"a":7},"n":1}"#);
    }
}

#[test]
fn late_whole_sums_reject_equal_or_lower_priority_selections() {
    for ty in ["Build", "Option<Build>"] {
        for flag in [false, true] {
            for whole in [
                "if self.flag { meson(options = []) } else { cargo() }",
                "force(if self.flag { meson(options = []) } else { cargo() })",
            ] {
                let text = format!(
                    "type Build = | cargo(features: List<Str> = []) | meson(options: List<Str> = [])\n\
                     struct Root {{ flag: Bool = {flag}, zero: Int = 0, build: {ty} }}\n\
                     build cargo {{ features = [\"#{{1 / self.zero}}\"] }}\n\
                     build = {whole}\n"
                );
                assert_late_whole_error(&text);
            }
        }
    }
}

#[test]
fn higher_priority_sum_selections_still_evaluate_late_wholes() {
    for ty in ["Build", "Option<Build>"] {
        for payload in ["", "features = [\"selected\"]"] {
            let text = format!(
                "type Build = | cargo(features: List<Str> = []) | meson(options: List<Str> = [])\n\
                 struct Root {{ zero: Int = 0, build: {ty} }}\n\
                 fn failing(n: Int) -> Build = meson(options = [\"#{{1 / n}}\"])\n\
                 build = default(failing(self.zero))\n\
                 build cargo {{ {payload} }}\n"
            );
            assert!(canonical(&text).unwrap_err().contains("E0603"));
        }
    }
}

#[test]
fn sum_blocks_still_evaluate_late_declared_wholes() {
    for ty in ["Build", "Option<Build>"] {
        let text = format!(
            "type Build = | cargo(features: List<Str> = []) | meson(options: List<Str> = [])\n\
             struct Root {{ zero: Int = 0, build: {ty} = failing(self.zero) }}\n\
             fn failing(n: Int) -> Build = meson(options = [\"#{{1 / n}}\"])\n\
             build cargo {{}}\n"
        );
        assert!(canonical(&text).unwrap_err().contains("E0603"));
    }
}

#[test]
fn selected_late_whole_sums_keep_dependency_edges() {
    let text = r#"
        type Build = | cargo(features: List<Str> = []) | meson(options: List<Str> = [])
        struct Root { flag: Bool = self.build == cargo(), build: Build }
        build = default(if self.flag { meson() } else { cargo() })
        build cargo {}
    "#;
    assert!(canonical(text).unwrap_err().contains("E0607"));
}

#[test]
fn late_whole_sums_exclude_nested_implicit_defaults_from_competitors() {
    let text = r#"
        type Inner = inner(x: Int)
        type Outer = outer(child: Inner = inner(x = 1))
        struct Root { zero: Int = 0, value: Outer }
        fn failing(n: Int) -> Outer = outer(child = inner(x = 1 / n))
        value = default(failing(self.zero))
        value outer { child inner {} }
    "#;
    assert!(canonical(text).unwrap_err().contains("E0603"));
}

#[test]
fn sum_selections_do_not_replace_enclosing_whole_structs() {
    let text = r#"
        type Choice = one(a: Int, b: Option<Int>)
        struct Box { seed: Int, choice: Choice = make_choice(self.seed) }
        struct Root { n: Int = 1, cfg: Box = make_box(self.n) }
        fn make_choice(n: Int) -> Choice = one(a = n, b = n)
        fn make_box(n: Int) -> Box = Box { seed = n, choice = one(a = n, b = n) }
        cfg { seed = 7, choice one { a = 9 } }
    "#;
    assert_eq!(
        canonical(text).expect("the enclosing whole supplies the omitted payload field"),
        r#"{"cfg":{"choice":{"one":{"a":9,"b":1}},"seed":7},"n":1}"#,
    );
}

#[test]
fn late_whole_sum_payload_fields_ignore_their_declared_defaults() {
    let value = canonical(
        r#"
            type Build = cargo(settings: Map<Str, Int> = {})
            struct Root { n: Int = 1, build: Build }
            fn make(n: Int) -> Map<Str, Int> = { a = n }
            build cargo { settings = make(self.n) }
        "#,
    )
    .unwrap();
    assert_eq!(value, r#"{"build":{"cargo":{"settings":{"a":1}}},"n":1}"#);
}

#[test]
fn optional_late_whole_sums_ignore_none_for_static_validation() {
    let error = canonical(
        r#"
            type Build = cargo
            struct Root { flag: Bool = false, build: Option<Build> }
            build = if self.flag { cargo } else { none }
            build = none
        "#,
    )
    .unwrap_err();
    assert!(error.contains("E0501"), "{error}");
    assert!(!error.contains("E0504"), "{error}");
}

#[test]
fn late_whole_sums_preserve_defaults_inside_selected_payload_structs() {
    let text = r#"
        struct Inner { a: Int = self.b, b: Int = 2 }
        type Build = cargo(inner: Inner)
        struct Root { zero: Int = 0, build: Build }
        fn failing(n: Int) -> Build = cargo(inner = Inner { b = 1 / n })
        build = default(failing(self.zero))
        build cargo { inner {} }
    "#;
    assert!(canonical(text).unwrap_err().contains("E0603"));
}

#[test]
fn suspended_sum_defaults_are_not_payload_competitors() {
    let error = canonical(
        r#"
            type Choice = one(settings: Map<Str, Int>)
            struct Box { choice: Choice = one(settings = {}) }
            struct Root { zero: Int = 0, cfg: Box }
            fn make_box(n: Int) -> Box = Box { choice = one(settings = {}) }
            fn failing(n: Int) -> Map<Str, Int> = { a = 1 / n }
            cfg = default(make_box(self.zero))
            cfg { choice one { settings = failing(self.zero) } }
        "#,
    )
    .unwrap_err();
    assert!(error.contains("E0603"), "{error}");
    assert!(!error.contains("E0504"), "{error}");
}

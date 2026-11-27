//! Checks the source locations of evaluation diagnostics through the public API.

use stav::{Host, ImportRoot, Limits, MemoryLoader, RootSpec, check};

#[test]
fn duplicate_computed_map_keys_point_at_the_second_key() {
    for value in ["1", "self.n"] {
        for expression in [
            format!("{{ a = {value}, \"#{{key}}\" = {value} }}"),
            format!("identity({{ a = {value}, \"#{{key}}\" = {value} }})"),
        ] {
            let text = format!(
                "struct Root {{ n: Int = 1, m: Map<Str, Int> }}\n\
                 fn identity(m: Map<Str, Int>) -> Map<Str, Int> = m\n\
                 let key = \"a\"\n\
                 m = {expression}\n"
            );
            let host = Host::new(MemoryLoader::new().with_file("app/main.stav", &text));
            let root = RootSpec {
                root_struct: "Root".to_owned(),
                entries: vec!["app/main.stav".into()],
                import_roots: vec![ImportRoot::new("app")],
            };
            let checked = check(&host, &root).expect("computed keys must pass checking");
            let diagnostics = checked
                .eval(&Limits::default())
                .expect_err("computed duplicate keys must fail evaluation");
            let errors: Vec<_> = diagnostics.iter().collect();
            assert_eq!(errors.len(), 1, "{text}:\n{diagnostics}");
            let error = &errors[0].diagnostic;
            assert_eq!(error.code, "E0605", "{text}");
            let start = text.find("\"#{key}\"").expect("the second key is present");
            assert_eq!(
                error.span.range(),
                start..start + "\"#{key}\"".len(),
                "{text}"
            );
        }
    }
}

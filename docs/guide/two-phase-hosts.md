# Two-phase hosts

Some hosts cannot type a root until they have evaluated part of it. A package manager may let a configuration set the options of its packages, while the types of those options come from the repositories that the same configuration lists. The host must read the repositories first, fetch their option declarations, and only then check the package options.

Stav serves such hosts with two features that work together. The host marks some fields **opaque**, so that whatever a root writes for them is parsed but not checked, and it evaluates only the **selected** attributes it needs. In the second phase it checks and evaluates the same sources again, normally, with the real types.

## Opaque fields

`RootSpec::opaque` lists the opaque fields of a root. A `FieldName` names a struct and one of its fields. The struct is found as the root struct is, among the structs of the prelude and the entry files. A name that names no such struct or field fails the check with [E0302](/reference/error-codes#e0302).

```rust
use stav::{FieldName, ImportRoot, RootSpec};

let mut root = RootSpec::new("Root", vec!["config/main.stav".into()], vec![ImportRoot::new("config")]);
root.opaque = vec![FieldName::new("System", "package")];
```

A field is opaque wherever its struct is used. The check accepts two kinds of code without checking it: what the root writes for an opaque field, and every definition that reads one.

What a root writes for an opaque field is its declared type, default and refinement, every attribute statement whose path goes through it, every block statement of its name, and its entry in a struct or map literal that the merge splits into fields and keys. In this configuration everything inside `package` stays unchecked, so `amd` and `mesa` need no declarations yet:

```stav
system {
  package.mesa.x11 = false
  package {
    mesa { vulkan = amd }
  }
  repo core { url = "https://example.org/core" }
}
```

A definition reads an opaque field when it accesses it, as `self.package` or `v.package` do, when it builds a value of a struct that holds one, or when it uses a binding whose value reads one. The smallest definition around such a read is then unchecked: the right-hand side of an attribute statement, an entry of a split literal, the value of a `let`, the body of a function with its parameter defaults, a default, or the head of an `if`, `for` or `include` statement or the interpolated label of a block. The check reports nothing inside it. Everything else is checked as usual, so code outside unchecked definitions that names a declaration only the second phase has fails with E0302.

Building a value counts as a read because no value with an unknown field ever exists. A struct or map literal that the merge splits, such as the right-hand side `{ package = { ... }, repos = { ... } }`, builds no value, so only its entries count.

## Selected evaluation

`Checked::eval_selected` evaluates the attributes at the given paths and what they need. Each path names a field at every step, through structs and `Option`s of structs. A map entry or a list element cannot be selected, and a path that names something else is [E0805](/reference/error-codes#e0805).

```rust
use std::collections::BTreeMap;

use stav::{AttrPath, HostValues, Limits};

let checked = stav::check(&host, &root)?;
let found = checked.eval_selected(
    &Limits::default(),
    &HostValues::new(),
    &[AttrPath::new(["system", "repos"])],
)?;
let repos: BTreeMap<String, Repo> = found.get(&AttrPath::new(["system", "repos"]))?;
```

Statements stay eager. Every statement of the root runs, in order, and so does every declared default that reads no `self` and may fill a field, because they decide which contributions exist. The merge decides winners, shapes and the presence of every `Option` for the whole root. Only then does evaluation restrict itself to the attributes that the selected ones need: through their contents, and through the `self` reads of the contributions that may supply them. It reports `self` cycles among those attributes only, evaluates only their deferred contributions, checks the required fields of the structs among them, and checks refinements on the selected values. A deferred contribution that nothing selected needs is never evaluated, so its errors are not reported. References are checked only when the root path is selected, so a selected `Ref` that names no block is returned as written.

The result holds the root struct with only the fields that lead to the selected attributes, and each selected attribute with its whole value. An `Option` on the way that is `none` is `none` in the value. `value_at`, `get`, `spans` and `priority` answer for paths at or under a selected attribute, and give E0805 or nothing elsewhere. `Checked::eval_with` is `eval_selected` of the root path.

## When an opaque field is needed

An evaluation stops with [E0619](/reference/error-codes#e0619) when it needs the value of an opaque field. That happens when a selected attribute needs an instance of the field or an unchecked contribution that wins or may supply it, when it needs a whole value of a struct that holds the field, and when it runs an unchecked function, default or statement. A statement runs in every evaluation, whatever is selected. A whole evaluation of a root with opaque fields therefore stops at the first instance of one.

The presence of an `Option` can depend on an unchecked contribution too, because a block inside it may hold statements of any priority. Such a presence stays undecided, without a conflict, and needing it stops with E0619.

The diagnostic names the selected attribute and the opaque field, points at the read or the contribution that needed it, and labels the bindings it went through. A host can map it to a code of its own when it has a rule to state.

## The second phase

The second phase checks the same sources with no opaque fields, and with the declarations that the opaque fields' types need, for example as a generated prelude file. When both phases succeed, each selected attribute has the same value in both, with the same spans and priorities. Every error of the first phase other than E0619, and E0302 for a declaration that only the second phase has, is one that the second phase reports as well.

`Checked::inputs` of the first phase lists every source that the second phase reads, apart from the sources the host adds for it, because loading does not depend on opaque fields. A host that wants both phases to see the same texts gives the second phase those texts through a `MemoryLoader`. A host that caches a selected evaluation keys it on the inputs, the host values, the opaque fields and the selected paths.

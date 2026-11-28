# Merging rules

A Stav program does not build its value statement by statement. Every statement contributes to attributes of the root struct, and once evaluation has finished, the contributions to each attribute are merged into one value. This page describes exactly how. The guide page on [structuring configuration](/guide/structuring) introduces the same ideas with a worked example.

## Contributions

Evaluating a body produces contributions. A contribution has four parts:

- a **path**, the attribute it contributes to, such as `servers.web.port`,
- a **priority**, one of the four levels below,
- a **value**, or a deferred value when the right side reads [`self`](/guide/self),
- an **order key**, which places it among all other contributions.

Every statement form produces contributions:

| Statement | Contributes |
|---|---|
| `a.b.c = e` | `e` to the attribute at `a.b.c` |
| `kind label { ... }` | the statements of the body, below the attribute the block maps to |
| `include e` | the statements of the part `e`, at the place of the `include`, unless the part is [already included](#a-part-merges-once-per-block) into the same block |
| `if c { ... } else { ... }` | the statements of the branch that `c` selects |
| `for x in e { ... }` | the statements of the body, once per element of `e` |

Conditions, `for` collections, block labels and `include` expressions are evaluated at once and cannot read `self`. Map-literal keys cannot read `self` either, including through interpolation, function arguments or late bindings. Map-literal values may read it. These restrictions keep contribution paths independent of merged values. [Expressions and statements](/reference/expressions) describes each statement in full.

A late whole value at a map, struct, payload sum or `Data` path, including an `Option` of one of these types, permits other contributions at or below its path only when they have strictly higher priority. Equal or lower priority contributions report [E0504](/reference/error-codes#e0504), citing both sources. Two kinds of contributions are excluded from this check: **every declared default**, including parts of a default literal belonging to a field above the path, and a `none` contribution at the `Option` path itself. Declared defaults are fallbacks, and option presence is decided separately. For these types, two statement whole values at the same path are E0504 regardless of their priorities: each is a statement contribution that must precede the other. A declared whole default beside a statement whole value is allowed instead. This validation runs before any deferred value is evaluated and never considers its output.

An enclosing default literal can therefore supply missing keys below a late whole map:

<!-- file: enclosing-default-map.stav -->

```stav
struct S { m: Map<Str, Int> }
struct Root { x: Int = 0, s: S = { m = { a = 1 } } }

fn mk(x: Int) -> Map<Str, Int> = {}

s.m = mk(self.x)
```

<!-- run: eval enclosing-default-map.stav -->

```json
{
  "x": 0,
  "s": {
    "m": {
      "a": 1
    }
  }
}
```

The declared `a = 1` belongs to the default of `s`, above the whole value's path `s.m`, and is still exempt from E0504. It fills the missing key. If `mk` returns `{ a = 2 }` instead, that higher whole value supplies `a = 2`.

A nullary sum, whose variants have no payload, is a scalar and is never subject to E0504, including inside an `Option`. A sum with even one payload variant remains subject to the rule, including when the contribution produces a nullary variant.

Struct and map literals at statically typed paths split into child contributions instead. A literal assigned to `Data` remains a whole value, so its syntax does not exempt it from the rule. [Late whole values](#late-whole-values) explains how higher contributions combine with a whole value and when it must run.

## Priorities

Each contribution has one of four priorities, from lowest to highest:

| Priority | Written as |
|---|---|
| declared default | the `= expr` of a struct field or a payload field |
| `default` | `attribute = default(expr)` |
| plain | `attribute = expr` |
| `force` | `attribute = force(expr)` |

At every scalar or collection leaf, only the contributions with the highest priority present take part in the merge. Structs merge per field and maps per key, so a higher contribution does not discard unmentioned fields or keys. Priorities are known before deferred values run, so a losing value that reads `self` is never evaluated and adds no dependency. An eager value still runs when its statement is evaluated, even if it later loses. The [`self` guide](/guide/self#how-it-fits-with-eager-evaluation) demonstrates both cases.

<!-- file: priorities.stav -->

```stav
struct Root {
  declared:  Int = 1
  defaulted: Int = 1
  plain:     Int = 1
  forced:    Int = 1
}

defaulted = default(2)

plain = default(2)
plain = 3

forced = 3
forced = force(4)
```

<!-- run: eval priorities.stav -->

```json
{
  "declared": 1,
  "defaulted": 2,
  "plain": 3,
  "forced": 4
}
```

`default(...)` and `force(...)` are priority markers, not functions. They are valid only as the whole right side of an attribute statement. Anywhere else they are [`E0441`](/reference/error-codes#e0441):

<!-- file: marker.stav -->

```stav
struct Root { port: Int = 80 }

port = default(8080) + 1
```

<!-- run: check marker.stav -->

```text
error[E0441]: `default` is a priority marker, valid only as the whole right side of an attribute
  ┌─ marker.stav:3:8
  │
3 │ port = default(8080) + 1
  │        ^^^^^^^^^^^^^
  │
  = help: write `attribute = default(...)`
```

A call of the bare name `default` or `force` is always the marker, so no declaration can change how a file merges. The name `exact` is reserved the same way for a marker that is not available yet, so a call `exact(...)` is [`E0302`](/reference/error-codes#e0302). A top-level function or a payload variant with one of these names is [`E0301`](/reference/error-codes#e0301). A `let`, a parameter or an import with one of these names is allowed and stays usable as a value, but calling the bare name while it is in scope is [`E0307`](/reference/error-codes#e0307), because the call would not mean the binding:

<!-- file: marker-binding.stav -->

```stav
struct Root { port: Int = 80 }

let default = fn(p: Int) = p + 1

port = default(8080)
```

<!-- run: check marker-binding.stav -->

```text
error[E0307]: `default` here is the priority marker, but a binding `default` is in scope
  ┌─ marker-binding.stav:5:8
  │
5 │ port = default(8080)
  │        ^^^^^^^
  │
  = help: rename the binding
```

A nullary variant named `default`, as in `type Profile = default | custom`, stays a variant, because only a call is a marker.

A marker around a struct or map literal applies to every contribution the literal makes. In the next program, `force` wins the field `a`, and the plain contribution still sets `b`, because nothing else contributes to `b` at a higher priority.

<!-- file: marker-literal.stav -->

```stav
struct Settings {
  a: Int = 1
  b: Int = 2
}

struct Root { settings: Settings = {} }

settings = force({ a = 5 })
settings = { a = 3, b = 4 }
```

<!-- run: eval marker-literal.stav -->

```json
{
  "settings": {
    "a": 5,
    "b": 4
  }
}
```

## Declared defaults

A declared default that fills a merged struct or tagged-block payload field is evaluated only when it may fill a field, and at most once per field it fills. Whether it may is decided statically from contributions, before any deferred value runs. This applies even without `self` and outside late whole values. A default that loses to another contribution at its own path never runs, so it cannot fail:

<!-- file: declared-losers.stav -->

```stav
struct S { x: Int = 1 / 0, y: Int = 0 }
struct Root { x: Int = 1 / 0, m: Map<Str, S> = {} }

x     = 7
m.b.x = 7
```

<!-- run: eval declared-losers.stav -->

```json
{
  "x": 7,
  "m": {
    "b": {
      "x": 7,
      "y": 0
    }
  }
}
```

Defaults without `self` reuse their first result across instances within one evaluation. Defaults that read `self` use the fields of their own instance. A struct or map literal written as a default splits recursively into field and literal-key contributions, so a covered child can remain unevaluated while a sibling still fills its field. Computed map keys remain unknown during cycle checking and are evaluated only when their default participates. The `self` reads written in each computed-key entry's value belong to the map field itself, not to a child path. Struct instances created by the entry get placeholder paths with their own literal-field and declared-default dependencies, recursively, whenever the literal's edges participate. [Computed keys and cycles](/guide/self#computed-keys-in-declared-defaults) explains this conservative graph. Other expressions remain whole. A participating variant constructor call eagerly builds a complete plain value, evaluating both explicit arguments and the declared defaults of omitted payload fields like function parameter defaults. Higher contributions to its payload fields do not suppress this work.

A whole declared default of a struct, a sum with a payload variant, an `Option`, a map or `Data` runs unless higher contributions [cover its path](#late-whole-values), whether or not it reads `self`. A higher tagged-block selection does not cover a payload sum: the default may supply the matching payload's omitted fields. Its constructor therefore runs even if it produces a different variant whose payload is discarded, and it may fail:

<!-- file: declared-sum-error.stav -->

```stav
type Build = cargo(n: Int = 1, kept: Int = 4) | meson(o: Int = 0)

struct Root { build: Build = meson(o = 1 / 0) }

build cargo { n = 7 }
```

<!-- run: eval declared-sum-error.stav -->

```text
error[E0603]: division by zero
  ┌─ declared-sum-error.stav:3:42
  │
3 │ struct Root { build: Build = meson(o = 1 / 0) }
  │                                          ^
```

This reports E0603 from the default's constructor, despite the higher selection of `cargo`. A matching default supplies the payload field that the block leaves out:

<!-- file: declared-sum-payload.stav -->

```stav
type Build = cargo(n: Int = 1, kept: Int = 4) | meson(o: Int = 0)

struct Root { build: Build = cargo(kept = 5) }

build cargo { n = 7 }
```

<!-- run: eval declared-sum-payload.stav -->

```json
{
  "build": {
    "cargo": {
      "n": 7,
      "kept": 5
    }
  }
}
```

The result is `cargo` with `n = 7` and `kept = 5`. A higher whole attribute contribution of the sum covers the path instead and suppresses the lower default.

The unit of evaluation is the contribution, not each field of its result. A variant constructor call is a call, not a literal. If an omitted payload default fails, the constructor fails even when a higher contribution replaces that field:

<!-- file: declared-payload-error.stav -->

```stav
type Build = cargo(n: Int = 1 / 0, kept: Int = 4) | meson(o: Int = 0)
struct Root { build: Build = cargo(kept = 5) }
build cargo { n = 7 }
```

<!-- run: eval declared-payload-error.stav -->

```text
error[E0603]: division by zero
  ┌─ declared-payload-error.stav:1:31
  │
1 │ type Build = cargo(n: Int = 1 / 0, kept: Int = 4) | meson(o: Int = 0)
  │                               ^
```

Calling a function that returns the variant does not change this behavior. The constructor still builds its complete value before the function returns:

<!-- file: declared-payload-function-error.stav -->

```stav
type Build = cargo(n: Int = 1 / 0, kept: Int = 4) | meson(o: Int = 0)
fn mk() -> Build = cargo(kept = 5)
struct Root { build: Build = mk() }
build cargo { n = 7 }
```

<!-- run: eval declared-payload-function-error.stav -->

```text
error[E0603]: division by zero
  ┌─ declared-payload-function-error.stav:1:31
  │
1 │ type Build = cargo(n: Int = 1 / 0, kept: Int = 4) | meson(o: Int = 0)
  │                               ^
2 │ fn mk() -> Build = cargo(kept = 5)
3 │ struct Root { build: Build = mk() }
  │                              ---- in this call of `mk`
```

In contrast, a struct literal written as a default splits into field contributions. The higher `s.a` suppresses the failing declared default, while the literal supplies `b = 5`:

<!-- file: declared-struct-literal.stav -->

```stav
struct S { a: Int = 1 / 0, b: Int = 0 }
struct Root { s: S = { b = 5 } }
s.a = 7
```

<!-- run: eval declared-struct-literal.stav -->

```json
{
  "s": {
    "a": 7,
    "b": 5
  }
}
```

An attribute statement such as `x = default(1 / 0)` is not a declared default. It still runs eagerly and fails even when a later `x = 7` wins. The [`self` guide](/guide/self#how-it-fits-with-eager-evaluation) contrasts eager statements with deferred values.

## How each type merges

The merge walks the root struct from the top and merges every attribute by its type.

| Type | Rule |
|---|---|
| `Bool`, `Int`, `Float`, `Str`, `Path`, nullary sum types, symbol types, `Ref<T>` | The winners must be equal. Different values are [`E0501`](/reference/error-codes#e0501). |
| sum type with payloads, set as a whole value | Like a scalar. |
| sum type with payloads, set by tagged blocks | The variant is selected like a scalar, then the payload merges field by field. See [tagged blocks](#tagged-blocks). |
| `List<T>` | The winners are concatenated in order. |
| `Set<T>` | The union of the winners, in canonical order. |
| `Map<K, V>` | Key by key. Each key merges as a `V`. |
| struct | Field by field. Each field merges by its own type. |
| `Option<T>` | Present when anything sets it or something inside it, then merged as a `T`. See [options](#options). |
| `Data` | By the shape of the winning values. See [`Data`](#data). |

### Scalars

Two plain contributions with the same value are accepted. Two plain contributions with different values conflict, and the diagnostic cites every winner with its priority. When one of them comes from a part file, it also says where that file was included.

<!-- file: conflict.stav -->

```stav
struct Root { hostname: Str }

hostname = "forge"
include "./laptop.stav"
```

<!-- file: laptop.stav -->

```stav
hostname = "laptop"
```

<!-- run: eval conflict.stav -->

```text
error[E0501]: conflicting values for `hostname`
  ┌─ conflict.stav:3:12
  │
3 │ hostname = "forge"
  │            ^^^^^^^
  │            │
  │            "forge" (plain)
  │
  ┌─ laptop.stav:1:12
  │
1 │ hostname = "laptop"
  │            -------- "laptop" (plain), included from conflict.stav:4:1
  │
  = help: keep one of them, or mark the one that should win with `force(...)`
```

To resolve it, remove one of the contributions, mark the one that should win with `force(...)`, or mark the one that should give way with `default(...)`.

Nullary sums keep their scalar rules even when a winner is deferred:

<!-- file: nullary-late.stav -->

```stav
type Mode = fast | slow
struct Root { x: Int = 0, mode: Mode, o: Option<Mode> }

fn pick(x: Int) -> Mode = fast
fn opt(x: Int) -> Option<Mode> = fast

mode = pick(self.x)
mode fast { }
o = opt(self.x)
o fast { }
```

<!-- run: eval nullary-late.stav -->

```json
{
  "x": 0,
  "mode": "fast",
  "o": "fast"
}
```

Both deferred functions run. Returning `slow` would give E0501 rather than E0504. Returning `none` from `opt` also conflicts with the present `fast` winner. A higher contribution instead suppresses the losing deferred expression and its `self` edges, as for any scalar.

### Lists and sets

Lists concatenate and sets take the union. Because only the highest priority takes part, a single `force(...)` replaces a list, while two plain contributions add up.

<!-- file: collections.stav -->

```stav
struct Root {
  tags:     List<Str> = ["declared"]
  replaced: List<Str> = []
  zones:    Set<Str> = []
}

tags = ["a"]
tags = ["b"]

replaced = ["p1"]
replaced = force(["f"])
replaced = ["p2"]

zones = ["west", "east"]
zones = ["east", "north"]
```

<!-- run: eval -o toml collections.stav -->

```toml
tags = ["a", "b"]
replaced = ["f"]
zones = ["east", "north", "west"]
```

The declared default `["declared"]` of `tags` is not part of the result, because the two plain contributions outrank it.

### Structs, maps and literals

Structs merge field by field and maps merge key by key. A struct or map literal on the right side of an attribute contributes each field or key it sets, at the priority of the statement. This splitting is recursive, including when a literal reads `self` or appears in a declared field default. Each statically known child records only its own reads. In declared defaults, a computed-key entry instead records the `self` reads written in its value at the map field. Struct instances the entry creates also get placeholder paths with their own dependencies, recursively, without evaluating the key. Only participating children of a declared default run. In a statement literal, only winning deferred children run, while eager children run before merging. A lower priority literal can therefore lose one child and still supply its siblings. Fields it leaves out contribute nothing. That is why a literal, an attribute path and a block can all add to the same value:

<!-- file: literals.stav -->

```stav
struct Kernel {
  modules: List<Str> = []
  params:  Map<Str, Str> = {}
  quiet:   Bool = false
}

struct Server {
  name: Str
  port: Int = 80
}

struct Root {
  kernel:  Kernel = {}
  limits:  Map<Str, Int> = {}
  servers: List<Server> = []
}

kernel         = { modules = ["virt-host"] }
kernel.modules = ["pci-passthru"]
kernel { params { console = "ttyS0" } }

limits       = { files = 1024 }
limits.procs = 64

servers = [Server { name = "a" }]
```

<!-- run: eval -o toml literals.stav -->

```toml
[kernel]
modules = ["virt-host", "pci-passthru"]
quiet = false

[kernel.params]
console = "ttyS0"

[limits]
files = 1024
procs = 64

[[servers]]
name = "a"
port = 80
```

Inside a list, a struct literal is a complete value. Its omitted fields take their declared defaults right away, as `port` does in `servers` above. These defaults read the constructed value's own fields through `self`, never an enclosing block. Each omitted default is computed at most once per construction, when first read. Reading a default that is still running stops evaluation with [E0607](/reference/error-codes#e0607), citing the struct. All fields are complete before the value is returned. These are not merge-instance dependency edges. The [self guide](/guide/self#defaults-in-constructed-values) shows the distinction.

An eager struct value preserves the record of fields explicitly given when it was constructed, through bindings, parameters and function results. Assigned to an attribute, it contributes only those fields, recursively. Fields filled by defaults contribute nothing and can take their defaults again after merging. A late whole value instead treats every field as given, including fields computed from defaults inside its constructor. The [`self` guide](/guide/self#late-and-eager-struct-values) compares the two.

### Late whole values

A late whole value **has** the paths that exist in its value. A struct is complete and has every field, a map has only its keys, `none` has nothing below its path, and a sum has only its own variant's payload. A whole value **supplies** a path it has only when no higher contribution replaces it there or between its own path and that path. At every depth below a late whole value, fields come from these sources in priority order:

1. Higher statement contributions at that path.
2. Whole values that supply the path, with the higher one first.
3. Declared defaults, but only in an instance that no whole value supplies.

A higher contribution can create a map entry absent from every whole value, an option value where the whole values return `none`, or a selected variant's payload when none of the whole values has that variant. These instances take their fields from the higher contributions and then from declared defaults, recursively through struct fields. The same rule applies inside a whole struct, not just at the whole value's own path. A default runs only when it may fill a field, as decided statically from contributions. It can run without supplying a final field if its whole value is not covered. A field in an instance that a whole value supplies keeps the whole value's field instead, even if its constructor computed that field from a default.

This example creates a missing map entry inside a whole struct:

<!-- file: late-missing-entry.stav -->

```stav
struct Download {
  version: Str
  url:     Str = "https://example.org/tool-#{self.version}.tar.gz"
}

struct Box { downloads: Map<Str, Download> }

struct Root { key: Str = "tool", cfg: Box }

fn make(key: Str) -> Box = Box { downloads = {} }

cfg = make(self.key)
cfg.downloads.tool.version = force("2")
```

<!-- run: eval late-missing-entry.stav -->

```json
{
  "key": "tool",
  "cfg": {
    "downloads": {
      "tool": {
        "version": "2",
        "url": "https://example.org/tool-2.tar.gz"
      }
    }
  }
}
```

`make` supplies `cfg`, but not `cfg.downloads.tool`. The higher contribution creates that entry, so its `url` falls back to the declared default using version `2`. If `make` supplied `tool`, its computed `url` would survive instead. If the higher contribution set only `url` and `make` omitted `tool`, the required `version` would be missing ([E0801](/reference/error-codes#e0801)). Whether a created instance lacks a required field is the one check here that depends on the whole value's result.

The same fallback works for an option nested inside a whole struct:

<!-- file: late-missing-option.stav -->

```stav
struct Download {
  version: Str
  url:     Str = "https://example.org/tool-#{self.version}.tar.gz"
}

struct Box { download: Option<Download> }

struct Root { key: Str = "tool", cfg: Box }

fn make(key: Str) -> Box = Box { download = none }

cfg                  = make(self.key)
cfg.download.version = force("2")
```

<!-- run: eval late-missing-option.stav -->

```json
{
  "key": "tool",
  "cfg": {
    "download": {
      "version": "2",
      "url": "https://example.org/tool-2.tar.gz"
    }
  }
}
```

Here `cfg.download` is present and its default `url` uses version `2`. Returning a present download with `url = "u"` instead would preserve `"u"`.

A higher late whole struct, sum, `Data` or `Option` replaces lower whole values throughout its path, even when it returns `none` or a different variant. A higher whole map replaces lower values only beneath its own keys. Several late whole values can therefore supply the same map. For example, `s = default(a(self.x))` and `s.m = b(self.y)` merge the maps at `s.m`: `b` wins shared keys, and keys only in `a` survive. A higher whole struct is complete, so a lower whole value supplies nothing inside that struct.

A late whole value runs unless higher contributions **cover** its own path. The same coverage rules apply to whole declared defaults, whether or not they read `self`. Coverage is decided statically:

| Path type | What covers it |
|---|---|
| Scalar (including a nullary sum), list, set or `Data` | Any higher contribution at the path itself |
| Sum with a payload variant | A higher attribute contribution or late whole sum, never a tagged-block selection |
| `Option<T>` | A higher `none`, a higher late whole `Option`, or coverage as `T` |
| Struct | A higher late whole struct, or coverage of every field recursively |
| Map | Nothing, because lower whole maps may have keys that higher contributions omit |

A covered whole is never evaluated and adds no dependency edges. Every other whole that takes part runs even when a contribution at its own node has a higher priority. In particular, a declared late map default and a higher statement whole map both run and both add their `self` edges. A higher late `Option` replaces the lower one without running it, including when the higher one returns `none`.

<!-- file: late-option-replacement.stav -->

```stav
struct Download {
  version: Str
  url:     Str = "https://example.org/tool-#{self.version}.tar.gz"
}

struct Box { download: Option<Download>, n: Int = 0 }
struct Root { key: Str = "x", cfg: Box }

fn lower(key: Str) -> Box = Box { download = Download { version = "1", url = "lower" }, n = 3 }
fn higher(key: Str) -> Option<Download> = none

cfg                  = default(lower(self.key))
cfg.download         = higher(self.key)
cfg.download.version = force("2")
```

<!-- run: eval late-option-replacement.stav -->

```json
{
  "key": "x",
  "cfg": {
    "download": {
      "version": "2",
      "url": "https://example.org/tool-2.tar.gz"
    },
    "n": 3
  }
}
```

The higher whole option replaces the lower download even though it is `none`. The forced version creates a new instance, whose `url` comes from its declared default rather than the lower value. The lower whole still supplies `n`.

The dependency graph includes the `self` edges of declared defaults that might fill a higher-created instance, before any whole value runs. These edges are included even if a whole value later supplies the instance and the defaults are unused. This applies at every depth, recursively through struct fields and struct-typed payload fields. In both examples above, `url` depends on the created download's `version` regardless of what `make` returns. This conservative graph can therefore reject a cycle involving a default that would not run. When a late whole value has a struct as its own type, it guarantees that instance and every instance reached through struct-typed fields. Their declared defaults of every kind never take part, never run and add no edges, including late whole map defaults or defaults that would otherwise win at a node. This is decided from paths and types only. A whole value of an `Option`, map or sum does not guarantee a struct instance. The guarantee also stops at any option, map entry or sum variant on the path. [Cycles](/guide/self#cycles) explains the instance graph.

Declared defaults cannot introduce a cycle or a failing fallback into this supplied struct:

<!-- file: late-suppressed-defaults.stav -->

```stav
struct S { w: Int = g(self.m), m: Map<Str, Int> = d(self.w) }
struct Root { x: Int = 0, y: Int = 0, s: S }

fn g(m: Map<Str, Int>) -> Int = 1 / 0
fn d(w: Int) -> Map<Str, Int> = { q = 1 / w }
fn a(x: Int) -> S = S { w = 7, m = { lower = 1, shared = 1 } }
fn b(y: Int) -> Map<Str, Int> = { higher = 2, shared = 2 }

s   = default(a(self.x))
s.m = b(self.y)
```

<!-- run: eval late-suppressed-defaults.stav -->

```json
{
  "x": 0,
  "y": 0,
  "s": {
    "w": 7,
    "m": {
      "higher": 2,
      "lower": 1,
      "shared": 2
    }
  }
}
```

The whole struct supplies `w`. Both whole values contribute map keys, but neither declared default of `S` takes part. Neither `g` nor `d` runs, and the graph has no default edges between `s.w` and `s.m`.

### Options

An `Option<T>` attribute is present when a contribution sets it or anything inside it, and that contribution outranks every `none`. A present option then merges as a `T`, with declared defaults filling fields of instances that no whole value supplies. An option that nothing sets is `none`, and so is an option whose highest contribution is `none`. A late whole value counts as present when presence is decided, before its result is known. If it evaluates to `none`, only higher contributions below the path remain. Without any, the result is `none`. Otherwise they merge as `T`, with declared defaults filling fields of instances that no whole value supplies, as described under [late whole values](#late-whole-values).

Optional scalars, lists and sets can have several deferred winners at the same priority. Evaluate those winners before merging their results: `none` beside a present value is E0501, all `none` results give `none`, and all present results merge as the inner type. For example, a deferred `["b"]` followed by a literal `["a"]` gives `["b", "a"]` for `Option<List<Str>>`, but a deferred `none` beside that literal conflicts. A literal `none` beside a deferred winner of the same priority is E0501 before evaluation, so a failing deferred expression does not run.

<!-- file: options.stav -->

```stav
struct Tls {
  cert: Str = "/etc/cert.pem"
  port: Int = 443
}

struct Root {
  tls:     Option<Tls>
  plain:   Option<Tls>
  dropped: Option<Tls> = {}
}

tls { port = 8443 }
dropped = none
```

<!-- run: eval options.stav -->

```json
{
  "tls": {
    "cert": "/etc/cert.pem",
    "port": 8443
  },
  "plain": null,
  "dropped": null
}
```

A `none` is a value like any other. At equal priority it conflicts with a contribution that makes the option present:

<!-- file: option-conflict.stav -->

```stav
struct Tls { port: Int = 443 }

struct Root { tls: Option<Tls> }

tls = none
tls { port = 8443 }
```

<!-- run: eval option-conflict.stav -->

```text
error[E0501]: conflicting values for `tls`
  ┌─ option-conflict.stav:5:7
  │
5 │ tls = none
  │       ^^^^
  │       │
  │       none (plain)
6 │ tls { port = 8443 }
  │ -------------------
  │ │            │
  │ │            8443 (plain)
  │ present (plain)
  │
  = help: keep one of them, or mark the one that should win with `force(...)`
```

Writing `tls = default(none)` instead lets the block win.

### Tagged blocks

A field whose type is a sum type can be set with a tagged block, such as `build native { features = ["simd"] }`. The label names the variant, and the body sets its payload fields. When every variant is nullary, a tagged block contributes only its variant at plain priority. It merges as a scalar, so different winners report [E0501](/reference/error-codes#e0501), whether they come from attributes, deferred expressions or other tagged blocks.

For a sum with at least one payload variant, a tagged block makes two kinds of contributions:

- a **selection** of the variant, at plain priority,
- one contribution per payload attribute, each at its own priority.

The selections decide the variant like a scalar. The selections with the highest priority must agree, otherwise the merge reports [`E0502`](/reference/error-codes#e0502). An attribute statement such as `build = force(script(target = "x"))` selects at its own priority and also contributes its payload. Payload contributions that belong to a variant that was not selected are dropped. The remaining payload contributions merge field by field, like a struct.

<!-- file: tagged.stav -->

```stav
type Build =
  | native(features: List<Str> = [], bins: List<Str> = [])
  | script(target: Str = "all")

struct Job { build: Build }

struct Root { jobs: Map<Str, Job> = {} block job }

job a { build native { features = ["a"] } }
job a { build native { features = ["b"], bins = ["finder"] } }

job b {
  build native { features = ["dropped"] }
  build script { }
  build = force(script(target = "x"))
}
```

<!-- run: eval -o toml tagged.stav -->

```toml
[jobs.a.build.native]
features = ["a", "b"]
bins = ["finder"]

[jobs.b.build.script]
target = "x"
```

The two blocks for `a` merge their `features`. In `b`, the forced selection of `script` discards the `native` block together with its payload. Without the `force`, the two blocks would disagree:

<!-- file: tagged-conflict.stav -->

```stav
type Build =
  | native(features: List<Str> = [])
  | script(target: Str = "all")

struct Job { build: Build }

struct Root { jobs: Map<Str, Job> = {} block job }

job c {
  build native { }
  build script { }
}
```

<!-- run: eval tagged-conflict.stav -->

```text
error[E0502]: tagged blocks select different variants of `jobs.c.build`
   ┌─ tagged-conflict.stav:10:3
   │
10 │   build native { }
   │   ^^^^^^^^^^^^^^^^
   │   │
   │   `native` (plain)
11 │   build script { }
   │   ---------------- `script` (plain)
   │
   = help: keep the blocks of one variant, or select the variant with `force(...)`
```

A payload sum set only through whole values, such as `build = script(target = "x")`, merges like a scalar.

A late whole sum with a payload variant also obeys [E0504](/reference/error-codes#e0504). Every other competing static contribution at or below its path must have strictly higher priority. Declared defaults remain excluded from that check. In particular, a plain tagged-block selection cannot coexist with a plain or forced late whole value, even if both would select the same variant. This is checked before any deferred expression runs and does not depend on the variant produced.

A higher selection of a nullary sum covers the path like any other higher scalar contribution.

For a sum with a payload variant, a strictly higher tagged-block selection does not suppress a late whole value. If the whole value returns the selected variant, its payload fills fields that the higher contributions leave. If it returns a different variant, that payload is discarded, and the selected payload takes its fallbacks from declared defaults. This applies at the whole value's own path and at sum fields nested below it:

<!-- file: late-tagged-default.stav -->

```stav
type Build =
  | native(features: List<Str> = [], bins: List<Str> = [])
  | script(target: Str = "all")

struct Job { build: Build }

struct Root {
  flag:      Bool = true
  same:      Job
  different: Job
}

fn make(flag: Bool) -> Job = Job { build = if flag { native(bins = ["tool"]) } else { script() } }

same      = default(make(self.flag))
different = default(make(!self.flag))

same { build native { features = ["simd"] } }
different { build native { features = ["simd"] } }
```

<!-- run: eval late-tagged-default.stav -->

```json
{
  "flag": true,
  "same": {
    "build": {
      "native": {
        "features": [
          "simd"
        ],
        "bins": [
          "tool"
        ]
      }
    }
  },
  "different": {
    "build": {
      "native": {
        "features": [
          "simd"
        ],
        "bins": []
      }
    }
  }
}
```

Both calls to `make` run. `same.build` keeps `bins = ["tool"]` from the matching variant. `different.build` uses the declared default `bins = []`, because the whole value supplies only a `script` payload there. A higher whole attribute such as `same.build = force(script())` replaces that sum completely instead, so the lower whole value need not run if it supplies nothing else. The distinction also applies through `Option<Build>`.

### Data

Unlike statically typed structs and maps, `Data` keeps whole-value priorities. A literal assigned to `Data` is not split into independently prioritized child contributions. If the whole deferred value loses, none of it runs.

`Data` holds any renderable value, so its merge follows the shape of the winning values at run time. Maps merge key by key, lists concatenate, sets take the union, and anything else merges as a scalar. Map keys come out in canonical order.

<!-- file: data.stav -->

```stav
struct Root {
  settings: Data = none
  replaced: Data = none
}

settings = { port = 80, hosts = ["a"], tls = { enabled = true } }
settings = { hosts = ["b"], tls = { cert = "/etc/cert.pem" } }
settings = { port = 80 }

replaced = { kind = "map" }
replaced = force(["a", "list"])
```

<!-- run: eval data.stav -->

```json
{
  "settings": {
    "hosts": [
      "a",
      "b"
    ],
    "port": 80,
    "tls": {
      "cert": "/etc/cert.pem",
      "enabled": true
    }
  },
  "replaced": [
    "a",
    "list"
  ]
}
```

Winners of different shapes at equal priority are [`E0503`](/reference/error-codes#e0503). A higher priority contribution replaces lower ones whatever their shape, as `replaced` shows, so `force(...)` or `default(...)` resolves every such conflict.

<!-- file: data-conflict.stav -->

```stav
struct Root { settings: Data = none }

settings = { port = 80 }
settings = ["a", "list"]
```

<!-- run: eval data-conflict.stav -->

```text
error[E0503]: `Data` values of different shapes for `settings`
  ┌─ data-conflict.stav:3:12
  │
3 │ settings = { port = 80 }
  │            ^^^^^^^^^^^^^
  │            │
  │            a map (plain)
4 │ settings = ["a", "list"]
  │            ------------- a list (plain)
  │
  = help: a higher priority contribution replaces the others: mark one with `force(...)`, or the others with `default(...)`
```

## Order

Every contribution carries an order key, a sequence of numbers compared from left to right:

1. Each entry file gets the next number, in the order the host lists them. A host's implicit parts come before every entry file.
2. The n-th statement of a body appends n.
3. The contributions of an `include` are numbered below the `include` statement, in the part's own statement order. A part that is [merged once](#a-part-merges-once-per-block) keeps the place of its first `include`.
4. The i-th iteration of a `for` statement appends i.

List concatenation follows the order keys, and so does the order of diagnostics. Nothing depends on hashing or memory addresses, so the same program always gives the same value.

<!-- file: order.stav -->

```stav
struct Root { steps: List<Str> = [] }

steps = ["first"]
include "./extra.stav"
for name in ["loop-1", "loop-2"] {
  steps = [name]
}
steps = ["last"]
```

<!-- file: extra.stav -->

```stav
steps = ["included-1"]
steps = ["included-2"]
```

<!-- run: eval -o toml order.stav -->

```toml
steps = ["first", "included-1", "included-2", "loop-1", "loop-2", "last"]
```

The `stav` command evaluates a single entry file, so the examples on this page spread contributions over files with part files and `include`. A program that embeds Stav can list several entry files in the `entries` field of `RootSpec`, and they merge in the order given. See [embedding in Rust](/guide/embedding).

## A part merges once per block

An `include` adds the contributions of a part only if the same part has not been included into the same block before. A later `include` of that part into that block adds nothing. The rule applies to everything that is evaluated for one root: the host's implicit parts, every entry file, and every part and part file included from them, however deeply.

Two parts are the same part when

- both are the same part file, or
- both come from the same `part { }` expression, evaluated with equal values for every local binding in scope at it: the parameters of the function around it, `let` bindings, loop variables, pattern bindings and the bindings a lambda captured.

Evaluation is pure, so this is what makes a call of the same function with equal arguments give the same part, whether an argument is written out or left to its default. Values compare as `==` compares them. Functions, which `==` cannot compare, are equal when they are the same top-level function, or come from the same lambda expression with equal captured bindings. Parts compare by the rule above. A part bound once with `let` and included twice is the same part both times.

The block of an `include` is the attribute its statement contributes to: the root for a statement at the top level of an entry file, and the attribute of the enclosing block otherwise. An `include` inside a part counts at the block where the part itself is included, so a part that two other parts include is merged once when both land in the same block. Included into two different blocks, such as two named blocks with different labels, a part contributes to each of them.

<!-- file: once.stav -->

```stav
struct Service { after: List<Str> = [] }

struct Root {
  ports:    List<Int> = []
  services: Map<Str, Service> = {} block service
}

fn remote(port: Int = 22) -> Part<Root> = part {
  ports = [port]
}

fn after(name: Str) -> Part<Service> = part {
  after = [name]
}

include remote()
include remote(port = 22)
include remote(port = 2222)

service web {
  include after("network")
  include after("network")
  include after("database")
}

service worker {
  include after("network")
}
```

<!-- run: eval -o toml once.stav -->

```toml
ports = [22, 2222]

[services.web]
after = ["network", "database"]

[services.worker]
after = ["network"]
```

`remote()` and `remote(port = 22)` are the same part, `remote(port = 2222)` is another one. `after("network")` is merged once into `service web` and once into `service worker`.

Including the same part file twice in a row into the same body is warning [W0703](/reference/error-codes#w0703), because the second `include` has no effect.

## Writing shared parts

A part that other configurations include, such as a function that returns `Part<S>` or a library of part files, should follow one rule so that its users can override it without `force`:

- Set every scalar with `default(...)`. A plain assignment by the user then wins.
- Contribute to lists, sets and maps at plain priority, never with `default(...)`.

The second rule matters because only the highest priority takes part in a merge. A `default(...)` contribution to a list is discarded as soon as anyone makes a plain contribution to the same list, so the part's entries would silently disappear:

<!-- file: shared.stav -->

```stav
struct Server {
  replicas: Int = 1
  env:      Map<Str, Str> = {}
  flags:    List<Str> = []
}

struct Root { server: Server }

fn web(replicas: Int) -> Part<Server> = part {
  replicas = default(replicas)
  flags    = ["--web"]
  env { LOG = "info" }
}

server {
  include web(2)
  replicas = 8
  flags    = ["--verbose"]
}
```

<!-- run: eval -o toml shared.stav -->

```toml
[server]
replicas = 8
flags = ["--web", "--verbose"]

[server.env]
LOG = "info"
```

The user's `replicas = 8` beats the part's `default(2)`, and both lists of flags are kept.

## Late values

A right side that reads `self`, directly or through a `let` that does, is deferred. It takes part in the merge with its priority like any other contribution, but its value is computed only after the merge has picked the winners, and only after the attributes it reads are final. A deferred contribution that loses to a higher priority is never evaluated.

Before any deferred value is computed, the `self` dependencies of every contribution that the merge may evaluate are checked for cycles, block instance by block instance. This includes winners, lower whole values whose paths are not covered, and potential declared-default fallbacks under late whole values. Outside late whole values, declared defaults retain their edges whenever they may run, including ordinary winning defaults and lower whole map defaults that may supply omitted keys. A cycle is [`E0607`](/reference/error-codes#e0607). [Late-bound self](/guide/self) covers this in detail.

<!-- file: late.stav -->

```stav
struct Download {
  version: Str
  url:     Str = "https://example.org/tool-#{self.version}.tar.gz"
}

struct Root { downloads: Map<Str, Download> = {} block download }

download tool { version = "1.2.3" }
include "./bump.stav"
```

<!-- file: bump.stav -->

```stav
download tool { version = force("1.3.0") }
```

<!-- run: eval -o toml late.stav -->

```toml
[downloads.tool]
version = "1.3.0"
url = "https://example.org/tool-1.3.0.tar.gz"
```

## Checks on the merged value

Some checks need the final value, so they run after merging:

- A required field, one without a default whose type is not an `Option`, must have a value ([`E0801`](/reference/error-codes#e0801)).
- Refinements of struct fields are checked on the merged value ([`E0803`](/reference/error-codes#e0803)).
- Every `Ref<T>` must name a block that exists in the merged value, or a label the host declares ([`E0802`](/reference/error-codes#e0802)).

<!-- file: refs.stav -->

```stav
struct Unit {
  exec:  List<Str> where nonEmpty
  after: List<Ref<Unit>> = []
}

struct Root { units: Map<Str, Unit> = {} block unit }

unit db { exec = ["/usr/bin/db"] }
unit api { exec = ["/usr/bin/api"], after = [db, cache] }
```

<!-- run: eval refs.stav -->

```text
error[E0802]: `units.api.after` refers to `cache`, which is not a declared `unit`
  ┌─ refs.stav:9:50
  │
9 │ unit api { exec = ["/usr/bin/api"], after = [db, cache] }
  │                                                  ^^^^^
  │
  = note: references to `Unit` name the blocks of `unit`
```

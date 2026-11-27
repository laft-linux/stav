# Late-bound `self`

Stav evaluates statements eagerly and in order, with one exception. `self` is the **final, merged value** of an enclosing block. A right-hand side that reads `self` is not evaluated where it is written. It is kept aside and evaluated during merging, once the attributes it reads have their final values.

That lets one attribute be derived from another, even when the other one is set or overridden somewhere else.

## Deriving one attribute from another

<!-- file: tools.stav -->

```stav
struct Tool {
  version: Str
  url:     Str = "https://example.org/releases/tool-#{self.version}.tar.gz"
}

struct Root {
  tools: Map<Str, Tool> = {} block tool
}

tool query {
  version = "1.7.1"
}

include "./pin.stav"
```

<!-- file: pin.stav -->

```stav
tool query {
  version = force("1.8.0")
}
```

<!-- run: eval tools.stav -->

```json
{
  "tools": {
    "query": {
      "version": "1.8.0",
      "url": "https://example.org/releases/tool-1.8.0.tar.gz"
    }
  }
}
```

The default of `url` reads `self.version`. Because it is read after merging, it sees the forced `1.8.0` from `pin.stav`, not the `1.7.1` written next to it. A field default that reads `self` is the most common use: the schema describes how one value follows from another, and every instance gets it.

## Which block `self` means

`self.f` refers to the nearest enclosing block whose struct declares a field `f`. The search starts at the innermost block and moves outward. A field default is different: it searches only the struct being defaulted and never searches outward. If that struct does not declare `f`, the default is E0303 at its declaration, even when an enclosing instance declares `f`. A declaration therefore means the same wherever the struct is used. Write context-dependent expressions in a body at the place of use instead.

<!-- file: outward.stav -->

```stav
struct Source {
  url:    Str
  sha256: Str
}

struct Package {
  version: Str
  source:  Source
}

struct Root {
  packages: Map<Str, Package> = {} block package
}

package codec {
  version = "2.4"
  source {
    url    = "https://example.org/codec-#{self.version}.tar.gz"
    sha256 = "6b1d2f0c"
  }
}
```

<!-- run: eval -o toml outward.stav -->

```toml
[packages.codec]
version = "2.4"

[packages.codec.source]
url = "https://example.org/codec-2.4.tar.gz"
sha256 = "6b1d2f0c"
```

`Source` has no field `version`, so `self.version` inside `source { }` resolves to the enclosing `Package`. When no enclosing block declares the field, the program does not check:

<!-- file: unknown.stav -->

```stav
struct Root {
  name:  Str = "inventory"
  title: Str = ""
}

title = "#{self.label}"
```

<!-- run: check unknown.stav -->

```text
error[E0303]: no enclosing block has a field `label`
  ┌─ unknown.stav:6:17
  │
6 │ title = "#{self.label}"
  │                 ^^^^^
```

Inside a part or a part file, the search stops at the struct of the part. A part cannot reach the blocks around the place where it is included.

## `let` bindings that read `self`

A `let` statement in a body may read `self` as well. Every right-hand side that uses the binding is then late too:

<!-- file: lets.stav -->

```stav
struct Root {
  release: Str = "stable"
  channel: Str = ""
  banner:  Str = ""
}

let channel = "#{self.release}-channel"

channel = channel
banner  = "built for #{channel}"

release = "testing"
```

<!-- run: eval lets.stav -->

```json
{
  "release": "testing",
  "channel": "testing-channel",
  "banner": "built for testing-channel"
}
```

## Where `self` is allowed

`self` may appear in the right-hand side of an attribute, in a `let` statement inside a body, and in a struct field default. It may not appear where Stav needs a value before merging starts:

- the condition of an `if` statement and the collection of a `for` statement,
- block labels, map-literal keys and `include` expressions,
- the body of a function or a lambda,
- parameter defaults and sum payload defaults.

These positions decide which contributions exist, or they would let a function depend on where it is called. The restriction also applies through interpolations, function arguments and bindings that read `self`. Map-literal values may still read it. A struct-typed payload field still brings in that struct's field defaults, which may read `self`.

A late whole map, struct, sum or `Data` value may have other contributions at or below its path only when they have strictly higher priority, including through an `Option`. [E0504](/reference/error-codes#e0504) checks this before any deferred value runs. It excludes all declared defaults at or below the path, and a `none` contribution at the option's own path. Struct and map literals split into children at statically typed paths instead. Literals assigned to `Data` stay whole. Map keys cannot read `self` in any context, including inside lists, function arguments and `Data`.

Higher contributions win at their own paths. The whole supplies the remaining fields, map keys and matching variant payloads. Declared defaults fill only instances that higher contributions create and no whole value supplies, such as a missing map entry, an option value where the whole returns `none`, or a selected payload of another variant. This applies at every depth, including inside a whole struct. A late whole struct need not run if higher contributions cover every field recursively. A late whole map always runs, because it might supply omitted keys. A tagged-block selection also leaves the whole eligible to run: a matching payload survives, while a different variant is discarded. A higher whole attribute contribution of a sum replaces it instead. [Merging rules](/reference/merging#late-whole-values) gives examples and the full coverage rules.

Using `self` in a structural position is an error:

<!-- file: structural.stav -->

```stav
struct Root {
  debug: Bool = false
  level: Str = "info"
}

if self.debug {
  level = "debug"
}
```

<!-- run: check structural.stav -->

```text
error[E0606]: `self` cannot be used in an `if` condition
  ┌─ structural.stav:6:4
  │
6 │ if self.debug {
  │    ^^^^^^^^^^
  │
  = help: pass the value as a parameter instead
```

## Cycles

A late value can read another late value, but the reads must not go around in a circle, or neither value could ever be computed. Stav checks this in two stages. While type checking, a cycle inside one body produces the provisional warning `W0607`. Checking still succeeds because another contribution can override an edge during merging:

<!-- file: cycle.stav -->

```stav
struct Root {
  version: Str = ""
  tag:     Str = ""
}

version = "#{self.tag}"
tag     = "v#{self.version}"
```

<!-- run: check cycle.stav -->

```text
warning[W0607]: `self` dependencies form a cycle unless something overrides them
  ┌─ cycle.stav:6:11
  │
6 │ version = "#{self.tag}"
  │           ^^^^^^^^^^^^^
  │           │
  │           `Root.version` reads `Root.tag`
7 │ tag     = "v#{self.version}"
  │           ------------------ `Root.tag` reads `Root.version`
  │
  = note: cycle: Root.version -> Root.tag -> Root.version
  = help: set one of the attributes without `self` at a higher priority
```

If both edges win the merge, evaluation fails with `E0607`. This authoritative check uses every contribution that the merge may evaluate in each block instance, including lower whole values whose paths are not covered and potential declared-default fallbacks, before resolving late values:

<!-- run: eval cycle.stav -->

```text
warning[W0607]: `self` dependencies form a cycle unless something overrides them
  ┌─ cycle.stav:6:11
  │
6 │ version = "#{self.tag}"
  │           ^^^^^^^^^^^^^
  │           │
  │           `Root.version` reads `Root.tag`
7 │ tag     = "v#{self.version}"
  │           ------------------ `Root.tag` reads `Root.version`
  │
  = note: cycle: Root.version -> Root.tag -> Root.version
  = help: set one of the attributes without `self` at a higher priority

error[E0607]: `self` dependencies form a cycle
  ┌─ cycle.stav:6:11
  │
6 │ version = "#{self.tag}"
  │           ^^^^^^^^^^^^^
  │           │
  │           `version` reads `tag`
7 │ tag     = "v#{self.version}"
  │           ------------------ `tag` reads `version`
  │
  = note: cycle: version -> tag -> version
```

An overlay can break the same body-local cycle by forcing `tag` to a constant. The warning remains, but the winning graph is acyclic and evaluation succeeds:

<!-- file: cycle-forced.stav -->

```stav
struct Root {
  version: Str = ""
  tag:     Str = ""
}

version = "#{self.tag}"
tag     = "v#{self.version}"

include "./cycle-overlay.stav"
```

<!-- file: cycle-overlay.stav -->

```stav
tag = force("v1.0")
```

<!-- run: eval cycle-forced.stav -->

```text
{
  "version": "v1.0",
  "tag": "v1.0"
}
warning[W0607]: `self` dependencies form a cycle unless something overrides them
  ┌─ cycle-forced.stav:6:11
  │
6 │ version = "#{self.tag}"
  │           ^^^^^^^^^^^^^
  │           │
  │           `Root.version` reads `Root.tag`
7 │ tag     = "v#{self.version}"
  │           ------------------ `Root.tag` reads `Root.version`
  │
  = note: cycle: Root.version -> Root.tag -> Root.version
  = help: set one of the attributes without `self` at a higher priority
```

The final check looks at block instances, not at types. Two instances of the same struct may therefore derive their fields in opposite directions:

<!-- file: instances.stav -->

```stav
struct Package {
  version: Str = ""
  tag:     Str = ""
}

struct Root {
  packages: Map<Str, Package> = {} block package
}

package a {
  tag     = "v1.0"
  version = "#{self.tag}-release"
}

package b {
  version = "2.0"
  tag     = "v#{self.version}"
}
```

<!-- run: eval -o toml instances.stav -->

```toml
[packages.a]
version = "v1.0-release"
tag = "v1.0"

[packages.b]
version = "2.0"
tag = "v2.0"
```

A contribution that loses the merge adds no dependency either. When an overlay sets `url = force("...")` to a constant, the default `url` that read `self.version` is discarded, and so is its dependency on `version`.

A lower late whole value still adds its `self` edges when the merge may evaluate it, even if a higher contribution exists at the same node. For example, a declared late whole map default and a higher statement whole map both run to supply their keys, so both reads belong in the graph. A higher whole struct, sum, `Data` or `Option` replaces lower whole values throughout its path. A higher whole option therefore removes the reads of a lower whole at that option path even when it returns `none`. A tagged-block selection does not suppress a late whole sum or remove its reads.

Under a late whole value, declared defaults that might fill an instance created by higher contributions add their edges before the whole value runs. The graph includes these edges even if the whole later supplies the instance and the defaults are unused. This applies to missing map entries, option values and selected variant payloads at every depth, recursively through struct fields, including struct-typed payload fields. Consequently, a cycle can be rejected even when one of its default expressions would never run. The graph must be acyclic before the whole value is evaluated, so it cannot wait to find out which fallbacks are needed. A late whole value whose own type is a struct guarantees that instance and every instance reached through struct-typed fields. Their declared defaults never fill fields, never run and add no edges, including defaults that are themselves late whole maps or would otherwise win at their nodes. This uses paths and types alone. A whole value at an `Option` path does not provide this guarantee, even for `Option<Struct>`, because it may return `none`. Neither does a whole map or sum, nor a whole value above an option, map entry or variant on the path to the instance.

## How it fits with eager evaluation

Late binding is narrow on purpose. Everything that decides the shape of the value, such as conditions, loops, labels and includes, is evaluated eagerly and cannot read `self`. So the set of contributions is fixed before merging, and only the values of some attributes wait for it. Each winning late value is evaluated once, after the values it reads, along a dependency graph that is known to be acyclic. Potential declared-default fallbacks below late whole values are included in that graph before their use is known. Evaluation still always terminates.

Priorities are decided before deferred values run. A losing deferred value is not evaluated. Struct and map literals split recursively into contributions for each field or key, so only the winning late children run. Eager children still run when the literal is collected, even if another child reads `self`.

<!-- file: deferred-winners.stav -->

```stav
struct Settings {
  a: Int
  b: Int
}

struct Root {
  zero: Int = 0
  x:    Int
  cfg:  Settings
}

x = default(1 / self.zero)
x = 7

cfg   = default({ a = 1 / self.zero, b = self.zero })
cfg.a = 7
```

<!-- run: eval deferred-winners.stav -->

```json
{
  "zero": 0,
  "x": 7,
  "cfg": {
    "a": 7,
    "b": 0
  }
}
```

Both divisions in this program read `self`, and both lose to `7`. The sibling `b` still contributes its final value. Removing `self` from the division makes it eager. It then reports an error even though `cfg.a` has a higher priority contribution:

<!-- file: eager-loser.stav -->

```stav
struct Root {
  zero: Int = 0
  cfg:  Map<Str, Int>
}

cfg   = default({ a = 1 / 0, b = self.zero })
cfg.a = 7
```

<!-- run: eval eager-loser.stav -->

```text
error[E0603]: division by zero
  ┌─ eager-loser.stav:6:25
  │
6 │ cfg   = default({ a = 1 / 0, b = self.zero })
  │                         ^
```

## Late and eager struct values

A constructor computes a complete struct, including its defaults. When an eager struct value is assigned to an attribute, only the fields explicitly given in its construction become contributions. Its defaulted fields are filled again from the merged instance. A late whole struct cannot use that record of explicit fields, because it is not known before evaluation. Every field in its result counts as given instead.

<!-- file: late-eager-download.stav -->

```stav
struct Download {
  version: Str
  url:     Str = "https://example.org/tool-#{self.version}.tar.gz"
}

struct Root {
  version: Str = "1"
  late:    Download
  eager:   Download
}

fn make(version: Str) -> Download = Download { version = version }

late          = make(self.version)
late.version  = force("9")
eager         = make("1")
eager.version = force("9")
```

<!-- run: eval late-eager-download.stav -->

```json
{
  "version": "1",
  "late": {
    "version": "9",
    "url": "https://example.org/tool-1.tar.gz"
  },
  "eager": {
    "version": "9",
    "url": "https://example.org/tool-9.tar.gz"
  }
}
```

Both versions become `9`. The late whole value keeps the `url` that `make` computed from version `1`. The eager assignment contributes only `version`, so its `url` default runs against the merged version `9`. Declared defaults do not compete with the late whole value for E0504, but they do not replace fields it supplies either.

## Next

[Imports and modules](./modules) shows how to share declarations between files.

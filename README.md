# scenegraph

A lossless parser and structural model for Godot text resource files
(`.tscn` / `.tres`, format 3), plus a small CLI for inspecting and
validating them.

This repository is the foundation for a future `sg fix` (auto-repair
broken `.tscn`/`.tres` files) and `sg merge` (a git merge driver for
scenes). Both of those depend on one hard guarantee, which is what this
repository actually implements and tests: **round-tripping a well-formed
file through parse -> serialize reproduces it byte-for-byte.** Untouched
text must never change, not even a trailing space or a line ending,
because a merge driver that "cleans up" lines it never touched destroys
information no one asked it to touch.

## Workspace layout

- `crates/scenegraph-core` - the parser and structural data model, plus a
  small mutation layer (`src/edit.rs`) for surgical, span-exact edits. No
  required dependencies beyond the standard library.
- `crates/sg` - a CLI (`sg parse`, `sg roundtrip`, `sg check`, `sg fix`)
  built on top of scenegraph-core. Depends on `clap` for argument parsing;
  everything else (diffing, JSON output) is hand-rolled to keep the
  dependency footprint small.
- `fixtures/` - realistic `.tscn`/`.tres` sample files used by the test
  suite, `fixtures/invalid/` for error-path (parse failure) tests,
  `fixtures/broken/` for structurally valid but semantically broken files
  used by the `sg check`/`sg fix` test suite, and two minimal real Godot
  projects (a `project.godot` plus a handful of files) used by the rules
  that resolve `res://` paths against disk: `fixtures/engine_project/` for
  `missing-ext-resource-path` and `sg check --engine` (see below), and
  `fixtures/case_mismatch_project/` for `ext-resource-path-case-mismatch`.

## Design

### Losslessness

Godot's text resource format is, at the top level, a sequence of
`[section header]` blocks, each followed by `key = value` property lines.
`scenegraph-core` never re-renders text from a parsed model. Instead, it
scans the source once and records byte spans (`Range<usize>`) into the
original string for every header line and every property line. Parsing
never discards a byte: it is always classified as belonging to some
span, even when the parser cannot make sense of it (see "Tolerant
parsing" below).

`Document::serialize()` walks those spans in order and copies the
corresponding slice of the original source for each one. For a
well-formed file this necessarily reproduces the input exactly, because
the spans partition the source with no gaps, no overlaps, and no
reordering - the round-trip guarantee falls out of that invariant rather
than being asserted separately. This also means CRLF/LF (even mixed
within one file), a leading UTF-8 BOM, the presence or absence of a
trailing newline, blank lines, and incidental whitespace around `=` are
all preserved automatically: they were never interpreted as anything
other than "bytes belonging to some span" in the first place.

A property or header attribute's *value text* may itself span multiple
physical lines - either because it is an array/dictionary/constructor
call that wraps, or because it is a quoted string containing a literal,
unescaped newline (this shows up in real projects for embedded shader
source). The low-level scanner (`src/scan.rs`) tracks bracket depth and
quoted-string state to find where a value really ends, rather than
assuming one line == one property.

On top of that lossless layer, `scenegraph-core` provides typed,
best-effort structural accessors - file descriptor, `ext_resource` /
`sub_resource` lists, nodes, connections, editable overrides, node-tree
reconstruction, and `ExtResource`/`SubResource` reference enumeration
(including references nested inside arrays and dictionaries). These are
built by additionally parsing the text inside spans (header attributes,
property values) into a small variant-literal AST (`src/value.rs`).
Failure to structurally interpret a piece of text never affects
losslessness - it only means that particular structural accessor returns
less information.

### Tolerant parsing

`Document::parse` is strict: if any part of the input could not be
confidently classified (an unterminated section header, an unclosed
string, unbalanced brackets that never close before EOF), it returns a
`ParseError` with a 1-based line/column pointing at the problem.

`Document::parse_tolerant` never fails. Anything it cannot classify
becomes an opaque "unknown" line, preserved verbatim, and a `Diagnostic`
describing what was skipped is returned alongside the document. The
document produced this way still round-trips byte-for-byte through
`serialize()` - this is the mode `sg fix` will eventually build on, since
it needs to load files that are already partially broken.

Neither mode panics on malformed input; this is exercised directly in
`crates/scenegraph-core/tests/errors.rs` against the fixtures under
`fixtures/invalid/`.

### Mutation: surgical, span-exact edits

`crates/scenegraph-core/src/edit.rs` adds three operations on top of the
read-only model, all built around one primitive: an `Edit` is a byte
`Span` plus replacement text, computed against a document's *current*
spans. `Document::apply_edits` applies a batch of them in a single
left-to-right pass and re-scans the result into a fresh `Document` -
every byte not covered by some edit's span is copied through unchanged,
which is what lets `sg fix` uphold the same "untouched text never
changes" guarantee even while mutating:

- `edit_header_attr(section, key, new_value)` - rewrites one header
  attribute's value text in place (e.g. `load_steps=3` -> `load_steps=7`
  touches only the digit), or inserts `" key=value"` just before the
  header's closing `]` if the attribute is absent.
- `edit_delete_section(section)` - removes a section's header and body
  (which already includes its trailing blank line(s), so deleting it
  never leaves a double-blank artifact).
- `edit_reorder_sections(section_indices, new_order)` - swaps the
  *content* of a set of sections among their own byte-range "slots",
  verbatim; a permutation, not a re-render, so nothing about the
  sections' own text is ever altered, and nothing between or around them
  is touched either.

Because every edit's span is computed once, against the original
document, `sg fix` builds several edits (say, a `load_steps` rewrite plus
some deletions plus a reorder) independently and applies them all in one
batch, rather than mutating and re-parsing between each one.

## CLI usage

```sh
# Parse a file and print structural statistics.
sg parse path/to/scene.tscn

# Parse, serialize, and verify the result is byte-for-byte identical to
# the input. Exits non-zero (and prints the first point of divergence)
# on any mismatch or parse error.
sg roundtrip path/to/scene.tscn

# Report structural problems (see "sg check" below).
sg check path/to/scene.tscn
sg check path/to/scene_dir/ --json

# Fix everything mechanically fixable, in place (see "sg fix" below).
sg fix path/to/scene.tscn
sg fix path/to/scene_dir/ --dry-run
sg fix path/to/scene.tscn --keep-unused
```

## `sg check`

Reports structural problems in `.tscn`/`.tres` files without modifying
anything. Accepts any mix of file and directory paths; directories are
searched recursively for `*.tscn`/`*.tres`. Output is one line per issue:
`file:line: severity [code] message`. Pass `--json` for a machine-
readable JSON array of `{file, line, severity, code, message, fixable}`
objects instead (intended for AI agents / MCP tooling; hand-rolled rather
than pulling in `serde` for one small, fixed shape, with string escaping
covered by unit tests in `crates/sg/src/json.rs`).

Exit code: `0` clean, `1` at least one issue found, `2` at least one
input file failed to parse (a `parse-error` issue is still emitted for
it, with `fixable: false`).

Rules, each tagged fixable or not:

| Code | Severity | Fixable | What it means |
|---|---|---|---|
| `load-steps-mismatch` | warning | yes | The descriptor's `load_steps` does not equal `ext_resource count + sub_resource count + 1` (including when it's omitted but resources exist). |
| `broken-ext-resource-ref` / `broken-sub-resource-ref` | error | no | An `ExtResource("id")`/`SubResource("id")` (including a node header's `instance=ExtResource(...)`) names an id with no matching declaration. |
| `sub-resource-forward-reference` | warning | yes | A `sub_resource` section references another `sub_resource` declared later in the file - Godot's loader processes sections sequentially, so this fails to resolve when actually loaded. |
| `circular-sub-resource-reference` | error | no | Two or more `sub_resource` sections reference each other in a cycle; no file order can satisfy every dependency. |
| `child-before-parent` | warning | yes | A `[node]` section appears before the `[node]` section of its own `parent=` path. |
| `orphan-node` | error | no | A node's `parent=` path does not match any node in the file. |
| `multiple-root-nodes` | error | no | More than one node has no `parent` attribute. |
| `unused-ext-resource` / `unused-sub-resource` | warning | yes | Not reachable from any `[node]`/`[connection]`/`[resource]` section, directly or transitively through other reachable `sub_resource`s. |
| `duplicate-ext-resource-id` / `duplicate-sub-resource-id` | error | no | The same id is declared by more than one `ext_resource`/`sub_resource` section (checked in separate namespaces, since `ExtResource("x")` and `SubResource("x")` never collide). |
| `missing-ext-resource-path` | error | no | An `ext_resource`'s `path="res://..."` does not resolve to any file on disk under the file's Godot project root (checked component-by-component against real directory listings; `uid=` attributes and non-`res://` paths are not checked). Skipped entirely for a file with no `project.godot` ancestor - see "Project-relative (`res://`) paths" below. |
| `ext-resource-path-case-mismatch` | warning | no | An `ext_resource`'s `path="res://..."` exists on disk, but some path component differs from it only in character case. Harmless on case-insensitive filesystems (Windows, macOS) but breaks on Linux and in exported builds, which are case-sensitive. |

### Project-relative (`res://`) paths

`missing-ext-resource-path` and `ext-resource-path-case-mismatch` need a Godot
project root to resolve a `res://` path against: the nearest ancestor
directory of the checked file that contains `project.godot` (the same
resolution `sg check --engine` uses - see below). A file with no such
ancestor has no meaningful `res://` root, so both rules are silently
skipped for it; that case is `--engine`'s `engine-project-not-found`
territory, not a new issue kind here. Directory listings are cached per
`sg check` invocation per file, since one file's `ext_resource` sections
commonly share leading path components (e.g. a common `scripts/`
directory).

## `sg check --engine`

Most of the rules above validate ids declared *within* a file (does
`ExtResource("x")` resolve to some `ext_resource` id also declared in this
file?) and never touch disk. `missing-ext-resource-path` and
`ext-resource-path-case-mismatch` are the exception: they do look at disk,
resolving each `ext_resource`'s `path="res://..."` against the file's Godot
project root and reporting it as missing or case-mismatched. What static
checking still cannot do is tell you whether Godot itself can actually load
the file - a syntactically fine, on-disk-and-correctly-cased `path` can still
point at something Godot's own loader rejects for reasons `sg` has no model
of (a corrupt resource, an incompatible format, a script with a syntax
error). `--engine` closes *that* gap by handing each file to a real,
headless Godot instance and trusting its judgment instead: a small generated
GDScript loads the file through Godot's own `ResourceLoader`, checks every
dependency Godot itself reports for it (`ResourceLoader.get_dependencies` +
`ResourceLoader.exists`), and separately checks that `ResourceLoader.load()`
doesn't return `null`. That dependency-existence check is what originally
motivated `--engine` and is still worth keeping even now that the static
rules cover the common case: it is Godot's own, independent confirmation of
exactly what `missing-ext-resource-path` already found, and it exercises the
same `ResourceLoader.get_dependencies` traversal Godot uses at load time,
which does not always agree byte-for-byte with `sg`'s own `ext_resource`
`path` scan (e.g. for resource types with dependencies beyond their direct
`ext_resource` list).

Static checks are the default because they're instant and need nothing
installed. Use `--engine` as the ground-truth pass - before a commit, or in
CI - once the static checks are already clean, since it costs a Godot process
launch per project.

### Requirements

`--engine` needs a Godot 4.x executable. `sg` looks for one in this order,
stopping at the first tier that's set at all (an explicit
`--godot-path`/`SG_GODOT` that doesn't point at a real, executable file is a
hard error - it does not fall through to the next tier):

1. `--godot-path <path>` on the command line.
2. the `SG_GODOT` environment variable.
3. `godot4`, then `godot`, on `PATH`.

If none of these resolve, `sg` exits with an error listing exactly what it
checked, e.g.:

```
error: could not find a Godot executable for --engine. Checked, in order: (1) --godot-path flag: not given; (2) SG_GODOT environment variable: not set; (3) 'godot4' or 'godot' on PATH: not found. Pass --godot-path <path>, set SG_GODOT, or add a Godot 4.x executable named 'godot4' or 'godot' to PATH.
```

`.godot-bin/` at the repository root is git-ignored specifically as a
conventional local drop location for a downloaded Godot editor binary, so you
can keep one around per-checkout without it ever being staged:

```sh
sg check fixtures/ --engine --godot-path .godot-bin/Godot_v4.7.1-stable_win64.exe
```

On Windows, PATH search for `godot4`/`godot` also matches `.exe`, `.cmd`, and
`.bat` variants of that name, not just an extension-less file.

### Example

Given `fixtures/engine_project/broken.tscn`, whose `ext_resource` points at a
script that doesn't exist:

```sh
$ sg check fixtures/engine_project/valid.tscn fixtures/engine_project/broken.tscn --engine --godot-path .godot-bin/Godot_v4.7.1-stable_win64.exe
fixtures/engine_project/broken.tscn:1: error [engine-load-failed] Godot failed to load 'res://broken.tscn': missing dependency: res://scripts/does_not_exist.gd
```

`valid.tscn` produced no line at all - the engine loaded it cleanly. Every
file passed to `--engine` must sit inside a directory tree with a
`project.godot` above it somewhere (that's what gives a file's `res://` path
meaning); files that don't are reported as `engine-project-not-found` instead
of being handed to Godot. Files under the same `project.godot` are verified
together in a single Godot process launch, so checking many scenes in one
project costs one engine startup, not one per file.

### Timeout

`--engine-timeout <seconds>` bounds how long the headless Godot process for
one project is allowed to run before `sg` kills it and reports
`engine-timeout` for every file in that project that hadn't already reported
a result. Default: `30` seconds.

### Exit codes

`--engine` issues (`engine-load-failed`, `engine-timeout`,
`engine-project-not-found`, and `engine-run-failed` for a Godot process that
exited without reporting a result) are never fixable and fold into the same
exit code `1` as any static issue found. On top of the exit codes already
described for `sg check`, `--engine` adds one more: exit code `3` means
engine verification could not run *at all* - no Godot binary could be found,
or the Godot process itself could not be started - and always wins over
whatever exit code the static results alone would have produced, since it
means the engine pass never happened, not that it passed. The engine pass
also never runs at all if any input file already failed to parse (exit code
`2`): there's nothing meaningful to hand Godot, and running a slow engine
pass over the files that did parse would just delay reporting a failure
that's already final.

## `sg fix`

Repairs everything `sg check` marks fixable, in place, and leaves
everything it marks unfixable untouched (reported, not silently
dropped). Accepts the same file/directory arguments as `sg check`.

- `--dry-run` - print what would change (a unified diff) without writing
  anything. Safe to run in CI as a "would this pass `sg fix`" check.
- `--keep-unused` - disable unused-resource deletion; every other
  fixable rule still applies (in particular, a forward reference from a
  kept-but-unused resource is still reordered, since that's an
  independent problem).

Every run is computed from a fresh parse and a fresh re-check of the
result, never assumed:

1. Parse the file (a parse failure here is reported per-file and
   contributes to exit code `2`, matching `sg check`).
2. Build one batch of edits: unused-resource deletions first (so the
   counts below reflect what's actually left), then a stable topological
   reorder of the surviving `sub_resource` sections (dependency-first;
   already-correct order is left untouched; an unresolvable cycle is
   left in place, `stable_topo_sort` is a bounded Kahn's-algorithm pass
   so it can never hang on one), then the same kind of reorder for
   `node` sections (skipped entirely if there's an orphan node or
   multiple roots, since no valid tree order exists), then a
   `load_steps` rewrite computed from the post-deletion counts.
3. Apply the batch (or write nothing, in `--dry-run`) and re-check the
   result from scratch. The exit code and the trailing issue lines both
   reflect that fresh check, never the pre-fix issue list - so a
   `--keep-unused` run that leaves real, still-fixable issues in the file
   on purpose is correctly still exit code `1`, not `0`.

Exit code: `0` clean (nothing to fix, or everything got fixed), `1` at
least one issue remains after fixing (including a deliberately-kept one),
`2` at least one input file failed to parse.

Two properties are enforced by the test suite in
`crates/sg/tests/check_and_fix.rs`, not just asserted in prose:

- **Idempotency** - running `sg fix` again on an already-fixed file is a
  byte-for-byte no-op.
- **Minimal diff** - for the load_steps-only, forward-reference-only, and
  child-before-parent-only fixtures, the fixed output is compared
  byte-for-byte against a hand-derived expected string, confirming
  nothing outside the touched section(s) changed.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

## License

This project is licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.

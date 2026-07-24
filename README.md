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
- `crates/sg` - a CLI (`sg parse`, `sg roundtrip`, `sg check`, `sg fix`,
  `sg i18n extract`, `sg i18n budget`, `sg i18n check`) built on top of
  scenegraph-core. `src/nodegraph.rs` holds the node-graph reconstruction
  shared by `src/rules.rs` (structural checks) and `src/i18n/` (the `sg
  i18n` command family). Depends on `clap` for argument parsing and
  `toml` for `sg.toml` configuration (see "Configuration (`sg.toml`)"
  below - the one deliberate exception to keeping the dependency
  footprint small, since hand-rolling TOML's quoting/escaping/comment
  rules is not worth it for one config file format); everything else
  (diffing, JSON output, the PO/CSV writers and reader in `sg i18n
  extract`, the text-width estimation in `sg i18n budget`) is still
  hand-rolled.
- `fixtures/` - realistic `.tscn`/`.tres` sample files used by the test
  suite, `fixtures/invalid/` for error-path (parse failure) tests,
  `fixtures/broken/` for structurally valid but semantically broken files
  used by the `sg check`/`sg fix` test suite, and minimal real Godot
  projects (a `project.godot` plus a handful of files) used by the rules
  that resolve `res://` paths against disk: `fixtures/engine_project/` for
  `missing-ext-resource-path` and `sg check --engine` (see below),
  `fixtures/case_mismatch_project/` for `ext-resource-path-case-mismatch`,
  `fixtures/i18n_project/` for `sg i18n extract` and `sg i18n check`
  (including its `translations.de.po` fixture), and
  `fixtures/i18n_budget_project/` for `sg i18n budget`.

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

# Extract translatable UI strings into a PO or CSV file (see
# "sg i18n extract" below).
sg i18n extract path/to/scene_dir/
sg i18n extract path/to/scene.tscn --format csv
sg i18n extract path/to/scene_dir/ --output strings.po

# Statically predict UI text overflow before translation (see
# "sg i18n budget" below).
sg i18n budget path/to/scene_dir/
sg i18n budget path/to/scene.tscn --expansion 60 --json
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
| `duplicate-node-name` | error | no | Two or more `[node]` sections declare the same `name` under the same literal `parent` path (parentless nodes form one sibling group). Godot's editor auto-renames a node on creation to keep siblings unique, so this can only be a hand-edit or merge artifact; names are compared case-sensitively, and duplicates are reported regardless of `index=`, `instance=`, or `instance_placeholder=`. |
| `unused-ext-resource` / `unused-sub-resource` | warning | yes | Not reachable from any `[node]`/`[connection]`/`[resource]` section, directly or transitively through other reachable `sub_resource`s. |
| `duplicate-ext-resource-id` / `duplicate-sub-resource-id` | error | no | The same id is declared by more than one `ext_resource`/`sub_resource` section (checked in separate namespaces, since `ExtResource("x")` and `SubResource("x")` never collide). |
| `missing-ext-resource-path` | error | no | An `ext_resource`'s `path="res://..."` does not resolve to any file on disk under the file's Godot project root (checked component-by-component against real directory listings; `uid=` attributes and non-`res://` paths are not checked). Skipped entirely for a file with no `project.godot` ancestor - see "Project-relative (`res://`) paths" below. |
| `ext-resource-path-case-mismatch` | warning | no | An `ext_resource`'s `path="res://..."` exists on disk, but some path component differs from it only in character case. Harmless on case-insensitive filesystems (Windows, macOS) but breaks on Linux and in exported builds, which are case-sensitive. |
| `ext-resource-path-is-directory` | error | no | An `ext_resource`'s `path="res://..."` resolves to a real, existing directory rather than a file - every path component matches something on disk, but Godot's `ResourceLoader` can never load a directory as a resource, so this is just as broken as a path that doesn't exist at all. |
| `broken-connection-node-path` | error | no | A `[connection]`'s `from=`/`to=` NodePath does not resolve to any node declared in this file. Conservatively skipped whenever the path could plausibly resolve elsewhere: it lives under a node with `instance=`/`instance_placeholder=` (an instanced sub-scene this file can't see into), it contains `%`/`@`/`..`/`:` (unique-name, special, parent-traversal, or property-subpath syntax), or the file's own root node is itself an instance (an inherited scene). |

### Project-relative (`res://`) paths

`missing-ext-resource-path`, `ext-resource-path-case-mismatch`, and
`ext-resource-path-is-directory` all need a Godot project root to resolve a
`res://` path against: the nearest ancestor directory of the checked file
that contains `project.godot` (the same resolution `sg check --engine` uses -
see below). A file with no such ancestor has no meaningful `res://` root, so
all three rules are silently skipped for it; that case is `--engine`'s
`engine-project-not-found` territory, not a new issue kind here. Directory
listings are cached per `sg check` invocation per file, since one file's
`ext_resource` sections commonly share leading path components (e.g. a
common `scripts/` directory).

## Configuration (`sg.toml`)

Both `sg check` and `sg fix` accept a project-level `sg.toml`, letting you
turn a rule off entirely or change the severity it reports at, without any
new CLI flags. Drop it next to your scene files, or anywhere above them:

```toml
# sg.toml
[rules]
unused-ext-resource = "off"                    # disable a rule entirely
ext-resource-path-case-mismatch = "error"      # promote warning -> error
load-steps-mismatch = "warning"                # demote error -> warning
```

### Discovery

For each file being checked or fixed, `sg` walks upward from the file's
directory looking for `sg.toml`, using the exact same nearest-ancestor
resolution as `project.godot` (see "Project-relative (`res://`) paths"
above): the first ancestor directory that has one governs that file. A
checkout with no `sg.toml` anywhere behaves exactly as if this feature did
not exist - every rule at its built-in default severity. Directory-to-
config resolution and each distinct `sg.toml`'s parsed contents are both
cached per `sg`/`sg fix` invocation, so a run over many files sharing a
directory (or an ancestor) only walks the filesystem and parses a given
`sg.toml` once.

### `[rules]` keys and values

Each key under `[rules]` must be one of the exact issue codes from the
rules table above (the same kebab-case string shown in `sg check`'s
output and its `--json` `code` field). Each value is one of three plain
strings:

- `"off"` - the rule neither reports an issue nor gets repaired by
  `sg fix`. This is the one setting that changes fixing behavior: a
  disabled fixable rule (e.g. `unused-ext-resource`) is left untouched by
  `sg fix`, exactly as if it were unfixable.
- `"warning"` / `"error"` - overrides the rule's reported severity, in
  both text and `--json` output. This changes *only* the severity label;
  it never changes whether a rule is fixable, and it never changes exit
  codes - `sg check`/`sg fix` still exit `1` whenever at least one issue
  is reported, regardless of its (possibly overridden) severity.

`sg.toml` is strict about mistakes on purpose: an unrecognized rule name,
a value other than `"off"`/`"warning"`/`"error"`, or malformed TOML all
produce a config error naming the `sg.toml` path and the offending
key/value, and exit with the same exit code `sg check`/`sg fix` already
use for a file that failed to parse (`2`) - a typo'd rule name silently
doing nothing would be worse than a loud failure. Engine-pass issue codes
(`engine-load-failed`, `engine-timeout`, `engine-project-not-found`,
`engine-run-failed` - see "`sg check --engine`" below) are not
configurable this round; naming one in `[rules]` is rejected the same
way, with a message explaining that engine issues always run at their
built-in severity.

## `sg check --engine`

Most of the rules above validate ids declared *within* a file (does
`ExtResource("x")` resolve to some `ext_resource` id also declared in this
file?) and never touch disk. `missing-ext-resource-path`,
`ext-resource-path-case-mismatch`, and `ext-resource-path-is-directory` are
the exception: they do look at disk, resolving each `ext_resource`'s
`path="res://..."` against the file's Godot project root and reporting it
as missing, case-mismatched, or pointing at a directory instead of a file.
What static checking still cannot do is tell you whether Godot itself can
actually load the file - a syntactically fine, on-disk-and-correctly-cased
`path` can still point at something Godot's own loader rejects for reasons
`sg` has no model of (a corrupt resource, an incompatible format, a script
with a syntax error). `--engine` closes *that* gap by handing each file to
a real, headless Godot instance and trusting its judgment instead: a small
generated GDScript loads the file through Godot's own `ResourceLoader`,
checks every dependency Godot itself reports for it
(`ResourceLoader.get_dependencies` + `ResourceLoader.exists`), and
separately checks that `ResourceLoader.load()` doesn't return `null`. That
dependency-existence check is what originally motivated `--engine` and is
still worth keeping even now that the static
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

## `sg i18n extract`

The first of a planned `sg i18n` command family for localization tooling
(`extract`, `budget`, and `check` today; `shots` is planned next - see
`crates/sg/src/i18n/mod.rs`'s module doc comment). `extract` solves two
common localization pains: spreadsheet round-tripping (translators
usually work in a spreadsheet or a PO editor, not a `.tscn` file) and
translators having no context for the strings they're given (a bare
"OK" or "Cancel" with no idea which screen or control it belongs to).

```sh
sg i18n extract path/to/scene_dir/
sg i18n extract path/to/scene.tscn --format csv
sg i18n extract path/to/scene_dir/ --output strings.po
```

Accepts the same file/directory arguments as `sg check` (directories are
searched recursively for `*.tscn`/`*.tres`, using the same path-expansion
helper). Every `[node]` section across every scanned file is walked once,
via the same node-graph reconstruction `sg check`'s rules use
(`crates/sg/src/nodegraph.rs`, shared so the two can never disagree about
a node's path), and the following property lines are read off of it:

| Property | Typical nodes |
|---|---|
| `text` | `Label`, `Button`, `CheckBox`, `RichTextLabel`, etc. |
| `tooltip_text` | Any `Control` |
| `placeholder_text` | `LineEdit`, `TextEdit` |
| `title` | `Window` and subclasses |
| `dialog_text` | `AcceptDialog` and subclasses |

This is a deliberate, extensible v1 list (`i18n::TRANSLATABLE_PROPERTIES`)
- only non-empty string values are extracted (an empty `text = ""` is
skipped, not emitted as a hollow entry). Array/items-valued text
properties (`OptionButton`/`ItemList`'s `items`) are out of scope for v1;
see the doc comment above `TRANSLATABLE_PROPERTIES` for why.

Each extracted string carries context: the node's `type`, the scene's
root node name (its "screen" identity), which property it came from, a
`res://...` reference when the file sits inside a discoverable Godot
project (same `project.godot` discovery as `sg check`'s `res://`-path
rules - falls back to the scene's file path otherwise), the node's
root-relative path within the scene, and the source line.

### `--format po` (default)

A minimal, hand-rolled gettext PO file. Every occurrence of the same
exact text is merged into one entry, accumulating one `#.`
extracted-comment line and one `#:` reference line per occurrence, with
every `#.` line grouped before every `#:` line (never interleaved
per-occurrence) - the standard gettext convention of grouping by comment
*kind*, which is what `msgmerge`/`msgcat` normalize to and what
Poedit/Crowdin/Weblate expect:

```po
#. Type: Button | Screen: MainMenu | Property: text
#: res://ui/main_menu.tscn:VBox/StartButton
msgid "Start Game"
msgstr ""

#. Type: Button | Screen: MainMenu | Property: text
#. Type: Button | Screen: MainMenu | Property: text
#: res://ui/main_menu.tscn:VBox/CancelButton
#: res://ui/main_menu.tscn:VBox/CloseButton
msgid "Cancel"
msgstr ""
```

A minimal, valid PO header (`Content-Type: text/plain; charset=UTF-8`,
`Content-Transfer-Encoding: 8bit`) is always emitted first, even when no
translatable strings were found. `msgid` escaping covers backslash,
double quote, newline, tab, and carriage return. Entries are emitted in
first-occurrence order (the order each unique string was first seen while
scanning) rather than sorted alphabetically by `msgid` - this keeps
related strings from the same screen together instead of interleaving
unrelated ones, while staying fully deterministic (re-running over
unchanged input reproduces the file byte-for-byte, since scanning itself
is deterministic).

### `--format csv`

A `key,source,context` CSV for reviewing in a spreadsheet, chosen over
Godot's native `keys,<locale>` import shape because this command's job is
translator-facing review (the context problem), not re-importing
translations back into Godot - `.po`, which Godot can import directly, is
already the primary format for that. Unlike PO, rows are **not** merged
by text: one row per occurrence, so a translator sees every use in place
with its own context (`Type: ... | Screen: ... | Property: ... | Ref:
...`), since identical source text can legitimately need different
translations in different places. Fields are quoted per RFC 4180 (wrapped
in double quotes, with internal double quotes doubled, whenever a field
contains a comma, a quote, or a newline).

### `--output <FILE>`

Writes the rendered result to `FILE` instead of stdout.

### Exit codes

`0` every input file scanned cleanly (an empty result - no translatable
strings found - is not a failure), `2` at least one input file failed to
read or parse (matching `sg check`/`sg fix`'s parse-error exit code;
files that did scan cleanly still contribute their strings to the
output), `1` the rendered output could not be written to `--output`.

## `sg i18n budget`

The flagship of the `sg i18n` family: statically predicts UI text overflow
- a `Button`/`Label`/`LineEdit` whose translated text will not fit its
control - **before** anything is sent for translation, by reading each
control's dimensions and font size straight out of the `.tscn` source,
without launching the Godot engine at all.

```sh
sg i18n budget path/to/scene_dir/
sg i18n budget path/to/scene.tscn --expansion 60
sg i18n budget path/to/scene_dir/ --default-font-size 20 --json
```

### Design philosophy: approximate, but it always runs

This command is deliberately built as a linter, not an oracle, per its
owning design directive: **static overflow detection is worth more
running always and catching ~90% of incidents than being 99% accurate and
never actually run.** Like a linter, a rare false positive gets caught
in code review; a tool nobody runs prevents nothing. Two consequences
follow directly from that:

- **Font metrics are approximate by design.** `estimate_text_width`
  classifies each character and multiplies a small, hand-picked
  em-relative width table by the font size - it never reads an actual
  font file. CJK/full-width characters (Unicode ranges: CJK Unified
  Ideographs, Hiragana, Katakana, Hangul Syllables, CJK
  Symbols/Punctuation, Halfwidth/Fullwidth Forms) count as `1.0` em;
  Latin/proportional characters fall into one of a few buckets tuned
  against how proportional fonts actually render (values in em units, at
  font-size 1):

  | Bucket | Characters | Width (em) |
  |---|---|---|
  | Full-width / CJK | any character in the ranges above | `1.0` |
  | Very narrow | `i l j f t I . , ' ! \| :` | `0.3` |
  | Narrow | `r s` and space | `0.35` |
  | Wide | `m w` | `0.9` |
  | Uppercase (default) | any other uppercase ASCII letter | `0.65` |
  | Digit | `0`-`9` | `0.55` |
  | Lowercase (default) / unknown | any other lowercase ASCII letter, or anything else (accented Latin, Cyrillic, emoji, generic punctuation, ...) | `0.5` |

  `width_px = sum(char_width_em(c) for c in text) * font_size_px`.

- **Where a control's available width cannot be statically determined -
  it stretches to fill a parent/container - the control is skipped
  entirely, never guessed at.** A false alarm on every stretchy label
  would train users to ignore the tool. This is exactly what keeps the
  false-positive rate down while still catching the case that matters
  most: fixed-size buttons, which is where overflow actually bites.

### Available-width precedence

For each control, in order:

1. `custom_minimum_size = Vector2(W, H)` with `W > 0` -> available width
   is `W`. A Button/Label will not shrink below this, and in a fixed
   layout cannot grow past it either, so text exceeding `W` is the
   canonical overflow case this tool targets.
2. Otherwise, fixed offsets with non-stretching anchors: resolve
   `anchor_left`/`anchor_right` (an absent one defaults to Godot's own
   default of `0.0` - most scenes only write an anchor when it differs
   from that), or fall back to `anchors_preset` when neither anchor is
   written at all. If the anchors do not stretch horizontally
   (`anchor_left == anchor_right`, or an `anchors_preset` that is not one
   of the four horizontally-stretching presets - `PRESET_TOP_WIDE`,
   `PRESET_BOTTOM_WIDE`, `PRESET_VCENTER_WIDE`, `PRESET_FULL_RECT`) and
   both `offset_left` and `offset_right` are present, available width is
   `|offset_right - offset_left|`.
3. Otherwise: **undeterminable - the control is skipped, not warned
   about.**

`autowrap_mode` set to anything other than off (`0`) means the text wraps
vertically instead of overflowing horizontally, so such a control is
skipped regardless of the above. Font size comes from the node's own
`theme_override_font_sizes/font_size` if present, else `--default-font-size`.

### Which strings are checked

Only `text` and `placeholder_text` - the single-line, fixed-width-prone
properties, read off the same node-graph walk `sg i18n extract` uses
(`crates/sg/src/nodegraph.rs`), so the two commands never disagree about a
node's path. `tooltip_text` is never a candidate (Godot's tooltip popup
always sizes to fit its content, so a width budget is meaningless for
it). `title`/`dialog_text` (`Window`/`AcceptDialog`) are skipped in v1:
their sizing is windowing-system-managed, not a fixed control rect the
way a `Button`/`Label` inside a layout is.

### Overflow decision

For each candidate string: `source_px = estimate_text_width(text,
font_size)`, `predicted_px = source_px * (1 + expansion / 100)`. A
warning is emitted when `predicted_px` **strictly exceeds**
`available_px` (an exact match does not warn). `--expansion <PERCENT>`
(default `40`) is the assumed translation-expansion factor - a common
rule of thumb for English-source UI text (German/Finnish/etc. commonly
run 30-50% longer); tune it per project. `--default-font-size <PX>`
(default `16`, Godot's own default `Control` theme font size) is used
whenever a control sets no font-size override of its own.

Text output matches `sg check`'s `file:line: severity [code] message`
shape, issue code `i18n-text-overflow`, severity always `warning`:

```
fixtures/i18n_budget_project/menu.tscn:9: warning [i18n-text-overflow] "Settings" in Button "VBox/SettingsButton" may overflow: predicted ~76px (source ~54px +40%) exceeds ~70px available (custom_minimum_size, font_size 16)
```

`--json` emits an array of objects with `file`, `line`, `severity`,
`code`, `string`, `node_path`, `node_type`, `property`, `available_px`,
`source_px`, `predicted_px`, `expansion_percent`, `font_size`, and
`width_source` (`"custom_minimum_size"` or `"offset_left/offset_right"`).
Every numeric field is rounded to the nearest integer for display in both
text and JSON output - the overflow decision itself always compares
full-precision numbers; rounding only happens at render time.

### Exit codes

`0` no overflow risk found, `1` at least one found, `2` at least one input
file failed to read or parse (matching `sg check`/`sg i18n extract`'s
parse-error exit code, and taking priority over `1` the same way it does
there).

## `sg i18n check`

The CI gate for the `sg i18n` family: one command, one exit code, designed
to be dropped straight into a pull-request pipeline. It combines two
independent localization gates:

1. **Overflow** - reuses `sg i18n budget`'s scan exactly (the same
   `budget::scan` function, not a reimplementation - see "DRY reuse"
   below), so `sg i18n check`'s overflow results are always identical to
   running `sg i18n budget` with the same flags. Runs by default; skip it
   with `--no-overflow`.
2. **Untranslated** - source strings that are missing from, or present
   but empty in, a gettext PO file (typically one `sg i18n extract`
   produced and a translator filled in). Only runs when `--against` is
   given.

```sh
# Overflow gate only (no --against given).
sg i18n check path/to/scene_dir/

# Both gates: overflow, plus untranslated strings against a filled-in PO.
sg i18n check path/to/scene_dir/ --against strings.de.po

# Untranslated gate only.
sg i18n check path/to/scene_dir/ --against strings.de.po --no-overflow
```

This is the intended `sg i18n` loop: `extract` a PO file, hand it to
translators, then `check` it in CI on every PR so overflow risk and
untranslated leakage are both caught before a bad translation state ships.

### `--against <FILE.po>`

A gettext PO file (`msgid`/`msgstr` pairs) to check every scanned scene's
source strings against - most commonly one `sg i18n extract` produced,
later filled in by translators. Parsing mirrors `extract`'s PO writer
exactly (same escape set: `\\ \" \n \t \r`; same multi-line
continuation-string convention), so a `sg i18n check --against` run
against `sg i18n extract`'s own PO output always round-trips. A source
string is a finding (`i18n-untranslated`, always severity `error`) when:

- its text is **not a key** in the PO file at all - it was never
  extracted or sent for translation (leaked past `sg i18n extract`), or
- it **is** a key, but `msgstr` is **empty** - it was extracted but has
  not been translated yet.

Every *occurrence* is reported separately (one finding per node/property,
not deduplicated by text) - the same convention `sg i18n budget` already
uses for overflow findings, so a string used in two buttons and
untranslated in both produces two findings, each at its own location.

PO is single-target in v1: one `msgid`/`msgstr` pair per string, no
per-locale sections to select between. Duplicate `msgid`s in the PO file
resolve last-occurrence-wins; a malformed entry (an unterminated string,
an unrecognized escape, a `msgid` with no following `msgstr`) is skipped
rather than aborting the whole file - see `crates/sg/src/i18n/extract.rs`,
`parse_po`'s doc comment for the exact policy.

Without `--against`, the untranslated gate does not run at all - `sg i18n
check` behaves as the overflow gate alone.

### `--locale <CODE>`

A cosmetic label (e.g. `de`) folded into untranslated-string messages only
("... in the translation file for locale \"de\""). It does not change how
`--against` is parsed in any way - PO being single-target in v1 means
there is no per-locale section to select with it.

### `--expansion` / `--default-font-size`

Passed straight through to the overflow gate; same meaning and same
defaults (`40`, `16`) as `sg i18n budget --expansion` /
`--default-font-size`. See "`sg i18n budget`" above for what they mean.

### `--no-overflow`

Skip the overflow gate and run only the untranslated gate. Requires
`--against` - passing `--no-overflow` with no `--against` leaves nothing
for `sg i18n check` to do at all, which is a usage error (reported
clearly on stderr, same exit code as a file error - see "Exit codes"
below).

### DRY reuse of the overflow gate

`sg i18n check`'s overflow gate is not a second implementation: it calls
`crate::i18n::budget::scan` - the exact function `sg i18n budget`'s own
command wraps - and reuses `budget`'s `OverflowFinding` message/JSON
rendering unchanged. The two commands can never silently disagree about
what overflows, because there is only one place that decision is made.

### Output

Text lines match `sg check`'s `file:line: severity [code] message` shape.
Findings from either gate are merged and sorted by `(file, line, code)`
before being printed - deterministic regardless of which gate found what,
or which order the two gates ran in:

```
fixtures/i18n_budget_project/menu.tscn:9: warning [i18n-text-overflow] "Settings" in Button "VBox/SettingsButton" may overflow: predicted ~76px (source ~54px +40%) exceeds ~70px available (custom_minimum_size, font_size 16)
fixtures/i18n_project/main_menu.tscn:15: error [i18n-untranslated] "Enter your name" in LineEdit "VBox/NameInput" has no entry in the translation file (never extracted or sent for translation)
fixtures/i18n_project/main_menu.tscn:21: error [i18n-untranslated] "Cancel" in Button "VBox/CancelButton" has an empty translation in the translation file (extracted but not yet translated)
```

`--json` emits a single array mixing both finding shapes. Overflow objects
reuse `sg i18n budget --json`'s exact shape (`file`, `line`, `severity`,
`code`, `string`, `node_path`, `node_type`, `property`, `available_px`,
`source_px`, `predicted_px`, `expansion_percent`, `font_size`,
`width_source`). Untranslated objects carry `file`, `line`,
`severity: "error"`, `code: "i18n-untranslated"`, `string`, `node_path`,
`node_type`, and `translation_state` (`"missing"` or `"empty"`).

### Exit codes

`0` clean (no finding from either gate), `1` at least one finding
(an overflow warning or an untranslated error), `2` a file - a scanned
scene, or the `--against` PO file - could not be read or parsed, **or**
`--no-overflow` was given without `--against` (a usage error). This
matches every other `sg`/`sg i18n` command's parse-error exit code, and
clap's own usage-error exit code (both already `2` in this codebase) - a
gate-usage error and a file error are not distinguishable by exit code
alone, exactly like a plain clap argument-parsing error already is.

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

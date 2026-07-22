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

- `crates/scenegraph-core` - the parser and structural data model. No
  required dependencies beyond the standard library.
- `crates/sg` - a CLI (`sg parse`, `sg roundtrip`) built on top of
  scenegraph-core. Depends on `clap` for argument parsing.
- `fixtures/` - realistic `.tscn`/`.tres` sample files used by the test
  suite, plus `fixtures/invalid/` for error-path tests.

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

## CLI usage

```sh
# Parse a file and print structural statistics.
sg parse path/to/scene.tscn

# Parse, serialize, and verify the result is byte-for-byte identical to
# the input. Exits non-zero (and prints the first point of divergence)
# on any mismatch or parse error.
sg roundtrip path/to/scene.tscn
```

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

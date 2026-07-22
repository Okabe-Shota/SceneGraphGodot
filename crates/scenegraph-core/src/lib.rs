//! scenegraph-core: a lossless parser and structural model for Godot text
//! resource files (`.tscn` / `.tres`, format 3).
//!
//! # Design
//!
//! Parsing never panics. [`Document::parse`] returns a [`ParseError`] on
//! malformed input; [`Document::parse_tolerant`] never fails at all,
//! recovering by treating unrecognized text as opaque "unknown" lines.
//!
//! The document keeps byte spans into the original source rather than
//! re-rendering text from a model, so [`Document::serialize`] reproduces
//! well-formed input byte-for-byte - this is required for scenegraph to be
//! usable as a git merge driver, where untouched lines must never change
//! by even one byte.
//!
//! On top of that lossless layer, [`Document`] exposes typed, best-effort
//! structural accessors (file descriptor, ext/sub resources, nodes,
//! connections, editable overrides, the reconstructed node tree, and
//! `ExtResource`/`SubResource` reference enumeration) for higher-level
//! tools such as `sg fix` and `sg merge`.

mod document;
mod error;
mod raw;
mod refs;
mod scan;
mod span;
mod tree;
mod value;

pub use document::{
    ConnectionInfo, Document, EditableInfo, ExtResourceInfo, FileDescriptor, FileKind, NodeInfo, PropertyInfo,
    SectionInfo, Stats, SubResourceInfo,
};
pub use error::{Diagnostic, ParseError, TreeError, ValueError};
pub use refs::{Reference, ReferenceKind};
pub use span::Span;
pub use tree::{NodeTree, TreeNode};
pub use value::{parse_complete, Value};

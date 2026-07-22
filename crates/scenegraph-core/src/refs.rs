//! Extraction of `ExtResource("id")` / `SubResource("id")` references from
//! a parsed [`crate::value::Value`] tree, including references nested
//! inside arrays, dictionaries, and other constructor calls.

use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferenceKind {
    ExtResource,
    SubResource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub kind: ReferenceKind,
    pub id: String,
}

/// Recursively walk `value`, appending every `ExtResource`/`SubResource`
/// reference found anywhere within it (including inside arrays,
/// dictionary keys/values, and nested constructor arguments) to `out`.
pub fn collect_references(value: &Value, out: &mut Vec<Reference>) {
    match value {
        Value::Call { name, args } => {
            if name == "ExtResource" {
                if let Some(id) = value.as_ext_resource_id() {
                    out.push(Reference {
                        kind: ReferenceKind::ExtResource,
                        id,
                    });
                }
            } else if name == "SubResource" {
                if let Some(id) = value.as_sub_resource_id() {
                    out.push(Reference {
                        kind: ReferenceKind::SubResource,
                        id,
                    });
                }
            }
            for arg in args {
                collect_references(arg, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_references(item, out);
            }
        }
        Value::Dictionary(pairs) => {
            for (k, v) in pairs {
                collect_references(k, out);
                collect_references(v, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::parse_complete;

    #[test]
    fn finds_reference_nested_in_array_and_dict() {
        let v = parse_complete(r#"{"a": [ExtResource("1_a"), SubResource("2_b")], "b": SubResource("3_c")}"#).unwrap();
        let mut refs = Vec::new();
        collect_references(&v, &mut refs);
        assert_eq!(refs.len(), 3);
        assert!(refs.contains(&Reference {
            kind: ReferenceKind::ExtResource,
            id: "1_a".into()
        }));
        assert!(refs.contains(&Reference {
            kind: ReferenceKind::SubResource,
            id: "2_b".into()
        }));
        assert!(refs.contains(&Reference {
            kind: ReferenceKind::SubResource,
            id: "3_c".into()
        }));
    }
}

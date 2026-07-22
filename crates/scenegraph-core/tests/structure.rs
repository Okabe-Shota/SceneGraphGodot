//! Integration tests for the structural (non-lossless) accessors: file
//! descriptor, ext/sub resource lists, node tree reconstruction, and
//! ExtResource/SubResource reference enumeration.

use std::fs;
use std::path::PathBuf;

use scenegraph_core::{Document, FileKind, ReferenceKind};

fn read_fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

#[test]
fn file_descriptor_reports_scene_metadata() {
    let doc = Document::parse(&read_fixture("01_basic_2d_scene.tscn")).unwrap();
    let fd = doc.file_descriptor().expect("expected a file descriptor");
    assert_eq!(fd.kind, FileKind::Scene);
    assert_eq!(fd.load_steps, Some(4));
    assert_eq!(fd.format, Some(3));
    assert_eq!(fd.uid.as_deref(), Some("uid://bxpq168tsujuo"));
}

#[test]
fn file_descriptor_reports_resource_type() {
    let doc = Document::parse(&read_fixture("03_shader_multiline.tres")).unwrap();
    let fd = doc.file_descriptor().unwrap();
    assert_eq!(
        fd.kind,
        FileKind::Resource {
            type_name: Some("Shader".to_string())
        }
    );
}

#[test]
fn ext_resources_carry_type_path_id_and_uid() {
    let doc = Document::parse(&read_fixture("01_basic_2d_scene.tscn")).unwrap();
    let ext = doc.ext_resources();
    assert_eq!(ext.len(), 1);
    assert_eq!(ext[0].type_name.as_deref(), Some("Texture2D"));
    assert_eq!(ext[0].path.as_deref(), Some("res://icon.svg"));
    assert_eq!(ext[0].id.as_deref(), Some("1_0ab2c"));
    assert_eq!(ext[0].uid.as_deref(), Some("uid://dcjxujtq6ur0y"));
}

#[test]
fn sub_resources_carry_type_and_id() {
    let doc = Document::parse(&read_fixture("01_basic_2d_scene.tscn")).unwrap();
    let sub = doc.sub_resources();
    assert_eq!(sub.len(), 2);
    assert_eq!(sub[0].type_name.as_deref(), Some("RectangleShape2D"));
    assert_eq!(sub[0].id.as_deref(), Some("RectangleShape2D_8k2xn"));
    assert_eq!(sub[1].id.as_deref(), Some("CircleShape2D_1x9pq"));
}

#[test]
fn node_tree_resolves_deep_parent_paths() {
    let doc = Document::parse(&read_fixture("01_basic_2d_scene.tscn")).unwrap();
    let tree = doc.build_tree().expect("tree should build");

    assert_eq!(tree.root_node().name, "Main");
    assert_eq!(tree.root_node().type_name.as_deref(), Some("Node2D"));

    let player_idx = tree.root_node().children[0];
    let player = tree.get(player_idx);
    assert_eq!(player.name, "Player");

    // Player has three children in file order: Sprite2D, CollisionShape2D,
    // Hurtbox.
    let child_names: Vec<&str> = player.children.iter().map(|&i| tree.get(i).name.as_str()).collect();
    assert_eq!(child_names, vec!["Sprite2D", "CollisionShape2D", "Hurtbox"]);

    // Hurtbox's own child (parent="Player/Hurtbox") resolves two levels
    // deep.
    let hurtbox_idx = player.children[2];
    let hurtbox = tree.get(hurtbox_idx);
    assert_eq!(hurtbox.children.len(), 1);
    assert_eq!(tree.get(hurtbox.children[0]).name, "CollisionShape2D");
}

#[test]
fn node_tree_handles_scene_inheritance_instance_root() {
    let doc = Document::parse(&read_fixture("05_scene_inheritance.tscn")).unwrap();
    let tree = doc.build_tree().unwrap();
    assert_eq!(tree.root_node().name, "Enemy");
    assert_eq!(tree.root_node().instance.as_deref(), Some("1_base"));
    assert_eq!(tree.root_node().type_name, None);
    assert_eq!(tree.root_node().children.len(), 2);

    let editables = doc.editables();
    assert_eq!(editables.len(), 1);
    assert_eq!(editables[0].path.as_deref(), Some("Enemy/Hitbox"));
}

#[test]
fn groups_are_parsed_from_node_header() {
    let doc = Document::parse(&read_fixture("07_groups_and_dict.tscn")).unwrap();
    let nodes = doc.nodes();
    let main = nodes.iter().find(|n| n.name == "Main").unwrap();
    assert_eq!(main.groups, vec!["enemies".to_string(), "damageable".to_string()]);
}

#[test]
fn references_are_found_in_nested_arrays_and_dicts() {
    let doc = Document::parse(&read_fixture("07_groups_and_dict.tscn")).unwrap();
    let refs = doc.references();
    // "drop_rules" holds a SubResource nested inside a dictionary value.
    // The id is declared (fixtures/07_groups_and_dict.tscn is a
    // well-formed fixture and must be `sg check`-clean; dangling-reference
    // detection has its own dedicated coverage in
    // fixtures/broken/02_broken_reference.tscn and
    // crates/sg/tests/check_and_fix.rs::detects_broken_reference_as_unfixable_error).
    assert!(refs
        .iter()
        .any(|r| r.kind == ReferenceKind::SubResource && r.id == "drop_table_1"));
}

#[test]
fn references_include_both_header_attrs_and_body_properties() {
    let doc = Document::parse(&read_fixture("05_scene_inheritance.tscn")).unwrap();
    let refs = doc.references();
    // instance=ExtResource("1_base") lives in a node *header* attribute.
    assert!(refs
        .iter()
        .any(|r| r.kind == ReferenceKind::ExtResource && r.id == "1_base"));
}

#[test]
fn animation_tracks_expose_node_paths_as_raw_properties() {
    let doc = Document::parse(&read_fixture("02_animation_player.tscn")).unwrap();
    let sections = doc.sections();
    let animation = sections
        .iter()
        .find(|s| s.kind == "sub_resource" && s.properties.iter().any(|p| p.key == "tracks/0/path"))
        .expect("animation sub_resource with tracks/0/path");
    let track_path = animation.properties.iter().find(|p| p.key == "tracks/0/path").unwrap();
    assert_eq!(track_path.raw_value, "NodePath(\"Sprite2D:position\")");
}

#[test]
fn packed_arrays_round_trip_through_the_value_parser() {
    let doc = Document::parse(&read_fixture("04_packed_arrays.tscn")).unwrap();
    let sections = doc.sections();
    let mesh = sections.iter().find(|s| s.kind == "sub_resource").unwrap();
    let surfaces = mesh.properties.iter().find(|p| p.key == "_surfaces").unwrap();
    let value = scenegraph_core::parse_complete(&surfaces.raw_value).expect("should parse as a value");
    // A one-element array containing a dictionary.
    let arr = value.as_array().expect("expected an array");
    assert_eq!(arr.len(), 1);
}

#[test]
fn stats_counts_match_expectations_for_basic_scene() {
    let doc = Document::parse(&read_fixture("01_basic_2d_scene.tscn")).unwrap();
    let stats = doc.stats();
    assert_eq!(stats.ext_resource_count, 1);
    assert_eq!(stats.sub_resource_count, 2);
    assert_eq!(stats.node_count, 6);
    assert_eq!(stats.connection_count, 1);
    // texture=ExtResource + shape=SubResource(x2)
    assert_eq!(stats.reference_count, 3);
}

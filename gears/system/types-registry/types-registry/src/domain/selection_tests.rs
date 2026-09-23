use super::*;

fn parse(names: &[&str]) -> Result<FieldSelection, SelectionError> {
    FieldSelection::parse(names)
}

#[test]
fn absent_selection_equals_the_explicit_default_set() {
    let explicit = parse(&[
        "gts_id",
        "gts_uuid",
        "kind",
        "origin",
        "lifecycle_status",
        "content_hash",
    ])
    .expect("the default spelled out is a valid selection");
    assert_eq!(explicit, FieldSelection::default());
    assert_eq!(
        FieldSelection::default().canonical(),
        "content_hash,gts_id,gts_uuid,kind,lifecycle_status,origin"
    );
}

#[test]
fn order_and_case_and_surrounding_whitespace_do_not_change_identity() {
    let a = parse(&["content", "gts_id"]).expect("valid");
    let b = parse(&[" GTS_ID ", "Content"]).expect("valid");
    assert_eq!(a, b);
    assert_eq!(a.canonical(), b.canonical());
}

#[test]
fn mandatory_fields_are_always_in_the_normalized_set() {
    let without = parse(&["content"]).expect("valid");
    let with = parse(&["gts_uuid", "content", "lifecycle_status", "gts_id"]).expect("valid");
    for field in FieldSelection::MANDATORY_FIELDS {
        assert!(without.contains(field), "{}", field.name());
    }
    assert_eq!(without, with, "naming a mandatory field changes nothing");
    assert_eq!(
        without.canonical(),
        "content,gts_id,gts_uuid,lifecycle_status"
    );
}

#[test]
fn every_selectable_field_round_trips_through_its_name() {
    for field in EntityField::ALL {
        assert_eq!(EntityField::from_name(field.name()), Some(field));
        let one = parse(&[field.name()]).expect("each field is selectable alone");
        assert!(one.contains(field), "{}", field.name());
    }
    assert_eq!(
        FieldSelection::full().fields().count(),
        EntityField::ALL.len()
    );
}

#[test]
fn documents_are_selected_individually() {
    let traits = parse(&["effective_traits"]).expect("valid");
    assert!(traits.contains(EntityField::EffectiveTraits));
    assert!(!traits.contains(EntityField::ResolvedSchema));
    assert!(!traits.contains(EntityField::Content));
    assert!(!FieldSelection::default().selects_any_document());
    assert!(traits.selects_any_document());
}

#[test]
fn an_empty_selection_is_refused() {
    assert_eq!(parse(&[]), Err(SelectionError::Empty));
    assert!(matches!(parse(&["  "]), Err(SelectionError::EmptySegment)));
}

#[test]
fn an_empty_segment_is_refused_rather_than_dropped() {
    assert_eq!(
        parse(&["content", "", "kind"]),
        Err(SelectionError::EmptySegment)
    );
}

#[test]
fn a_duplicate_is_refused_after_normalization() {
    assert_eq!(
        parse(&["content", "CONTENT"]),
        Err(SelectionError::Duplicate("content".to_owned()))
    );
}

#[test]
fn unknown_unavailable_and_nested_names_are_told_apart() {
    assert_eq!(
        parse(&["contents"]),
        Err(SelectionError::Unknown("contents".to_owned()))
    );
    for name in ["availability", "owned_by_context_tenant"] {
        assert_eq!(
            parse(&[name]),
            Err(SelectionError::Unavailable(name.to_owned())),
            "{name} is a DESIGN field P0 cannot answer",
        );
    }
    for name in [
        "content.title",
        "effective/resolved_schema",
        "provenance.owning_gear",
    ] {
        assert_eq!(
            parse(&[name]),
            Err(SelectionError::Nested(name.to_owned())),
            "{name}",
        );
    }
}

/// `key`, `status` and `etag` are envelope metadata, never selectable.
#[test]
fn envelope_metadata_is_not_selectable() {
    for name in ["key", "status", "etag", "resource_version", "owning_gear"] {
        assert_eq!(
            parse(&[name]),
            Err(SelectionError::Unknown(name.to_owned())),
            "{name}",
        );
    }
}

#[test]
fn the_canonical_spelling_parses_back_to_the_same_selection() {
    for selection in [
        FieldSelection::default(),
        FieldSelection::full(),
        parse(&["provenance"]).expect("valid"),
    ] {
        let canonical = selection.canonical();
        let names: Vec<&str> = canonical.split(',').collect();
        assert_eq!(parse(&names), Ok(selection), "{canonical}");
    }
}

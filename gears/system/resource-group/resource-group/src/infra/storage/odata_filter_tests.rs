use super::*;
use resource_group_sdk::odata::{GroupFilterField, MembershipFilterField};
use toolkit_odata::parse_filter_string;

const ID: &str = "019f1d85-e3a3-7670-ae2a-0257e3777fb7";

#[test]
fn quoted_uuid_normalization_covers_comparisons_lists_and_nested_expressions() {
    for filter in [
        format!("group_id eq '{ID}'"),
        format!("group_id ne '{ID}'"),
        format!("group_id in ('{ID}', {ID})"),
        format!("not (group_id eq '{ID}') or (group_id eq '{ID}' and resource_id eq '{ID}')"),
    ] {
        let parsed = parse_filter_string(&filter).expect("valid syntax");
        let normalized = validate_filter::<MembershipFilterField>(parsed.as_expr())
            .expect("quoted UUID accepted");
        assert_membership_values(&normalized);
    }
}

fn assert_membership_values(node: &FilterNode<MembershipFilterField>) {
    match node {
        FilterNode::Binary { field, value, .. } => assert_membership_value(*field, value),
        FilterNode::InList { field, values } => {
            for value in values {
                assert_membership_value(*field, value);
            }
        }
        FilterNode::Composite { children, .. } => {
            for child in children {
                assert_membership_values(child);
            }
        }
        FilterNode::Not(inner) => assert_membership_values(inner),
    }
}

fn assert_membership_value(field: MembershipFilterField, value: &Value) {
    match (field, value) {
        (MembershipFilterField::GroupId, Value::Uuid(id)) => assert_eq!(id.to_string(), ID),
        (MembershipFilterField::ResourceId, Value::String(id)) => assert_eq!(id, ID),
        other => panic!("unexpected field/value: {other:?}"),
    }
}

#[test]
fn quoted_uuid_policy_is_shared_with_group_filters() {
    for field in ["id", "tenant_id", "hierarchy/parent_id"] {
        let parsed = parse_filter_string(&format!("{field} eq '{ID}'")).expect("valid syntax");
        validate_filter::<GroupFilterField>(parsed.as_expr()).expect("quoted UUID accepted");
    }
}

#[test]
fn invalid_uuid_and_wrong_literal_types_are_validation_errors() {
    for filter in [
        "group_id eq 'invalid'",
        "group_id eq 123",
        "group_id in ('invalid')",
    ] {
        let parsed = parse_filter_string(filter).expect("valid syntax");
        let error = validate_filter::<MembershipFilterField>(parsed.as_expr())
            .expect_err("invalid UUID must fail");
        assert!(matches!(error, DomainError::Validation { .. }));
        assert!(error.to_string().contains("group_id"));
    }
}

/// Reject unsupported operations regardless of their position in the logical tree.
#[test]
fn collect_type_paths_rejects_nested_operators_and_non_strings() {
    use MembershipFilterField::{ResourceId, ResourceType};
    for (op, value) in [
        (FilterOp::Gt, Value::String("type".into())),
        (FilterOp::Contains, Value::String("type".into())),
        (FilterOp::Eq, Value::Null),
        (FilterOp::Eq, Value::Number(42.into())),
    ] {
        let invalid = FilterNode::binary(ResourceType, op, value);
        let other = FilterNode::binary(ResourceId, FilterOp::Eq, Value::String("id".into()));
        for tree in [
            FilterNode::Not(Box::new(invalid.clone())),
            FilterNode::and(vec![other.clone(), invalid.clone()]),
            FilterNode::or(vec![other, invalid]),
        ] {
            let error = collect_type_paths(&tree, ResourceType, &mut Vec::new()).unwrap_err();
            assert!(matches!(error, DomainError::Validation { .. }));
        }
    }
    for value in [Value::Null, Value::Number(42.into())] {
        let tree = FilterNode::InList {
            field: ResourceType,
            values: vec![Value::String("known".into()), value],
        };
        assert!(collect_type_paths(&tree, ResourceType, &mut Vec::new()).is_err());
    }
}

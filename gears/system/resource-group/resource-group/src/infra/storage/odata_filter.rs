//! RG filter normalization. GTS type predicates compare the stored identifier.

use sea_orm::EntityTrait;
use toolkit_db::odata::sea_orm_filter::{FieldToColumn, filter_node_to_condition};
use toolkit_db::secure::{Scoped, SecureSelect};
use toolkit_odata::ODataQuery;
use toolkit_odata::ast::{Expr, Value};
use toolkit_odata::filter::{
    FieldKind, FilterField, FilterNode, FilterOp, convert_expr_to_filter_node,
};
use uuid::Uuid;

use crate::domain::error::DomainError;

/// Apply the resolved filter once, preserving the caller's scope and original
/// filter hash for pagination, and clear the AST to prevent a second application.
pub(super) fn prepare_filter<F, M, E>(
    mut select: SecureSelect<E, Scoped>,
    query: &ODataQuery,
    type_field: F,
) -> Result<(SecureSelect<E, Scoped>, ODataQuery), DomainError>
where
    F: FilterField,
    M: FieldToColumn<F>,
    E: EntityTrait,
{
    if let Some(ast) = query.filter.as_deref() {
        let validated = validate_filter::<F>(ast)?;
        // Only eq/ne/in on the GTS type field; the stored identifier is
        // compared as written, and an unknown one simply matches nothing.
        collect_type_paths(&validated, type_field, &mut Vec::new())?;
        let condition = filter_node_to_condition::<F, M>(&validated)
            .map_err(|e| DomainError::validation(format!("invalid $filter: {e}")))?;
        select = select.filter(condition);
    }
    let mut query = query.clone();
    query.filter = None;
    Ok((select, query))
}

/// Accept quoted UUIDs only for UUID fields. Keep the caller's AST and filter
/// hash intact so cursor validation continues to describe the public query.
pub(super) fn validate_filter<F: FilterField>(ast: &Expr) -> Result<FilterNode<F>, DomainError> {
    let mut normalized = ast.clone();
    normalize_uuids::<F>(&mut normalized)?;
    convert_expr_to_filter_node::<F>(&normalized)
        .map_err(|e| DomainError::validation(format!("invalid $filter: {e}")))
}

fn normalize_uuids<F: FilterField>(expr: &mut Expr) -> Result<(), DomainError> {
    match expr {
        Expr::And(left, right) | Expr::Or(left, right) => {
            normalize_uuids::<F>(left)?;
            normalize_uuids::<F>(right)?;
        }
        Expr::Not(inner) => normalize_uuids::<F>(inner)?,
        Expr::Compare(left, _, right) => normalize_uuid_literal::<F>(left, right)?,
        Expr::In(left, values) => {
            for value in values {
                normalize_uuid_literal::<F>(left, value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn normalize_uuid_literal<F: FilterField>(
    left: &Expr,
    right: &mut Expr,
) -> Result<(), DomainError> {
    if let Expr::Identifier(name) = left
        && let Some(field) = F::from_name(name)
        && field.kind() == FieldKind::Uuid
        && let Expr::Value(Value::String(raw)) = right
    {
        let uuid = Uuid::parse_str(raw).map_err(|_| {
            DomainError::validation(format!(
                "invalid $filter: field {} expects a valid UUID",
                field.name()
            ))
        })?;
        *right = Expr::Value(Value::Uuid(uuid));
    }
    Ok(())
}

/// Collect borrowed identifiers and reject unsupported GTS operators.
fn collect_type_paths<'a, F: FilterField>(
    node: &'a FilterNode<F>,
    type_field: F,
    paths: &mut Vec<&'a str>,
) -> Result<(), DomainError> {
    match node {
        FilterNode::Binary { field, op, value } if *field == type_field => {
            if !matches!(op, FilterOp::Eq | FilterOp::Ne) {
                return Err(DomainError::validation(format!(
                    "invalid $filter: field {} supports only eq, ne, and in",
                    field.name()
                )));
            }
            paths.push(type_path(value, field.name())?);
        }
        FilterNode::InList { field, values } if *field == type_field => {
            for value in values {
                paths.push(type_path(value, field.name())?);
            }
        }
        FilterNode::Composite { children, .. } => {
            for child in children {
                collect_type_paths(child, type_field, paths)?;
            }
        }
        FilterNode::Not(inner) => collect_type_paths(inner, type_field, paths)?,
        _ => {}
    }
    Ok(())
}

fn type_path<'a>(value: &'a Value, field: &str) -> Result<&'a str, DomainError> {
    let Value::String(raw) = value else {
        return Err(DomainError::validation(format!(
            "invalid $filter: field {field} expects a GTS type path string"
        )));
    };
    Ok(raw)
}

#[cfg(test)]
#[path = "odata_filter_tests.rs"]
mod tests;

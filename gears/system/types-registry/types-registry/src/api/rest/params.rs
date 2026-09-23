//! Query-string guards for the v2 read routes (SPEC §10.2).
//!
//! axum drops unclaimed keys and `ToolKit` refuses only unknown `$` options, so an
//! undeclared parameter would otherwise be answered as if it had not been sent.

use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use toolkit::api::odata::{ODataQuery, extract_odata_query};
use toolkit_canonical_errors::CanonicalError;

use super::error::{
    duplicate_query_param, page_size_zero, pattern_too_long, query_params_unreadable,
    unsupported_query_params,
};
use super::select;
use crate::domain::selection::FieldSelection;

/// Parameters `GET /entities/{entity_key}` accepts.
pub const EXACT_READ: &[&str] = &["$select"];

/// `POST /entities:batchGet` carries everything in its body, `$select` included.
pub const BATCH_READ: &[&str] = &[];

fn guard(pairs: &[(String, String)], allowed: &[&str]) -> Result<(), CanonicalError> {
    let mut unsupported: Vec<&str> = Vec::new();
    for (key, _) in pairs {
        if !allowed.contains(&key.as_str()) && !unsupported.contains(&key.as_str()) {
            unsupported.push(key);
        }
    }
    if !unsupported.is_empty() {
        return Err(unsupported_query_params(&unsupported, allowed));
    }
    for (i, (key, _)) in pairs.iter().enumerate() {
        if pairs[..i].iter().any(|(earlier, _)| earlier == key) {
            return Err(duplicate_query_param(key));
        }
    }
    Ok(())
}

async fn raw_pairs<S: Send + Sync>(
    parts: &mut Parts,
    state: &S,
) -> Result<Vec<(String, String)>, CanonicalError> {
    let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state)
        .await
        .map_err(|e| query_params_unreadable(&e.to_string()))?;
    Ok(pairs)
}

/// # Errors
/// A `400` for an unsupported, repeated or malformed parameter.
pub async fn extract<S: Send + Sync>(
    parts: &mut Parts,
    state: &S,
    allowed: &[&str],
) -> Result<(ODataQuery, FieldSelection, Vec<(String, String)>), CanonicalError> {
    let pairs = raw_pairs(parts, state).await?;
    guard(&pairs, allowed)?;
    if let Some((_, raw)) = pairs.iter().find(|(key, _)| key == "$select") {
        select::check_raw(raw)?;
    }
    let query = extract_odata_query(parts, state).await?;
    let selection = select::from_names(query.selected_fields())?;
    Ok((query, selection, pairs))
}

/// The exact read's one query parameter, `$select`.
pub struct ExactReadSelection(pub FieldSelection);

impl<S: Send + Sync> FromRequestParts<S> for ExactReadSelection {
    type Rejection = CanonicalError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let (_, selection, _) = extract(parts, state, EXACT_READ).await?;
        Ok(Self(selection))
    }
}

/// `:batchGet` takes no query parameters; its `$select` is a body field.
pub struct NoQuery;

impl<S: Send + Sync> FromRequestParts<S> for NoQuery {
    type Rejection = CanonicalError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        guard(&raw_pairs(parts, state).await?, BATCH_READ)?;
        Ok(Self)
    }
}

/// Parameters `GET /entities` accepts; `limit`/`$top` and `cursor`/`$skiptoken`
/// are `ToolKit`'s two spellings of one slot each.
pub const DISCOVERY: &[&str] = &[
    "pattern",
    "limit",
    "$top",
    "cursor",
    "$skiptoken",
    "$select",
];

/// The discovery query, validated but with its cursor still unbound: the binding
/// needs the normalized selection, which only exists after extraction.
pub struct DiscoveryParams {
    pub pattern: Option<String>,
    pub limit: Option<u32>,
    pub cursor: Option<String>,
    pub selection: FieldSelection,
}

fn value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// One slot under either spelling; both at once is ambiguous.
fn slot<'a>(
    pairs: &'a [(String, String)],
    names: [&'static str; 2],
) -> Result<Option<(&'static str, &'a str)>, CanonicalError> {
    match names.map(|name| value(pairs, name).map(|v| (name, v))) {
        [Some(_), Some(_)] => Err(duplicate_query_param(names[1])),
        [first, second] => Ok(first.or(second)),
    }
}

impl<S: Send + Sync> FromRequestParts<S> for DiscoveryParams {
    type Rejection = CanonicalError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let pairs = raw_pairs(parts, state).await?;
        guard(&pairs, DISCOVERY)?;
        let limit = slot(&pairs, ["limit", "$top"])?;
        let cursor = slot(&pairs, ["cursor", "$skiptoken"])?;
        // Checked before ToolKit's extraction so the refusal names the spelling
        // the caller used and carries the gear's cursor diagnostics.
        if let Some((name, "0")) = limit {
            return Err(page_size_zero(name));
        }
        if let Some((_, token)) = cursor {
            super::cursor::check_readable(token)?;
        }

        let (query, selection, _) = extract(parts, state, DISCOVERY).await?;
        let pattern = value(&pairs, "pattern").map(str::to_owned);
        if let Some(p) = &pattern
            && p.len() > 1024
        {
            return Err(pattern_too_long(p.len()));
        }
        Ok(Self {
            pattern,
            limit: query.limit.map(|l| u32::try_from(l).unwrap_or(u32::MAX)),
            cursor: cursor.map(|(_, token)| token.to_owned()),
            selection,
        })
    }
}

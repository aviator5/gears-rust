//! The wire form of a discovery page's position (D12).
//!
//! `toolkit-odata`'s [`CursorV1`] rather than a bespoke token, because the property
//! the page contract needs is already there: the encoding is versioned base64url and
//! decoding **refuses** an unknown version instead of reading it as a position this
//! build understands. A cursor that outlives a protocol change is then a `400`
//! rather than a silently wrong page.
//!
//! # Why this lives in the transport layer
//!
//! The domain's position is a stored `gts_id` — [`DiscoveryQuery::after`]. The
//! base64url envelope is how one page hands that to the next *over HTTP*, so it is
//! encoding, not policy: `discover` never sees a token, and the bound below is a
//! contract rule a gRPC adapter would restate in its own encoding rather than
//! inherit.
//!
//! # What the cursor binds
//!
//! The query it was issued for: the pattern, `depth`, `kind` and the canonical
//! [`FieldSelection`]. Replaying a position under any of them changed is refused
//! rather than spliced; an absent `$select` and an explicit default one share one
//! canonical spelling and so one binding.
//!
//! [`DiscoveryQuery::after`]: crate::domain::registry_service::DiscoveryQuery::after

use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::pagination::short_filter_hash;
use toolkit_odata::{CursorV1, ODataOrderBy, OrderKey, SortDir, ast, validate_cursor_against};

use super::error::{cursor_not_usable, cursor_too_long};
use crate::domain::enums::EntityKind;
use crate::domain::selection::FieldSelection;

/// The one keyset column. `gts_id` is unique and immutable, which is what makes the
/// cursor a plain keyset: a page boundary cannot drift or duplicate (§10.2).
const KEY_FIELD: &str = "gts_id";

/// The binding's name for the selection; not an entity column.
const SELECT_FIELD: &str = "$select";
const KIND_FIELD: &str = "kind";
const DEPTH_FIELD: &str = "depth";

const fn kind_name(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::TypeSchema => "type_schema",
        EntityKind::Instance => "instance",
    }
}

/// Forward-only. Backward paging is not part of the discovery contract, and a
/// `"bwd"` token would describe a traversal this route does not perform.
const FORWARD: &str = "fwd";

/// The fixed page order: `gts_id` ascending.
fn page_order() -> ODataOrderBy {
    ODataOrderBy(vec![OrderKey {
        field: KEY_FIELD.to_owned(),
        dir: SortDir::Asc,
    }])
}

/// The query a page was taken under, hashed through `toolkit-odata`'s normalizer so
/// the opaque token does not spell it out. Never `None`: the selection is always
/// bound, so `validate_cursor_against` compares every pair of bindings.
fn binding_hash(binding: &Binding<'_>) -> Option<String> {
    let equals = |field: &str, value: &str| {
        ast::Expr::Compare(
            Box::new(ast::Expr::Identifier(field.to_owned())),
            ast::CompareOperator::Eq,
            Box::new(ast::Expr::Value(ast::Value::String(value.to_owned()))),
        )
    };
    let select = equals(SELECT_FIELD, &binding.selection.canonical());
    let base = match binding.pattern {
        Some(pattern) => ast::Expr::And(Box::new(equals(KEY_FIELD, pattern)), Box::new(select)),
        None => select,
    };
    // T22b's expression is the base, so a token issued before `depth`/`kind`
    // existed resumes the same traversal when neither is named. An absent filter
    // adds no term; a present one always changes the hash.
    let terms = [
        binding.kind.map(|kind| equals(KIND_FIELD, kind_name(kind))),
        binding
            .max_chain_depth
            .map(|depth| equals(DEPTH_FIELD, &depth.to_string())),
    ];
    let expr = terms.into_iter().flatten().fold(base, |expr, term| {
        ast::Expr::And(Box::new(expr), Box::new(term))
    });
    short_filter_hash(Some(&expr))
}

/// What a discovery cursor is bound to.
pub struct Binding<'a> {
    pub pattern: Option<&'a str>,
    pub kind: Option<EntityKind>,
    pub max_chain_depth: Option<u8>,
    pub selection: FieldSelection,
}

/// Encode the position a page stopped at, bound to the query that produced it.
///
/// # Errors
/// A canonical internal error if the token will not serialize, which is a bug here
/// rather than anything the caller did.
pub fn encode(after: &str, binding: &Binding<'_>) -> Result<String, CanonicalError> {
    CursorV1 {
        k: vec![after.to_owned()],
        o: SortDir::Asc,
        s: page_order().to_signed_tokens(),
        f: binding_hash(binding),
        d: FORWARD.to_owned(),
    }
    .encode()
    .map_err(|e| {
        tracing::error!(error = %e, "types_registry could not encode a discovery cursor");
        CanonicalError::internal("the registry could not construct a page cursor").create()
    })
}

/// Refuse an oversized or undecodable token before anything else reads it.
///
/// # Errors
/// A `400` naming `cursor`.
pub fn check_readable(token: &str) -> Result<(), CanonicalError> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(cursor_too_long(token.len()));
    }
    CursorV1::decode(token)
        .map(drop)
        .map_err(|e| cursor_not_usable(&e.to_string()))
}

/// Base64url JSON of one identifier; a real token cannot approach this.
const MAX_TOKEN_LEN: usize = 4096;

/// Decode a cursor into the stored `gts_id` the next page resumes after.
///
/// # Errors
/// A `400` problem naming `cursor` when the token is unreadable, of an unsupported
/// version, or bound to a different query than this request asks.
pub fn decode(token: &str, binding: &Binding<'_>) -> Result<String, CanonicalError> {
    let cursor = CursorV1::decode(token).map_err(|e| cursor_not_usable(&e.to_string()))?;
    let expected = binding_hash(binding);
    validate_cursor_against(&cursor, &page_order(), expected.as_deref())
        .map_err(|e| cursor_not_usable(&e.to_string()))?;
    // `validate_cursor_against` skips the comparison when the token has no filter,
    // which only a pre-T22b cursor lacks.
    if cursor.f != expected {
        return Err(cursor_not_usable(
            "it was issued for a different pattern, depth, kind or $select than this \
             request names",
        ));
    }
    if cursor.d != FORWARD {
        return Err(cursor_not_usable("discovery pages forward only"));
    }
    match cursor.k.as_slice() {
        [after] => Ok(after.clone()),
        keys => Err(cursor_not_usable(&format!(
            "a discovery cursor names exactly one key, not {}",
            keys.len()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AFTER: &str = "gts.cf.core.example.type.v1~";
    const PATTERN: &str = "gts.cf.core.example.*";

    fn bound<'a>(pattern: Option<&'a str>, select: &[&str]) -> Binding<'a> {
        filtered(pattern, None, None, select)
    }

    fn filtered<'a>(
        pattern: Option<&'a str>,
        kind: Option<EntityKind>,
        max_chain_depth: Option<u8>,
        select: &[&str],
    ) -> Binding<'a> {
        Binding {
            pattern,
            kind,
            max_chain_depth,
            selection: if select.is_empty() {
                FieldSelection::default()
            } else {
                FieldSelection::parse(select).expect("valid")
            },
        }
    }

    #[test]
    fn a_cursor_round_trips_its_position() -> Result<(), CanonicalError> {
        let token = encode(AFTER, &bound(None, &[]))?;
        assert_eq!(decode(&token, &bound(None, &[]))?, AFTER);
        let filtered = encode(AFTER, &bound(Some(PATTERN), &["content"]))?;
        assert_eq!(
            decode(&filtered, &bound(Some(PATTERN), &["content"]))?,
            AFTER
        );
        Ok(())
    }

    /// The token is opaque: neither the position's pattern nor the selection may be
    /// readable off it, or a caller will start editing one.
    #[test]
    fn the_token_does_not_spell_the_query_out() -> Result<(), CanonicalError> {
        let token = encode(AFTER, &bound(Some(PATTERN), &["content"]))?;
        for needle in [PATTERN, "example", "content"] {
            assert!(!token.contains(needle), "{token}");
        }
        Ok(())
    }

    #[test]
    fn a_cursor_from_another_pattern_is_refused() -> Result<(), CanonicalError> {
        let token = encode(AFTER, &bound(Some(PATTERN), &[]))?;
        assert!(decode(&token, &bound(Some("gts.cf.other.*"), &[])).is_err());
        assert!(decode(&token, &bound(None, &[])).is_err());
        let unfiltered = encode(AFTER, &bound(None, &[]))?;
        assert!(decode(&unfiltered, &bound(Some(PATTERN), &[])).is_err());
        Ok(())
    }

    #[test]
    fn a_cursor_from_another_selection_is_refused() -> Result<(), CanonicalError> {
        for pattern in [None, Some(PATTERN)] {
            let token = encode(AFTER, &bound(pattern, &[]))?;
            assert!(decode(&token, &bound(pattern, &["content"])).is_err());
            let content = encode(AFTER, &bound(pattern, &["content"]))?;
            assert!(decode(&content, &bound(pattern, &[])).is_err());
            assert!(decode(&content, &bound(pattern, &["content", "kind"])).is_err());
        }
        Ok(())
    }

    /// Absent is its own binding, distinct from every explicit value.
    #[test]
    fn a_cursor_from_another_kind_or_depth_is_refused() -> Result<(), CanonicalError> {
        let kinds = [
            None,
            Some(EntityKind::TypeSchema),
            Some(EntityKind::Instance),
        ];
        let depths = [None, Some(1), Some(2), Some(255)];
        for pattern in [None, Some(PATTERN)] {
            for issued in kinds.iter().flat_map(|k| depths.map(|d| (*k, d))) {
                let token = encode(AFTER, &filtered(pattern, issued.0, issued.1, &[]))?;
                for resumed in kinds.iter().flat_map(|k| depths.map(|d| (*k, d))) {
                    let result = decode(&token, &filtered(pattern, resumed.0, resumed.1, &[]));
                    assert_eq!(
                        result.is_ok(),
                        issued == resumed,
                        "{issued:?} -> {resumed:?}"
                    );
                }
            }
        }
        Ok(())
    }

    /// A T22b token, whose hash covered only pattern and `$select`, resumes while
    /// neither `depth` nor `kind` is named, and is refused once either is.
    #[test]
    fn a_t22b_cursor_resumes_under_the_same_absent_filters() -> Result<(), serde_json::Error> {
        let equals = |field: &str, value: &str| {
            ast::Expr::Compare(
                Box::new(ast::Expr::Identifier(field.to_owned())),
                ast::CompareOperator::Eq,
                Box::new(ast::Expr::Value(ast::Value::String(value.to_owned()))),
            )
        };
        let select = equals("$select", &FieldSelection::default().canonical());
        for (pattern, t22b_filter) in [
            (None, select.clone()),
            (
                Some(PATTERN),
                ast::Expr::And(Box::new(equals("gts_id", PATTERN)), Box::new(select)),
            ),
        ] {
            let token = CursorV1 {
                k: vec![AFTER.to_owned()],
                o: SortDir::Asc,
                s: page_order().to_signed_tokens(),
                f: short_filter_hash(Some(&t22b_filter)),
                d: FORWARD.to_owned(),
            }
            .encode()?;
            assert_eq!(
                decode(&token, &bound(pattern, &[])).ok().as_deref(),
                Some(AFTER),
                "{pattern:?}",
            );
            let with_kind = filtered(pattern, Some(EntityKind::Instance), None, &[]);
            assert!(decode(&token, &with_kind).is_err(), "{pattern:?}");
            assert!(decode(&token, &filtered(pattern, None, Some(2), &[])).is_err());
        }
        Ok(())
    }

    /// The binding is the canonical set, never the spelling that produced it.
    #[test]
    fn absent_and_explicit_default_selections_are_interchangeable() -> Result<(), CanonicalError> {
        let explicit = [
            "origin",
            "GTS_ID",
            "content_hash",
            "kind",
            "gts_uuid",
            "lifecycle_status",
        ];
        let token = encode(AFTER, &bound(Some(PATTERN), &[]))?;
        assert_eq!(decode(&token, &bound(Some(PATTERN), &explicit))?, AFTER);
        let token = encode(AFTER, &bound(Some(PATTERN), &explicit))?;
        assert_eq!(decode(&token, &bound(Some(PATTERN), &[]))?, AFTER);
        let reordered = encode(AFTER, &bound(None, &["kind", "content"]))?;
        assert_eq!(
            decode(&reordered, &bound(None, &["Content", " kind"]))?,
            AFTER
        );
        Ok(())
    }

    /// A T22a token carries no selection binding, so it cannot resume a T22b page.
    #[test]
    fn a_cursor_without_a_selection_binding_is_refused() -> Result<(), serde_json::Error> {
        let token = CursorV1 {
            k: vec![AFTER.to_owned()],
            o: SortDir::Asc,
            s: page_order().to_signed_tokens(),
            f: None,
            d: FORWARD.to_owned(),
        }
        .encode()?;
        assert!(decode(&token, &bound(None, &[])).is_err());
        Ok(())
    }

    #[test]
    fn an_unreadable_token_is_refused() {
        assert!(decode("not-base64url-json", &bound(None, &[])).is_err());
        assert!(decode("", &bound(None, &[])).is_err());
    }

    /// The version field is the upgrade path, so a token this build does not know
    /// must be refused rather than read for the fields it recognizes.
    #[test]
    fn an_unknown_cursor_version_is_refused() {
        // `{"v":2,"k":["gts.cf.core.example.type.v1~"],"o":"asc","s":"+gts_id","d":"fwd"}`,
        // which `CursorV1` cannot construct — hence the literal.
        const VERSION_2: &str = "eyJ2IjoyLCJrIjpbImd0cy5jZi5jb3JlLmV4YW1wbGUudHlwZS52MX4iXSwibyI6\
                                 ImFzYyIsInMiOiIrZ3RzX2lkIiwiZCI6ImZ3ZCJ9";
        assert!(decode(VERSION_2, &bound(None, &[])).is_err());
    }

    #[test]
    fn a_cursor_with_another_order_or_direction_is_refused() -> Result<(), CanonicalError> {
        let binding = bound(None, &[]);
        for (o, s, d) in [
            (SortDir::Desc, "-gts_id".to_owned(), FORWARD),
            (SortDir::Asc, page_order().to_signed_tokens(), "bwd"),
        ] {
            let token = CursorV1 {
                k: vec![AFTER.to_owned()],
                o,
                s,
                f: binding_hash(&binding),
                d: d.to_owned(),
            }
            .encode()
            .map_err(|e| CanonicalError::internal(e.to_string()).create())?;
            assert!(decode(&token, &binding).is_err(), "{d}");
        }
        Ok(())
    }
}

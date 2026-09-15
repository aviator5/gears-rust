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
//! The query it was issued for. A page is complete with respect to *its* traversal,
//! so replaying one page's position under a different pattern would return a page
//! that is complete for neither — which is why a mismatch is refused rather than
//! reinterpreted. Ordering is fixed (`gts_id` ascending, the keyset the page is
//! built on), so it is bound too and cannot be renegotiated by a caller.
//!
//! [`DiscoveryQuery::after`]: crate::domain::registry_service::DiscoveryQuery::after

use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::pagination::short_filter_hash;
use toolkit_odata::{CursorV1, ODataOrderBy, OrderKey, SortDir, ast, validate_cursor_against};

use super::error::cursor_not_usable;

/// The one keyset column. `gts_id` is unique and immutable, which is what makes the
/// cursor a plain keyset: a page boundary cannot drift or duplicate (§10.2).
const KEY_FIELD: &str = "gts_id";

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

/// The fingerprint of the filter a page was taken under, or `None` for an
/// unfiltered traversal.
///
/// Hashed through `toolkit-odata`'s own normalizer rather than by carrying the
/// pattern verbatim: the cursor is opaque by contract, and a token that spells the
/// query out invites a caller to edit it. The GTS pattern is not an `OData` filter,
/// so it is expressed as the one comparison it is — `gts_id` against the pattern
/// text — purely to reach that normalizer.
fn pattern_hash(pattern: Option<&str>) -> Option<String> {
    let expr = pattern.map(|pattern| {
        ast::Expr::Compare(
            Box::new(ast::Expr::Identifier(KEY_FIELD.to_owned())),
            ast::CompareOperator::Eq,
            Box::new(ast::Expr::Value(ast::Value::String(pattern.to_owned()))),
        )
    });
    short_filter_hash(expr.as_ref())
}

/// Encode the position a page stopped at, bound to the query that produced it.
///
/// # Errors
/// A canonical internal error if the token will not serialize, which is a bug here
/// rather than anything the caller did.
pub fn encode(after: &str, pattern: Option<&str>) -> Result<String, CanonicalError> {
    CursorV1 {
        k: vec![after.to_owned()],
        o: SortDir::Asc,
        s: page_order().to_signed_tokens(),
        f: pattern_hash(pattern),
        d: FORWARD.to_owned(),
    }
    .encode()
    .map_err(|e| {
        tracing::error!(error = %e, "types_registry could not encode a discovery cursor");
        CanonicalError::internal("the registry could not construct a page cursor").create()
    })
}

/// Decode a cursor into the stored `gts_id` the next page resumes after.
///
/// # Errors
/// A `400` problem naming `cursor` when the token is unreadable, of an unsupported
/// version, or bound to a different query than this request asks.
pub fn decode(token: &str, pattern: Option<&str>) -> Result<String, CanonicalError> {
    let cursor = CursorV1::decode(token).map_err(|e| cursor_not_usable(&e.to_string()))?;
    validate_cursor_against(&cursor, &page_order(), pattern_hash(pattern).as_deref())
        .map_err(|e| cursor_not_usable(&e.to_string()))?;
    // `validate_cursor_against` compares filters only when both sides carry one, so
    // the unfiltered/filtered pair is checked here: without this, a cursor from a
    // patternless traversal would be accepted under a pattern and resume at a
    // position that traversal never visited.
    if cursor.f != pattern_hash(pattern) {
        return Err(cursor_not_usable(
            "it was issued for a different pattern than this request names",
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

    #[test]
    fn a_cursor_round_trips_its_position() -> Result<(), CanonicalError> {
        let token = encode(AFTER, None)?;
        assert_eq!(decode(&token, None)?, AFTER);
        let filtered = encode(AFTER, Some(PATTERN))?;
        assert_eq!(decode(&filtered, Some(PATTERN))?, AFTER);
        Ok(())
    }

    /// The token is opaque: the position and the pattern must not be readable off
    /// it, or a caller will start editing one.
    #[test]
    fn the_token_does_not_spell_the_query_out() -> Result<(), CanonicalError> {
        let token = encode(AFTER, Some(PATTERN))?;
        assert!(!token.contains(PATTERN), "{token}");
        assert!(!token.contains("example"), "{token}");
        Ok(())
    }

    #[test]
    fn a_cursor_from_another_pattern_is_refused() -> Result<(), CanonicalError> {
        let token = encode(AFTER, Some(PATTERN))?;
        assert!(decode(&token, Some("gts.cf.other.*")).is_err());
        assert!(
            decode(&token, None).is_err(),
            "a filtered traversal's position is not a patternless traversal's",
        );
        let unfiltered = encode(AFTER, None)?;
        assert!(
            decode(&unfiltered, Some(PATTERN)).is_err(),
            "and the reverse: `validate_cursor_against` alone would accept this",
        );
        Ok(())
    }

    #[test]
    fn an_unreadable_token_is_refused() {
        assert!(decode("not-base64url-json", None).is_err());
        assert!(decode("", None).is_err());
    }

    /// The version field is the upgrade path, so a token this build does not know
    /// must be refused rather than read for the fields it recognizes.
    #[test]
    fn an_unknown_cursor_version_is_refused() {
        // `{"v":2,"k":["gts.cf.core.example.type.v1~"],"o":"asc","s":"+gts_id","d":"fwd"}`,
        // which `CursorV1` cannot construct — hence the literal.
        const VERSION_2: &str = "eyJ2IjoyLCJrIjpbImd0cy5jZi5jb3JlLmV4YW1wbGUudHlwZS52MX4iXSwibyI6\
                                 ImFzYyIsInMiOiIrZ3RzX2lkIiwiZCI6ImZ3ZCJ9";
        assert!(decode(VERSION_2, None).is_err());
    }

    /// A cursor taken under a different sort order describes a different traversal.
    #[test]
    fn a_cursor_with_another_order_is_refused() -> Result<(), serde_json::Error> {
        let token = CursorV1 {
            k: vec![AFTER.to_owned()],
            o: SortDir::Desc,
            s: "-gts_id".to_owned(),
            f: None,
            d: FORWARD.to_owned(),
        }
        .encode()?;
        assert!(decode(&token, None).is_err());
        Ok(())
    }

    #[test]
    fn a_backward_cursor_is_refused() -> Result<(), serde_json::Error> {
        let token = CursorV1 {
            k: vec![AFTER.to_owned()],
            o: SortDir::Asc,
            s: page_order().to_signed_tokens(),
            f: None,
            d: "bwd".to_owned(),
        }
        .encode()?;
        assert!(decode(&token, None).is_err());
        Ok(())
    }
}

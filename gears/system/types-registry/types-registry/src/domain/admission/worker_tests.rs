//! `reason_label`: the closed-vocabulary bridge between a stored failure and the
//! refusal counter's `reason` label — a pure function, pinned here so the
//! `other` fallback cannot be "simplified" into an unbounded label or dropped
//! without a test noticing.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::borrow::Cow;

use super::{ItemFailure, reason_label};

/// A fresh refusal site names its reason as a `&'static str` literal, and a
/// literal is its own label — every series a dashboard knows stays exactly where
/// the site put it.
#[test]
fn a_borrowed_literal_reason_is_its_own_label() {
    for literal in [
        "precondition_failed",
        "dependent_invalid",
        "revalidation_exhausted",
        "activation_write_set_exceeded",
    ] {
        assert_eq!(reason_label(&Cow::Borrowed(literal)), literal);
    }
}

/// The one producer of an owned reason is [`ItemFailure::from_payload`] — a
/// failure read back off a stored `error_payload`, whose reason was a literal in
/// some *earlier* process. It is only *probably* still in the vocabulary, so it
/// counts under the single closed fallback rather than becoming a series of its
/// own.
#[test]
fn an_owned_reason_counts_under_the_closed_other_label() {
    let failure = ItemFailure::from_payload(
        r#"{"reason":"precondition_failed","message":"read back off a stored row"}"#,
    );
    assert!(
        matches!(failure.reason, Cow::Owned(_)),
        "from_payload is the owned-reason producer the mapping exists for"
    );
    assert_eq!(reason_label(&failure.reason), "other");
}

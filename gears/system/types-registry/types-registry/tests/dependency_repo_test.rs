//! The reverse-impact read (T14): everything that transitively depends on a
//! revised entity, in one recursive CTE.
//!
//! `repo_test.rs` covers the *forward* closure. This suite is about the other
//! direction and the two properties that make it safe to commit against:
//!
//! * **No silent truncation.** The CTE carries a mandatory depth cap, so a walk
//!   deeper than the cap returns a *subset* with no error of its own — and a
//!   missed dependent means stale artifacts committed as current. The cap is set
//!   to the write-set bound, which makes truncation provably impossible below the
//!   bound: a hidden node's shortest path would have to pass through more distinct
//!   dependents than the bound admits, so the bound refusal fires first. These
//!   tests pin the refusal, not the truncation.
//! * **A row contradicting acyclicity still terminates.** The relation is acyclic by
//!   construction (ADR-0012) — that invariant is pinned in `dependency_test.rs`,
//!   where it is enforced — so the cycle here is written straight through the
//!   repository. `UNION` plus the depth cap is what keeps such a row from hanging
//!   the commit transaction this read runs inside.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]

mod common;

use std::sync::Arc;

use time::OffsetDateTime;
use time::macros::datetime;
use toolkit_db::{DBProvider, DbError};
use toolkit_gts::gts_id;
use uuid::Uuid;

use common::{allow_all, test_db};
use types_registry::domain::enums::{DependencyKind, EntityKind, OwnershipScope};
use types_registry::domain::ports::{NewEntity, ReverseImpact};
use types_registry::infra::storage::repo::{DependencyRepo, EntityRepo, VersionFamilyRepo};

const NOW: OffsetDateTime = datetime!(2026-08-18 09:15:30 UTC);
const FAMILY_KEY: &str = "gts.acme.rev.thing.type";

/// The bound every test that is not *about* the bound passes: comfortably above
/// the handful of rows these fixtures create.
const BOUND: usize = 512;

type Provider = Arc<DBProvider<DbError>>;

fn new_entity(gts_id: &str, family_id: i64) -> NewEntity {
    NewEntity {
        gts_uuid: Uuid::new_v5(&Uuid::NAMESPACE_URL, gts_id.as_bytes()),
        gts_id: gts_id.to_owned(),
        entity_kind: EntityKind::TypeSchema,
        family_id,
        ownership_scope: OwnershipScope::Global,
        owner_tenant_id: None,
        owning_gear: Some("types-registry".to_owned()),
        now: NOW,
    }
}

/// Seed one global family plus the given identifiers, and return their entity ids
/// in the order the identifiers were given.
async fn seed(db: &Provider, ids: &[&str]) -> Vec<i64> {
    let conn = db.conn().expect("conn");
    let scope = allow_all();
    let (family, _) = VersionFamilyRepo::create_or_get(
        &conn,
        &scope,
        FAMILY_KEY,
        OwnershipScope::Global,
        None,
        NOW,
    )
    .await
    .expect("family");

    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let row = EntityRepo::insert(&conn, &scope, new_entity(id, family.id))
            .await
            .unwrap_or_else(|e| panic!("insert {id}: {e}"))
            .unwrap_or_else(|| panic!("{id} was already held"));
        out.push(row.id);
    }
    out
}

/// `from` depends on `to`, so reverse impact travels `to` → `from`.
async fn edge(db: &Provider, from: i64, to: i64) {
    let conn = db.conn().expect("conn");
    DependencyRepo::replace_outgoing(
        &conn,
        &allow_all(),
        from,
        &[(DependencyKind::SchemaRef, to)],
    )
    .await
    .expect("edge");
}

/// The identifiers of a reverse-impact read, sorted — the read's own order is
/// `gts_id`, and asserting on the sorted list keeps a test from depending on it
/// twice.
async fn impact(db: &Provider, roots: &[i64], bound: usize) -> Vec<String> {
    let conn = db.conn().expect("conn");
    match DependencyRepo::reverse_impact(&conn, &allow_all(), roots, bound)
        .await
        .expect("reverse impact")
    {
        ReverseImpact::Within(rows) => rows.into_iter().map(|r| r.gts_id).collect(),
        ReverseImpact::OverBound { at_least, bound } => {
            panic!("unexpected refusal: {at_least} dependents against a bound of {bound}")
        }
    }
}

/// The bound refusal, or a panic naming what came back instead. A helper because
/// the claim under test is the *variant*, not a message: the wording belongs to the
/// candidate refusal the domain writes, and the structured warning to the log.
async fn over_bound(db: &Provider, roots: &[i64], bound: usize) -> usize {
    let conn = db.conn().expect("conn");
    match DependencyRepo::reverse_impact(&conn, &allow_all(), roots, bound)
        .await
        .expect("reverse impact")
    {
        ReverseImpact::OverBound { at_least, .. } => at_least,
        ReverseImpact::Within(rows) => panic!(
            "expected a refusal over the bound of {bound}, got {} dependents",
            rows.len()
        ),
    }
}

// ---------------------------------------------------------------------------
// what the walk reaches
// ---------------------------------------------------------------------------

/// The whole reverse chain, not only the first hop: revising `country` changes
/// `address`'s effective artifacts, which changes `customer`'s.
#[tokio::test]
async fn reverse_impact_reaches_direct_and_transitive_dependents() {
    let db = test_db().await;
    let customer = gts_id!("acme.rev.customer.type.v1~");
    let address = gts_id!("acme.rev.address.type.v1~");
    let country = gts_id!("acme.rev.country.type.v1~");
    let outside = gts_id!("acme.rev.unrelated.type.v1~");
    let ids = seed(&db, &[customer, address, country, outside]).await;

    edge(&db, ids[0], ids[1]).await;
    edge(&db, ids[1], ids[2]).await;

    let got = impact(&db, &[ids[2]], BOUND).await;
    assert_eq!(
        got,
        vec![address.to_owned(), customer.to_owned()],
        "both the direct dependent and the one behind it, gts_id-sorted, and \
         nothing that depends on neither"
    );
}

/// The root is the candidate being committed; it is refreshed by the commit
/// itself, so returning it would make the write set count it twice.
#[tokio::test]
async fn reverse_impact_excludes_the_roots_themselves() {
    let db = test_db().await;
    let base = gts_id!("acme.rev.base.type.v1~");
    let leaf = gts_id!("acme.rev.leaf.type.v1~");
    let ids = seed(&db, &[base, leaf]).await;
    edge(&db, ids[1], ids[0]).await;

    let got = impact(&db, &[ids[0]], BOUND).await;
    assert_eq!(got, vec![leaf.to_owned()]);
}

/// Defence in depth: admission cannot produce this graph (ADR-0012), so the cycle is
/// written through the repository. `UNION` plus the depth cap is what terminates the
/// walk, and the root stays excluded even though the cycle leads back to it.
#[tokio::test]
async fn reverse_impact_terminates_on_a_row_that_contradicts_acyclicity() {
    let db = test_db().await;
    let a = gts_id!("acme.rev.a.type.v1~");
    let b = gts_id!("acme.rev.b.type.v1~");
    let ids = seed(&db, &[a, b]).await;
    edge(&db, ids[0], ids[1]).await;
    edge(&db, ids[1], ids[0]).await;

    let got = impact(&db, &[ids[0]], BOUND).await;
    assert_eq!(
        got,
        vec![b.to_owned()],
        "the other member once, and not the root"
    );
}

#[tokio::test]
async fn reverse_impact_of_an_entity_nothing_depends_on_is_empty() {
    let db = test_db().await;
    let lonely = gts_id!("acme.rev.lonely.type.v1~");
    let ids = seed(&db, &[lonely]).await;

    assert!(impact(&db, &ids, BOUND).await.is_empty());
}

/// Every edge kind propagates: a derived type's resolution inlines its base, and
/// so does a `$ref`. The traversal is over rows, so it must not filter on kind.
#[tokio::test]
async fn reverse_impact_follows_every_edge_kind() {
    let db = test_db().await;
    let base = gts_id!("acme.rev.kinds.type.v1~");
    let derived = gts_id!("acme.rev.kinds.type.v1~acme.rev.d.type.v1~");
    let referrer = gts_id!("acme.rev.referrer.type.v1~");
    let ids = seed(&db, &[base, derived, referrer]).await;

    let conn = db.conn().expect("conn");
    DependencyRepo::replace_outgoing(
        &conn,
        &allow_all(),
        ids[1],
        &[(DependencyKind::Derivation, ids[0])],
    )
    .await
    .expect("derivation edge");
    DependencyRepo::replace_outgoing(
        &conn,
        &allow_all(),
        ids[2],
        &[(DependencyKind::SchemaRef, ids[0])],
    )
    .await
    .expect("schema-ref edge");

    let got = impact(&db, &[ids[0]], BOUND).await;
    assert_eq!(got, vec![derived.to_owned(), referrer.to_owned()]);
}

// ---------------------------------------------------------------------------
// the bound
// ---------------------------------------------------------------------------

/// Over the bound is a refusal, not a truncated answer: the caller is about to
/// write to every row this read returns.
#[tokio::test]
async fn reverse_impact_refuses_a_set_over_the_bound() {
    let db = test_db().await;
    let base = gts_id!("acme.rev.hub.type.v1~");
    let first = gts_id!("acme.rev.first.type.v1~");
    let second = gts_id!("acme.rev.second.type.v1~");
    let ids = seed(&db, &[base, first, second]).await;
    edge(&db, ids[1], ids[0]).await;
    edge(&db, ids[2], ids[0]).await;

    assert!(
        over_bound(&db, &[ids[0]], 1).await > 1,
        "the read counted past the bound before it stopped"
    );
}

/// The depth cap is the write-set bound, so a chain deeper than the bound cannot
/// come back quietly shortened — the same refusal fires. This is the property that
/// makes a depth-capped CTE safe to commit against.
#[tokio::test]
async fn reverse_impact_refuses_rather_than_truncating_a_chain_deeper_than_the_bound() {
    let db = test_db().await;
    let chain = [
        gts_id!("acme.rev.d0.type.v1~"),
        gts_id!("acme.rev.d1.type.v1~"),
        gts_id!("acme.rev.d2.type.v1~"),
        gts_id!("acme.rev.d3.type.v1~"),
    ];
    let ids = seed(&db, &chain).await;
    // d3 -> d2 -> d1 -> d0, so the reverse impact of d0 is the other three.
    edge(&db, ids[1], ids[0]).await;
    edge(&db, ids[2], ids[1]).await;
    edge(&db, ids[3], ids[2]).await;

    assert!(
        over_bound(&db, &[ids[0]], 2).await > 2,
        "a chain deeper than the cap is refused, never shortened"
    );

    // …and with a bound that admits them, all three come back: the cap did not
    // quietly cut the third hop off.
    assert_eq!(
        impact(&db, &[ids[0]], 3).await,
        vec![
            chain[1].to_owned(),
            chain[2].to_owned(),
            chain[3].to_owned()
        ],
        "every hop of the chain, with no hop lost to the depth cap"
    );
}

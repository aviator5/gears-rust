use serde_json::json;

use super::*;

fn fields() -> Vec<String> {
    vec!["$id".to_owned(), "gtsId".to_owned(), "id".to_owned()]
}

#[test]
fn collect_identifies_schemas_by_uri_and_instances_by_id() {
    let set = SeedSet::collect(
        [
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "type": "object"}),
            json!({"id": "gts.cf.core.seed.base.v1~cf.core.seed.one.v1"}),
        ],
        &fields(),
    )
    .expect("a valid seed set");

    let ids: Vec<&str> = set.documents.keys().map(String::as_str).collect();
    assert_eq!(
        ids,
        [
            "gts.cf.core.seed.base.v1~",
            "gts.cf.core.seed.base.v1~cf.core.seed.one.v1"
        ]
    );
}

#[test]
fn collect_collapses_identical_duplicates() {
    let doc = json!({"$id": "gts://gts.cf.core.seed.base.v1~"});
    let set = SeedSet::collect([doc.clone(), doc], &fields()).expect("identical duplicates");
    assert_eq!(set.len(), 1);
}

#[test]
fn collect_refuses_differing_duplicates_by_identifier() {
    let error = SeedSet::collect(
        [
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "description": "a"}),
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "description": "b"}),
        ],
        &fields(),
    )
    .expect_err("two documents for one identifier");
    assert!(
        matches!(&error, SeedError::ConflictingDeclarations { gts_id } if gts_id == "gts.cf.core.seed.base.v1~"),
        "{error}"
    );
}

#[test]
fn collect_refuses_a_document_without_an_identifier() {
    let error =
        SeedSet::collect([json!({"type": "object"})], &fields()).expect_err("no identifier field");
    assert!(
        matches!(error, SeedError::MissingIdentifier { index: 0, .. }),
        "{error}"
    );
}

#[test]
fn compare_submits_absent_differing_and_deleted_seeds_only() {
    let set = SeedSet::collect(
        [
            json!({"$id": "gts://gts.cf.core.seed.absent.v1~"}),
            json!({"$id": "gts://gts.cf.core.seed.equal.v1~"}),
            json!({"$id": "gts://gts.cf.core.seed.changed.v1~", "description": "new"}),
            json!({"$id": "gts://gts.cf.core.seed.deleted.v1~"}),
        ],
        &fields(),
    )
    .expect("a valid seed set");
    let stored = |id: &str, content: Value, lifecycle| {
        (
            id.to_owned(),
            Current {
                content: canonical_text(&content),
                resource_version: 3,
                lifecycle,
            },
        )
    };
    let current = BTreeMap::from([
        stored(
            "gts.cf.core.seed.equal.v1~",
            json!({"$id": "gts://gts.cf.core.seed.equal.v1~"}),
            LifecycleStatus::Active,
        ),
        stored(
            "gts.cf.core.seed.changed.v1~",
            json!({"$id": "gts://gts.cf.core.seed.changed.v1~", "description": "old"}),
            LifecycleStatus::Active,
        ),
        stored(
            "gts.cf.core.seed.deleted.v1~",
            json!({"$id": "gts://gts.cf.core.seed.deleted.v1~"}),
            LifecycleStatus::Deleted,
        ),
    ]);

    let candidates: Vec<(String, Option<i64>)> = compare(&set, &current)
        .into_iter()
        .map(|c| (c.gts_id, c.expected_resource_version))
        .collect();

    assert_eq!(
        candidates,
        [
            ("gts.cf.core.seed.absent.v1~".to_owned(), None),
            ("gts.cf.core.seed.changed.v1~".to_owned(), Some(3)),
            ("gts.cf.core.seed.deleted.v1~".to_owned(), Some(3)),
        ]
    );
}

#[test]
fn collect_refuses_duplicates_differing_only_in_a_signed_zero() {
    // `Value` equality calls them equal; the canonical text acceptance stores does not.
    let error = SeedSet::collect(
        [
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "default": 0.0}),
            json!({"$id": "gts://gts.cf.core.seed.base.v1~", "default": -0.0}),
        ],
        &fields(),
    )
    .expect_err("byte-distinct declarations of one identifier");
    assert!(
        matches!(error, SeedError::ConflictingDeclarations { .. }),
        "{error}"
    );
}

#[test]
fn compare_submits_a_seed_differing_only_in_a_signed_zero() {
    let id = "gts.cf.core.seed.zero.v1~";
    let set = SeedSet::collect(
        [json!({"$id": format!("gts://{id}"), "default": -0.0})],
        &fields(),
    )
    .expect("a valid seed set");
    let current = BTreeMap::from([(
        id.to_owned(),
        Current {
            content: canonical_text(&json!({"$id": format!("gts://{id}"), "default": 0.0})),
            resource_version: 1,
            lifecycle: LifecycleStatus::Active,
        },
    )]);

    let candidates = compare(&set, &current);

    assert_eq!(candidates.len(), 1, "-0.0 is not the stored 0.0");
    assert_eq!(candidates[0].expected_resource_version, Some(1));
}

#[tokio::test]
async fn an_exhausted_or_cancelled_budget_refuses_success() {
    let cancel = CancellationToken::new();
    let expired = Budget {
        deadline: Instant::now(),
        cancel: &cancel,
    };
    assert!(matches!(expired.check(), Err(Stop::Deadline)));

    let live = Budget {
        deadline: Instant::now() + Duration::from_secs(60),
        cancel: &cancel,
    };
    assert!(live.check().is_ok());
    cancel.cancel();
    assert!(matches!(live.check(), Err(Stop::Cancelled)));
}

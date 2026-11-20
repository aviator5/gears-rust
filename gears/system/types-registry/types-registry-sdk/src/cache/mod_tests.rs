use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use uuid::Uuid;

use super::{CacheSettings, CachedPlatformTypesRegistry};
use crate::contract::PlatformTypesRegistryApi;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, DeleteEntitiesRequest,
    DeletionOperation, EntityField, EntityKey, EntityLookup, FieldSelection, IdempotencyKey,
    ListEntitiesRequest, ListEntitiesResponse, Operation, OperationStatus, Projection,
    PublisherContext, RegisterEntitiesRequest, RegisterItem, RegistrationOperation, Validator,
};
use crate::testing_platform::{Call, Fault, MockTypesRegistry, Seed};

const TYPE: &str = "gts.cf.cache.client.entity.v1~";
const OTHER: &str = "gts.cf.cache.client.other.v1~";
const THIRD: &str = "gts.cf.cache.client.third.v1~";
const WINDOW: Duration = Duration::from_secs(30);

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn gts_key(id: &str) -> EntityKey {
    EntityKey::GtsId(gts::GtsId::try_new(id).expect("valid GTS identifier"))
}

fn uuid_key(id: &str) -> EntityKey {
    EntityKey::GtsUuid(
        gts::GtsId::try_new(id)
            .expect("valid GTS identifier")
            .to_uuid(),
    )
}

fn schema(id: &str, title: &str) -> serde_json::Value {
    json!({
        "$id": format!("gts://{id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "title": title,
    })
}

fn full() -> Projection {
    Projection::Select(FieldSelection::full())
}

fn content_only() -> Projection {
    Projection::Select(FieldSelection::with(&[EntityField::Content]))
}

/// A registry change the client does not observe: a new revision stored directly.
fn change(registry: &MockTypesRegistry, id: &str, title: &str, version: u64) {
    registry.seed_entity(Seed::new(id, schema(id, title)).version(version));
}

/// What the registry answered, per key: whether a snapshot crossed the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Found,
    Unchanged,
    NotFound,
}

/// The registry behind the cache, recording each batch read and its answers.
type Batch = Vec<(BatchGetItem, Answer)>;

/// Rewrites the next registry answer before the cache sees it.
type Tamper = Box<dyn FnOnce(&mut BatchGetEntitiesResponse) + Send>;

struct Recorder {
    registry: Arc<MockTypesRegistry>,
    batches: Mutex<Vec<Batch>>,
    tamper: Mutex<Option<Tamper>>,
}

impl Recorder {
    fn batches(&self) -> Vec<Batch> {
        self.batches.lock().expect("lock").clone()
    }

    fn batch_count(&self) -> usize {
        self.batches.lock().expect("lock").len()
    }

    fn tamper_next(&self, tamper: impl FnOnce(&mut BatchGetEntitiesResponse) + Send + 'static) {
        *self.tamper.lock().expect("lock") = Some(Box::new(tamper));
    }
}

#[async_trait]
impl PlatformTypesRegistryApi for Recorder {
    async fn batch_get_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let items = request.items.clone();
        let mut response = self.registry.batch_get_entities(ctx, request).await?;
        if let Some(tamper) = self.tamper.lock().expect("lock").take() {
            tamper(&mut response);
        }
        let answered = items
            .into_iter()
            .map(|item| {
                let answer = match response.0.get(&item.key) {
                    Some(EntityLookup::Found { .. }) => Answer::Found,
                    Some(EntityLookup::Unchanged { .. }) => Answer::Unchanged,
                    _ => Answer::NotFound,
                };
                (item, answer)
            })
            .collect();
        self.batches.lock().expect("lock").push(answered);
        Ok(response)
    }

    async fn list_entities(
        &self,
        ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.registry.list_entities(ctx, request).await
    }

    async fn register_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        self.registry.register_entities(ctx, key, request).await
    }

    async fn delete_entities(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        self.registry.delete_entities(ctx, key, request).await
    }

    async fn get_operation(
        &self,
        ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        self.registry.get_operation(ctx, operation_id).await
    }
}

struct Fixture {
    registry: Arc<MockTypesRegistry>,
    recorder: Arc<Recorder>,
    cache: CachedPlatformTypesRegistry,
}

fn fixture(window: Duration) -> Fixture {
    fixture_over(MockTypesRegistry::new(), window)
}

fn fixture_over(registry: MockTypesRegistry, window: Duration) -> Fixture {
    let registry = Arc::new(registry);
    for id in [TYPE, OTHER, THIRD] {
        registry.seed(id, schema(id, "v1"));
    }
    let recorder = Arc::new(Recorder {
        registry: Arc::clone(&registry),
        batches: Mutex::new(Vec::new()),
        tamper: Mutex::new(None),
    });
    let cache = CachedPlatformTypesRegistry::new(
        Arc::clone(&recorder) as Arc<dyn PlatformTypesRegistryApi>,
        CacheSettings {
            freshness_window: window,
            store_bound: CacheSettings::DEFAULT_STORE_BOUND,
        },
    );
    Fixture {
        registry,
        recorder,
        cache,
    }
}

impl Fixture {
    async fn read_with(
        &self,
        keys: Vec<BatchGetItem>,
        projection: Projection,
        fresh: bool,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let request = BatchGetEntitiesRequest {
            items: keys,
            projection,
            fresh,
        };
        self.cache.batch_get_entities(&ctx(), request).await
    }

    async fn lookup(&self, key: EntityKey, projection: Projection, fresh: bool) -> EntityLookup {
        let mut response = self
            .read_with(vec![key.clone().into()], projection, fresh)
            .await
            .expect("read");
        response.0.remove(&key).expect("answered")
    }

    /// The title of `key`'s content under `projection`.
    async fn title(&self, key: EntityKey, projection: Projection, fresh: bool) -> String {
        match self.lookup(key, projection, fresh).await {
            EntityLookup::Found { entity, .. } => entity
                .content
                .as_ref()
                .and_then(|c| c["title"].as_str())
                .expect("content selected")
                .to_owned(),
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn inside_the_window_a_read_is_cached_and_fresh_bypasses_it() {
    let f = fixture(WINDOW);
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v1");
    change(&f.registry, TYPE, "v2", 2);

    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v1",
        "inside the window the unobserved change is not seen"
    );
    assert_eq!(f.recorder.batch_count(), 1, "served without the registry");
    assert_eq!(f.title(gts_key(TYPE), full(), true).await, "v2");
    let fresh = &f.recorder.batches()[1];
    assert!(
        fresh[0].0.if_none_match.is_some(),
        "fresh revalidates against the entry's validator"
    );
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v2",
        "the fresh answer replaced the entry"
    );
    assert_eq!(f.recorder.batch_count(), 2);
}

#[tokio::test(start_paused = true)]
async fn after_the_window_a_changed_entity_is_read_anew() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    change(&f.registry, TYPE, "v2", 2);
    tokio::time::advance(WINDOW).await;
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
}

#[tokio::test(start_paused = true)]
async fn a_zero_window_never_serves_a_cached_entry_and_moves_no_unchanged_document() {
    let f = fixture(Duration::ZERO);
    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(TYPE), full(), false).await;
    let batches = f.recorder.batches();
    assert_eq!(batches.len(), 2, "every read asks the registry");
    assert_eq!(
        batches[1][0].1,
        Answer::Unchanged,
        "no document transferred"
    );

    change(&f.registry, TYPE, "v2", 2);
    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v2");
}

#[tokio::test(start_paused = true)]
async fn an_expired_unchanged_entry_is_kept_and_its_window_restarts() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    tokio::time::advance(WINDOW + Duration::from_secs(1)).await;

    assert_eq!(f.title(gts_key(TYPE), full(), false).await, "v1");
    let revalidation = &f.recorder.batches()[1];
    assert!(revalidation[0].0.if_none_match.is_some());
    assert_eq!(
        revalidation[0].1,
        Answer::Unchanged,
        "kept, not refetched: the registry sent no snapshot"
    );

    tokio::time::advance(WINDOW.saturating_sub(Duration::from_secs(1))).await;
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "the confirmation restarted the window"
    );
}

#[tokio::test(start_paused = true)]
async fn one_conditional_batch_carries_every_expired_key_and_only_those_validators() {
    let f = fixture(WINDOW);
    f.read_with(
        vec![gts_key(TYPE).into(), gts_key(OTHER).into()],
        full(),
        false,
    )
    .await
    .expect("warm");
    tokio::time::advance(WINDOW).await;
    // THIRD is warmed after expiry, so it is still fresh below.
    f.lookup(gts_key(THIRD), full(), false).await;

    let missing = "gts.cf.cache.client.missing.v1~";
    let response = f
        .read_with(
            vec![
                gts_key(TYPE).into(),
                gts_key(OTHER).into(),
                gts_key(THIRD).into(),
                gts_key(missing).into(),
            ],
            full(),
            false,
        )
        .await
        .expect("read");

    let batches = f.recorder.batches();
    assert_eq!(batches.len(), 3, "one registry call for the whole batch");
    let sent = &batches[2];
    let conditional: Vec<_> = sent
        .iter()
        .filter(|(item, _)| item.if_none_match.is_some())
        .map(|(item, _)| item.key.clone())
        .collect();
    assert_eq!(conditional, vec![gts_key(TYPE), gts_key(OTHER)]);
    assert!(
        sent.iter().all(|(item, _)| item.key != gts_key(THIRD)),
        "a fresh entry is answered locally"
    );
    assert!(
        sent.iter()
            .any(|(item, _)| item.key == gts_key(missing) && item.if_none_match.is_none()),
        "an uncached key is read unconditionally"
    );
    assert_eq!(response.0.len(), 4);
    assert_eq!(response.0[&gts_key(missing)], EntityLookup::NotFound);
}

#[tokio::test(start_paused = true)]
async fn a_failed_revalidation_propagates_and_never_extends_the_window() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    tokio::time::advance(WINDOW).await;
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .fail(CanonicalError::service_unavailable().create()),
    );

    let error = f
        .read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("the registry could not answer");
    assert!(
        matches!(error, CanonicalError::ServiceUnavailable { .. }),
        "{error:?}"
    );

    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.registry.calls(Call::BatchGet),
        3,
        "the expired entry was not served after the failure"
    );
}

#[tokio::test(start_paused = true)]
async fn a_failed_read_leaves_no_entry() {
    let f = fixture(WINDOW);
    f.registry.inject(
        Fault::on(Call::BatchGet)
            .nth(1)
            .fail(CanonicalError::service_unavailable().create()),
    );
    f.read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("injected");
    f.title(gts_key(TYPE), full(), false).await;
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.registry.calls(Call::BatchGet),
        2,
        "the second read filled it"
    );
}

#[tokio::test(start_paused = true)]
async fn not_found_is_never_cached() {
    let f = fixture(WINDOW);
    let late = "gts.cf.cache.client.late.v1~";
    assert_eq!(
        f.lookup(gts_key(late), full(), false).await,
        EntityLookup::NotFound
    );
    f.registry.seed(late, schema(late, "v1"));
    assert_eq!(f.title(gts_key(late), full(), false).await, "v1");
}

#[tokio::test(start_paused = true)]
async fn a_discovery_page_and_its_items_are_never_cached() {
    let f = fixture(WINDOW);
    let page = f
        .cache
        .list_entities(&ctx(), ListEntitiesRequest::default())
        .await
        .expect("page");
    assert!(!page.items.is_empty());
    f.cache
        .list_entities(&ctx(), ListEntitiesRequest::default())
        .await
        .expect("page");
    assert_eq!(f.registry.calls(Call::List), 2, "the page is not cached");

    f.lookup(gts_key(TYPE), Projection::Default, false).await;
    assert_eq!(
        f.recorder.batch_count(),
        1,
        "a page item does not prime exact reads"
    );
}

#[tokio::test(start_paused = true)]
async fn an_operation_resource_is_never_cached() {
    let f = fixture_over(MockTypesRegistry::new().completing_after(2), WINDOW);
    let new = "gts.cf.cache.client.new.v1~";
    let submitted = f
        .cache
        .register_entities(
            &ctx(),
            IdempotencyKey::new("cache-operation").expect("key"),
            RegisterEntitiesRequest {
                items: vec![RegisterItem {
                    gts_id: gts::GtsId::try_new(new).expect("identifier"),
                    content: schema(new, "v1"),
                    expected_resource_version: None,
                    skip_compatibility_check: false,
                }],
                dry_run: false,
                publisher: PublisherContext {
                    name: "cache-test".to_owned(),
                    version: "1.0.0".parse().expect("version"),
                },
            },
        )
        .await
        .expect("accepted");
    let first = f
        .cache
        .get_operation(&ctx(), submitted.operation_id)
        .await
        .expect("poll");
    let second = f
        .cache
        .get_operation(&ctx(), submitted.operation_id)
        .await
        .expect("poll");
    assert_ne!(first.status(), OperationStatus::Completed);
    assert_eq!(
        second.status(),
        OperationStatus::Completed,
        "not a cached poll"
    );
    assert_eq!(f.registry.calls(Call::GetOperation), 2);
}

#[tokio::test(start_paused = true)]
async fn either_key_form_hits_the_same_snapshot() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(f.title(uuid_key(TYPE), full(), false).await, "v1");
    assert_eq!(f.recorder.batch_count(), 1);
}

#[tokio::test(start_paused = true)]
async fn projections_are_distinct_entries_and_equivalent_selections_share_one() {
    let f = fixture(WINDOW);
    f.lookup(gts_key(TYPE), Projection::Default, false).await;
    let explicit_default = Projection::Select(FieldSelection::with(&EntityField::DEFAULT));
    f.lookup(gts_key(TYPE), explicit_default, false).await;
    assert_eq!(
        f.recorder.batch_count(),
        1,
        "an explicit default reuses Default"
    );

    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "a narrow entry never answers a wider selection"
    );
}

#[tokio::test(start_paused = true)]
async fn a_new_revision_seen_under_one_projection_drops_the_others() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), content_only(), false).await;
    f.title(gts_key(TYPE), full(), false).await;
    change(&f.registry, TYPE, "v2", 2);

    assert_eq!(f.title(gts_key(TYPE), content_only(), true).await, "v2");
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v2",
        "the other projection was invalidated, not served for the rest of its window"
    );
}

#[tokio::test(start_paused = true)]
async fn a_caller_validator_is_answered_from_the_current_representation() {
    let f = fixture(WINDOW);
    let EntityLookup::Found { etag, .. } = f.lookup(gts_key(TYPE), full(), false).await else {
        panic!("found");
    };
    let conditional = |etag: Validator| BatchGetItem::new(gts_key(TYPE)).with_if_none_match(etag);

    let response = f
        .read_with(vec![conditional(etag.clone())], full(), false)
        .await
        .expect("read");
    assert_eq!(
        response.0[&gts_key(TYPE)],
        EntityLookup::Unchanged { etag: etag.clone() }
    );
    let response = f
        .read_with(
            vec![conditional(Validator::from_bytes(b"stale".to_vec()))],
            full(),
            false,
        )
        .await
        .expect("read");
    assert!(matches!(
        response.0[&gts_key(TYPE)],
        EntityLookup::Found { .. }
    ));
    assert_eq!(f.recorder.batch_count(), 1, "both answered from the cache");
}

#[tokio::test(start_paused = true)]
async fn an_unconditional_fresh_read_of_an_unchanged_entry_still_gets_the_snapshot() {
    // Reconciliation reads `fresh` and treats `Unchanged` as a protocol fault.
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(f.title(gts_key(TYPE), full(), true).await, "v1");
    assert_eq!(f.recorder.batches()[1][0].1, Answer::Unchanged);
}

#[tokio::test(start_paused = true)]
async fn an_out_of_range_batch_is_refused_even_when_every_key_is_cached() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    let error = f
        .read_with(vec![], full(), false)
        .await
        .expect_err("empty batch");
    assert!(
        matches!(error, CanonicalError::InvalidArgument { .. }),
        "{error:?}"
    );
    let oversized = vec![BatchGetItem::new(gts_key(TYPE)); crate::models::MAX_BATCH_GET_KEYS + 1];
    f.read_with(oversized, full(), false)
        .await
        .expect_err("oversized batch");
}

fn oversized() -> Validator {
    Validator::from_bytes(vec![b'a'; 1025])
}

/// The request the registry last received carried `item` exactly.
fn forwarded(f: &Fixture, item: &BatchGetItem) -> bool {
    f.recorder
        .batches()
        .last()
        .is_some_and(|batch| batch.iter().any(|(sent, _)| sent == item))
}

#[tokio::test(start_paused = true)]
async fn an_oversized_caller_validator_reaches_the_registry_intact() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    let bad = BatchGetItem::new(gts_key(TYPE)).with_if_none_match(oversized());

    // Warm: a hit would have hidden it.
    drop(f.read_with(vec![bad.clone()], full(), false).await);
    assert!(forwarded(&f, &bad), "not answered from the cache");

    // Expired: the cache's validator would have replaced it.
    tokio::time::advance(WINDOW).await;
    drop(f.read_with(vec![bad.clone()], full(), false).await);
    assert!(forwarded(&f, &bad), "not replaced by the cache's validator");

    // Repeated: deduplication would have dropped the second mention.
    drop(
        f.read_with(
            vec![
                gts_key(OTHER).into(),
                BatchGetItem::new(gts_key(OTHER)).with_if_none_match(oversized()),
            ],
            full(),
            false,
        )
        .await,
    );
    assert!(forwarded(
        &f,
        &BatchGetItem::new(gts_key(OTHER)).with_if_none_match(oversized())
    ));
}

#[tokio::test(start_paused = true)]
async fn a_partial_answer_fails_the_read_and_caches_none_of_it() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        response.0.remove(&gts_key(OTHER));
    });
    let error = f
        .read_with(
            vec![gts_key(TYPE).into(), gts_key(OTHER).into()],
            full(),
            false,
        )
        .await
        .expect_err("an answer is missing");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );

    f.title(gts_key(TYPE), full(), false).await;
    assert_eq!(
        f.recorder.batch_count(),
        2,
        "the answered key was not cached either"
    );
}

#[tokio::test(start_paused = true)]
async fn an_unchanged_answer_to_a_validator_nobody_sent_fails_the_read() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        response.0.insert(
            gts_key(TYPE),
            EntityLookup::Unchanged {
                etag: Validator::from_bytes(b"forged".to_vec()),
            },
        );
    });
    let error = f
        .read_with(vec![gts_key(TYPE).into()], full(), true)
        .await
        .expect_err("unsolicited unchanged");
    assert!(
        matches!(error, CanonicalError::Internal { .. }),
        "{error:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_mislabelled_snapshot_fails_the_read_and_is_not_cached() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        if let Some(EntityLookup::Found { entity, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            entity.kind = crate::models::EntityKind::Instance;
        }
    });
    f.read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("inconsistent kind");
    let healthy = f.lookup(gts_key(TYPE), full(), false).await;
    assert!(
        matches!(&healthy, EntityLookup::Found { entity, .. } if entity.kind == crate::models::EntityKind::TypeSchema),
        "{healthy:?}"
    );
    assert_eq!(f.recorder.batch_count(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_validator_over_the_quoted_bound_reaches_the_registry_intact() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    // 1023 bytes fit the local bound but not REST's quoted one.
    let bad = BatchGetItem::new(gts_key(TYPE))
        .with_if_none_match(Validator::from_bytes(vec![b'a'; 1023]));
    drop(f.read_with(vec![bad.clone()], full(), false).await);
    assert!(forwarded(&f, &bad), "not answered from the cache");

    let fits = BatchGetItem::new(gts_key(TYPE))
        .with_if_none_match(Validator::from_bytes(vec![b'a'; 1022]));
    let batches = f.recorder.batch_count();
    f.read_with(vec![fits], full(), false).await.expect("read");
    assert_eq!(
        f.recorder.batch_count(),
        batches,
        "a fitting one is served locally"
    );
}

#[tokio::test(start_paused = true)]
async fn a_snapshot_missing_a_selected_field_fails_the_read_and_is_not_cached() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        if let Some(EntityLookup::Found { entity, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            entity.origin = None;
        }
    });
    f.read_with(vec![gts_key(TYPE).into()], full(), true)
        .await
        .expect_err("origin was selected");
    let retry = f
        .read_with(vec![gts_key(TYPE).into()], full(), true)
        .await
        .expect("healthy");
    assert!(matches!(
        &retry.0[&gts_key(TYPE)],
        EntityLookup::Found { entity, .. } if entity.origin.is_some()
    ));
    assert!(
        f.recorder.batches()[1][0].0.if_none_match.is_none(),
        "the broken body left no validator to revalidate against"
    );
}

#[tokio::test(start_paused = true)]
async fn a_snapshot_with_an_unselected_field_fails_the_read() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        if let Some(EntityLookup::Found { entity, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            entity.content = Some(json!({}));
        }
    });
    f.read_with(vec![gts_key(TYPE).into()], Projection::Default, false)
        .await
        .expect_err("content was not selected");
}

#[tokio::test(start_paused = true)]
async fn a_validator_too_long_to_send_back_fails_the_read() {
    let f = fixture(WINDOW);
    f.recorder.tamper_next(|response| {
        if let Some(EntityLookup::Found { etag, .. }) = response.0.get_mut(&gts_key(TYPE)) {
            *etag = Validator::from_bytes(vec![b'a'; 1025]);
        }
    });
    f.read_with(vec![gts_key(TYPE).into()], full(), false)
        .await
        .expect_err("unusable validator");
    f.title(gts_key(TYPE), full(), false).await;
    assert!(f.recorder.batches()[1][0].0.if_none_match.is_none());
}

#[tokio::test(start_paused = true)]
async fn a_changed_validator_the_caller_sent_drops_the_other_projections() {
    let f = fixture(WINDOW);
    f.title(gts_key(TYPE), full(), false).await;
    // The caller holds a content-only validator from elsewhere; this cache holds none.
    let held = match f
        .registry
        .batch_get_entities(
            &ctx(),
            BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(content_only()),
        )
        .await
        .expect("read")
        .0
        .remove(&gts_key(TYPE))
    {
        Some(EntityLookup::Found { etag, .. }) => etag,
        other => panic!("found, got {other:?}"),
    };
    change(&f.registry, TYPE, "v2", 2);

    let response = f
        .read_with(
            vec![BatchGetItem::new(gts_key(TYPE)).with_if_none_match(held)],
            content_only(),
            false,
        )
        .await
        .expect("read");
    assert!(matches!(
        response.0[&gts_key(TYPE)],
        EntityLookup::Found { .. }
    ));
    assert_eq!(
        f.title(gts_key(TYPE), full(), false).await,
        "v2",
        "the observed change reached the other projection"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cold_caller_match_does_not_evict_what_the_other_spelling_just_filled() {
    for uuid_first in [false, true] {
        let f = fixture(WINDOW);
        let held = match f
            .registry
            .batch_get_entities(
                &ctx(),
                BatchGetEntitiesRequest::new([gts_key(TYPE).into()]).with_projection(full()),
            )
            .await
            .expect("read")
            .0
            .remove(&gts_key(TYPE))
        {
            Some(EntityLookup::Found { etag, .. }) => etag,
            other => panic!("found, got {other:?}"),
        };
        let by_id = BatchGetItem::new(gts_key(TYPE));
        let by_uuid = BatchGetItem::new(uuid_key(TYPE)).with_if_none_match(held.clone());
        let items = if uuid_first {
            vec![by_uuid, by_id]
        } else {
            vec![by_id, by_uuid]
        };
        let response = f.read_with(items, full(), false).await.expect("read");
        assert!(matches!(
            response.0[&gts_key(TYPE)],
            EntityLookup::Found { .. }
        ));
        assert_eq!(
            response.0[&uuid_key(TYPE)],
            EntityLookup::Unchanged { etag: held }
        );

        f.title(gts_key(TYPE), full(), false).await;
        assert_eq!(
            f.recorder.batch_count(),
            1,
            "uuid_first={uuid_first}: still resident"
        );
    }
}

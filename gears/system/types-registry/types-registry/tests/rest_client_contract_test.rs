#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! The SDK's platform REST client against the registry's real platform routes over TCP,
//! behind the `OoP` route-auth stack (REST contract, T34; SPEC §10.1, D17, D19).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::{Next, from_fn};
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;
use secrecy::SecretString;
use serde_json::json;
use toolkit_canonical_errors::{CanonicalError, InvalidArgument};
use toolkit_contract::runtime::config::{
    ClientConfig, CredentialState, InternalTokenProvider, RetryConfig,
};
use toolkit_gts::gts_id;
use toolkit_security::PlatformSecurityContext;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetItem, EntityKey, EntityLookup, FieldSelection, IdempotencyKey,
    ListEntitiesRequest, Operation, OperationStatus, PageRequest, PlatformTypesRegistryApi,
    PlatformTypesRegistryRestClient, Projection, PublisherContext, RegisterEntitiesRequest,
    RegisterItem, RegistrationOutcome, reason,
};
use uuid::Uuid;

mod common;
use common::platform_server::{INTERNAL_TOKEN, PlatformServer, serve, serve_with};

const CF_TYPE: &str = gts_id!("cf.core.example.type.v1~");
const CF_OTHER: &str = gts_id!("cf.core.example.other.v1~");

/// One request as it reached the listener, before authentication.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    token: bool,
    bearer: bool,
}

/// Records every request, and answers the first `n` matching ones `503` before the
/// registry sees them.
#[derive(Default)]
struct Faults {
    seen: Mutex<Vec<Seen>>,
    /// `(method, path suffix, remaining failures)`.
    fail: Mutex<Vec<(String, String, usize)>>,
}

impl Faults {
    fn fail(&self, method: &str, suffix: &str, times: usize) {
        self.fail
            .lock()
            .push((method.to_owned(), suffix.to_owned(), times));
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().clone()
    }

    fn wrap(self: &Arc<Self>) -> impl FnOnce(Router) -> Router + use<> {
        let faults = Arc::clone(self);
        move |app| {
            app.layer(from_fn(move |request: Request<Body>, next: Next| {
                let faults = Arc::clone(&faults);
                async move { faults.handle(request, next).await }
            }))
        }
    }

    async fn handle(&self, request: Request<Body>, next: Next) -> Response {
        let method = request.method().to_string();
        let path = request.uri().path().to_owned();
        self.seen.lock().push(Seen {
            method: method.clone(),
            path: path.clone(),
            token: request.headers().contains_key(INTERNAL_TOKEN_HEADER),
            bearer: request.headers().contains_key("authorization"),
        });
        let refuse = {
            let mut rules = self.fail.lock();
            rules
                .iter_mut()
                .find(|(m, suffix, left)| *m == method && path.ends_with(suffix) && *left > 0)
                .map(|rule| rule.2 -= 1)
                .is_some()
        };
        if refuse {
            return (StatusCode::SERVICE_UNAVAILABLE, "injected").into_response();
        }
        next.run(request).await
    }
}

/// A client whose token provider counts how often it is consulted: once per attempt.
fn client(
    server: &PlatformServer,
    resolutions: &Arc<AtomicUsize>,
) -> PlatformTypesRegistryRestClient {
    let counter = Arc::clone(resolutions);
    let provider = InternalTokenProvider::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        CredentialState::Available(SecretString::from(INTERNAL_TOKEN.to_owned()))
    });
    let config = ClientConfig::new(server.base_url.clone())
        .with_internal_token_provider(provider)
        .with_retry(RetryConfig {
            max_attempts: 3,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(50),
            multiplier: 2.0,
        });
    PlatformTypesRegistryRestClient::new(config).expect("a REST client")
}

fn ctx() -> PlatformSecurityContext {
    PlatformSecurityContext::outbound_marker()
}

fn publisher() -> PublisherContext {
    PublisherContext {
        name: "rest-contract-test".to_owned(),
        version: "1.0.0".parse().expect("version"),
    }
}

fn schema(gts_id: &str) -> serde_json::Value {
    json!({
        "$id": format!("gts://{gts_id}"),
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "name": { "type": "string" } },
    })
}

fn register(gts_id: &str) -> RegisterEntitiesRequest {
    RegisterEntitiesRequest {
        items: vec![RegisterItem {
            gts_id: gts::GtsId::try_new(gts_id).expect("an identifier"),
            content: schema(gts_id),
            expected_resource_version: None,
            skip_compatibility_check: false,
        }],
        dry_run: false,
        publisher: publisher(),
    }
}

fn key(text: &str) -> IdempotencyKey {
    IdempotencyKey::new(text).expect("a key")
}

/// The local and the TCP client agree: the SDK's whole conformance suite over REST.
#[tokio::test]
async fn the_rest_client_conforms_to_the_contract() {
    let server = serve().await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    types_registry_sdk::testing_platform::conformance::run(&client(&server, &resolutions)).await;
}

/// Submit → poll → exact read → `batchGet` (`found`, then `unchanged` under the same
/// `$select`) → discovery; then replay and a conflicting reuse of the key.
#[tokio::test]
async fn submit_read_and_replay_over_tcp() {
    let faults = Arc::new(Faults::default());
    let server = serve_with(faults.wrap()).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = client(&server, &resolutions);

    let operation = client
        .register_entities(&ctx(), key("tcp-register"), register(CF_TYPE))
        .await
        .expect("accepted and read back");
    let operation = if operation.status == OperationStatus::Completed {
        operation
    } else {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let Operation::Registration(polled) = client
                .get_operation(&ctx(), operation.operation_id)
                .await
                .expect("polled")
            else {
                panic!("a registration");
            };
            if polled.status == OperationStatus::Completed {
                break polled;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the operation completes"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    assert!(
        matches!(
            operation.items[0].outcome,
            RegistrationOutcome::Succeeded {
                resource_version: 1
            }
        ),
        "{:?}",
        operation.items
    );

    let selection = Projection::Select(FieldSelection::full());
    let read = |if_none_match| BatchGetEntitiesRequest {
        items: vec![BatchGetItem {
            key: EntityKey::GtsId(gts::GtsId::try_new(CF_TYPE).unwrap()),
            if_none_match,
        }],
        projection: selection.clone(),
        fresh: false,
    };
    let found = client
        .batch_get_entities(&ctx(), read(None))
        .await
        .expect("read")
        .0;
    let Some(EntityLookup::Found { entity, etag }) = found.values().next() else {
        panic!("found: {found:?}");
    };
    assert_eq!(entity.content, Some(schema(CF_TYPE)));
    let unchanged = client
        .batch_get_entities(&ctx(), read(Some(etag.clone())))
        .await
        .expect("read")
        .0;
    assert_eq!(
        unchanged.values().next(),
        Some(&EntityLookup::Unchanged { etag: etag.clone() })
    );

    let page = client
        .list_entities(
            &ctx(),
            ListEntitiesRequest {
                filter: types_registry_sdk::EntityFilter {
                    pattern: Some(gts::GtsIdPattern::try_new(CF_TYPE).unwrap()),
                    ..Default::default()
                },
                projection: Projection::Default,
                page: PageRequest::default(),
            },
        )
        .await
        .expect("a page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].gts_id.as_ref(), CF_TYPE);

    let replay = client
        .register_entities(&ctx(), key("tcp-register"), register(CF_TYPE))
        .await
        .expect("replayed");
    assert_eq!(
        replay.operation_id, operation.operation_id,
        "the same operation"
    );
    assert!(matches!(
        replay.items[0].outcome,
        RegistrationOutcome::Succeeded {
            resource_version: 1
        }
    ));

    let conflict = client
        .register_entities(&ctx(), key("tcp-register"), register(CF_OTHER))
        .await
        .expect_err("a different request under one key");
    assert!(
        matches!(conflict, CanonicalError::AlreadyExists { .. }),
        "{conflict:?}"
    );

    let seen = faults.seen();
    assert!(!seen.is_empty());
    assert!(
        seen.iter().all(|request| request.token && !request.bearer),
        "every request carries the internal token and never a bearer: {seen:?}"
    );
    assert_eq!(
        resolutions.load(Ordering::SeqCst),
        seen.len(),
        "one token resolution per attempt"
    );
}

/// A transient failure retries a keyed submission and a read with a fresh token each
/// time; the retried submission is one operation.
#[tokio::test]
async fn transient_failures_are_retried_with_a_fresh_token() {
    let faults = Arc::new(Faults::default());
    faults.fail("POST", "/platform/v1/entities", 1);
    faults.fail("POST", "/platform/v1/entities:batchGet", 2);
    let server = serve_with(faults.wrap()).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = client(&server, &resolutions);

    let operation = client
        .register_entities(&ctx(), key("tcp-retry"), register(CF_TYPE))
        .await
        .expect("accepted on the second attempt");
    client
        .batch_get_entities(
            &ctx(),
            BatchGetEntitiesRequest {
                items: vec![BatchGetItem::from(EntityKey::GtsId(
                    gts::GtsId::try_new(CF_TYPE).unwrap(),
                ))],
                projection: Projection::Default,
                fresh: false,
            },
        )
        .await
        .expect("read on the third attempt");

    let submissions = faults
        .seen()
        .into_iter()
        .filter(|seen| seen.method == "POST" && seen.path.ends_with("/platform/v1/entities"))
        .count();
    assert_eq!(submissions, 2, "one refused attempt, one accepted");
    assert_eq!(resolutions.load(Ordering::SeqCst), faults.seen().len());
    let records = server
        .service
        .operation(&ctx(), operation.operation_id)
        .await
        .expect("read")
        .expect("the operation exists");
    assert_eq!(records.operation_id, operation.operation_id);
}

/// D19: a lost read-back names the accepted operation; replaying the key recovers it.
#[tokio::test]
async fn a_lost_read_back_names_the_operation_and_a_replay_recovers_it() {
    let faults = Arc::new(Faults::default());
    faults.fail("GET", "", usize::MAX);
    let server = serve_with(faults.wrap()).await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = client(&server, &resolutions);

    let lost = client
        .register_entities(&ctx(), key("tcp-lost"), register(CF_TYPE))
        .await
        .expect_err("the read-back fails");
    let CanonicalError::Aborted {
        ctx: ref aborted,
        ref resource_name,
        ..
    } = lost
    else {
        panic!("Aborted, got {lost:?}");
    };
    assert_eq!(aborted.reason, reason::aborted::OPERATION_READ_FAILED);
    let operation_id: Uuid = resource_name
        .as_deref()
        .expect("names the operation")
        .parse()
        .expect("an operation id");

    faults.fail.lock().clear();
    let replay = client
        .register_entities(&ctx(), key("tcp-lost"), register(CF_TYPE))
        .await
        .expect("replayed");
    assert_eq!(replay.operation_id, operation_id);
}

/// Problems map to `CanonicalError` without loss: category, resource and violations.
#[tokio::test]
async fn refusals_map_to_their_canonical_errors() {
    let server = serve().await;
    let resolutions = Arc::new(AtomicUsize::new(0));
    let client = client(&server, &resolutions);

    let missing = Uuid::from_u128(7);
    let not_found = client
        .get_operation(&ctx(), missing)
        .await
        .expect_err("no such operation");
    let CanonicalError::NotFound {
        ref resource_type,
        ref resource_name,
        ..
    } = not_found
    else {
        panic!("NotFound, got {not_found:?}");
    };
    assert_eq!(
        resource_type.as_deref(),
        Some(types_registry_sdk::OPERATION_RESOURCE_TYPE)
    );
    assert_eq!(resource_name.as_deref(), Some(missing.to_string().as_str()));

    let refused = client
        .list_entities(
            &ctx(),
            ListEntitiesRequest {
                page: PageRequest {
                    limit: Some(100_000),
                    cursor: None,
                },
                ..Default::default()
            },
        )
        .await
        .expect_err("a page over the configured maximum");
    let CanonicalError::InvalidArgument {
        ctx: InvalidArgument::FieldViolations { field_violations },
        ..
    } = refused
    else {
        panic!("InvalidArgument, got {refused:?}");
    };
    assert_eq!(field_violations[0].field, "limit");
}

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]

//! Registry auth matrix through api-gateway without an internal authenticator (T35).
//! Platform routes return canonical 401s even with a valid token; tenant reads
//! accept a valid bearer. Both credentials together are refused on either plane.
//! No database is bound: an admitted request reaches its handler and returns 503.
//!
//! See e2e `TR-PLANE` for a configured gateway and `platform_routes_auth_test`
//! for the OoP listener.

use std::sync::Arc;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationResult, ClientCredentialsRequest,
};
use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use toolkit::config::ConfigProvider;
use toolkit::context::GearCtx;
use toolkit::contracts::{ApiGatewayCapability, OpenApiRegistry, RestApiCapability};
use toolkit::{ClientHub, Gear};
use toolkit_security::SecurityContext;
use tower::ServiceExt;
use uuid::Uuid;

const BEARER: &str = "types-registry-planes-bearer";
const TOKEN: &str = "types-registry-planes-token";
const CF_TYPE: &str = "gts.cf.core.example.type.v1~";
const PLATFORM: &str = "/types-registry/v2/platform";
const TENANT: &str = "/types-registry/v2";

struct Config(Value);

impl ConfigProvider for Config {
    fn get_gear_config(&self, gear: &str) -> Option<&Value> {
        self.0.get(gear)
    }
}

/// Accepts [`BEARER`] only.
struct OneBearer;

#[async_trait]
impl AuthNResolverClient for OneBearer {
    async fn authenticate(&self, token: &str) -> Result<AuthenticationResult, AuthNResolverError> {
        if token == BEARER {
            Ok(AuthenticationResult {
                security_context: SecurityContext::builder()
                    .subject_id(Uuid::from_u128(1))
                    .subject_tenant_id(Uuid::from_u128(2))
                    .build()
                    .unwrap(),
            })
        } else {
            Err(AuthNResolverError::Unauthorized("invalid token".to_owned()))
        }
    }

    async fn exchange_client_credentials(
        &self,
        _request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        Err(AuthNResolverError::Internal("not used".to_owned()))
    }
}

/// The registry's routes as its gear registers them, without a database.
struct Registry;

#[async_trait]
impl Gear for Registry {
    async fn init(&self, _ctx: &GearCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

impl RestApiCapability for Registry {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<Router> {
        let config = types_registry::config::TypesRegistryConfig::default();
        let legacy = Arc::new(types_registry::domain::service::TypesRegistryService::new(
            Arc::new(types_registry::infra::InMemoryGtsRepository::new(
                config.to_gts_config(),
            )),
            config,
        ));
        Ok(types_registry::api::rest::routes::register_routes(
            router, openapi, legacy, None,
        ))
    }
}

fn ctx(name: &str, config: Value, hub: Arc<ClientHub>) -> GearCtx {
    GearCtx::new(
        name,
        Uuid::new_v4(),
        Arc::new(Config(config)),
        hub,
        tokio_util::sync::CancellationToken::new(),
    )
}

/// A finalized gateway with authentication on and no `internal_auth`.
async fn gateway() -> Router {
    let hub = Arc::new(ClientHub::new());
    hub.register::<dyn AuthNResolverClient>(Arc::new(OneBearer));
    let api_ctx = ctx(
        "api-gateway",
        json!({ "api-gateway": { "config": {
            "bind_addr": "0.0.0.0:8080",
            "enable_docs": false,
            "auth_disabled": false,
        } } }),
        hub,
    );
    let gateway = api_gateway::ApiGateway::default();
    gateway.init(&api_ctx).await.expect("init");
    let router = Registry
        .register_rest(
            &ctx("types-registry", json!({}), Arc::new(ClientHub::new())),
            Router::new(),
            &gateway,
        )
        .expect("routes");
    gateway
        .rest_finalize(
            &api_ctx,
            router,
            Arc::new(toolkit::RestHealthcheckRegistry::new()),
        )
        .expect("finalize")
}

#[derive(Clone, Copy, Debug)]
enum Credentials {
    None,
    Token(&'static str),
    Bearer(&'static str),
    Both(&'static str, &'static str),
}

/// A route: its name in failure messages, method, URI and body.
type Route = (&'static str, Method, String, Option<Value>);

/// Every route of both sets, with a body each accepts.
fn routes() -> Vec<Route> {
    let operation = "00000000-0000-0000-0000-000000000001";
    let read = json!({ "items": [{ "entity_key": CF_TYPE }] });
    vec![
        (
            "submit",
            Method::POST,
            format!("{PLATFORM}/entities"),
            Some(json!({ "items": [], "dry_run": true })),
        ),
        (
            "batch_delete",
            Method::POST,
            format!("{PLATFORM}/entities:batchDelete"),
            Some(json!({ "items": [], "dry_run": true })),
        ),
        (
            "delete",
            Method::DELETE,
            format!("{PLATFORM}/entities/{CF_TYPE}?expected_resource_version=1&dry_run=true"),
            None,
        ),
        (
            "operation",
            Method::GET,
            format!("{PLATFORM}/operations/{operation}"),
            None,
        ),
        (
            "platform_get",
            Method::GET,
            format!("{PLATFORM}/entities/{CF_TYPE}"),
            None,
        ),
        (
            "platform_batch_get",
            Method::POST,
            format!("{PLATFORM}/entities:batchGet"),
            Some(read.clone()),
        ),
        (
            "platform_list",
            Method::GET,
            format!("{PLATFORM}/entities"),
            None,
        ),
        (
            "tenant_get",
            Method::GET,
            format!("{TENANT}/entities/{CF_TYPE}"),
            None,
        ),
        (
            "tenant_batch_get",
            Method::POST,
            format!("{TENANT}/entities:batchGet"),
            Some(read),
        ),
        (
            "tenant_list",
            Method::GET,
            format!("{TENANT}/entities"),
            None,
        ),
    ]
}

async fn send(
    router: &Router,
    method: &Method,
    uri: &str,
    body: Option<&Value>,
    credentials: Credentials,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(uri)
        .header("idempotency-key", Uuid::new_v4().to_string());
    let (bearer, token) = match credentials {
        Credentials::None => (None, None),
        Credentials::Token(token) => (None, Some(token)),
        Credentials::Bearer(bearer) => (Some(bearer), None),
        Credentials::Both(token, bearer) => (Some(bearer), Some(token)),
    };
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    if let Some(token) = token {
        builder = builder.header("x-toolkit-internal-token", token);
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string())),
        None => builder.body(Body::empty()),
    }
    .unwrap();
    let response = router.clone().oneshot(request).await.expect("a response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn assert_refused(
    route: &str,
    credentials: Credentials,
    answer: &(StatusCode, Value),
    reason: &str,
) {
    assert_eq!(
        answer.0,
        StatusCode::UNAUTHORIZED,
        "{route} with {credentials:?}: {}",
        answer.1
    );
    assert_eq!(
        answer.1["context"]["reason"],
        json!(reason),
        "{route} with {credentials:?}: {}",
        answer.1
    );
}

#[tokio::test]
async fn without_an_internal_authenticator_the_gateway_refuses_every_platform_route() {
    let router = gateway().await;
    for (route, method, uri, body) in routes().into_iter().filter(|r| !r.0.starts_with("tenant")) {
        for (credentials, reason) in [
            (Credentials::None, "MISSING_INTERNAL_TOKEN"),
            // Presented, but nothing at this gateway can validate it.
            (Credentials::Token(TOKEN), "UNVERIFIED_INTERNAL_TOKEN"),
            (
                Credentials::Token("forged-token"),
                "UNVERIFIED_INTERNAL_TOKEN",
            ),
            // Dropped unvalidated on a platform route, so the token is missing.
            (Credentials::Bearer(BEARER), "MISSING_INTERNAL_TOKEN"),
            (
                Credentials::Bearer("forged-bearer"),
                "MISSING_INTERNAL_TOKEN",
            ),
        ] {
            let answer = send(&router, &method, &uri, body.as_ref(), credentials).await;
            assert_refused(route, credentials, &answer, reason);
        }
    }
}

#[tokio::test]
async fn the_tenant_reads_admit_a_valid_bearer_only() {
    let router = gateway().await;
    for (route, method, uri, body) in routes().into_iter().filter(|r| r.0.starts_with("tenant")) {
        for (credentials, reason) in [
            (Credentials::None, "MISSING_BEARER"),
            // Dropped unvalidated on a tenant route, so the bearer is missing.
            (Credentials::Token(TOKEN), "MISSING_BEARER"),
            (Credentials::Bearer("forged-bearer"), "AUTHN_FAILED"),
        ] {
            let answer = send(&router, &method, &uri, body.as_ref(), credentials).await;
            assert_refused(route, credentials, &answer, reason);
        }
        let admitted = send(
            &router,
            &method,
            &uri,
            body.as_ref(),
            Credentials::Bearer(BEARER),
        )
        .await;
        assert_eq!(
            admitted.0,
            StatusCode::SERVICE_UNAVAILABLE,
            "{route}: the registry's handler answered (no database): {}",
            admitted.1
        );
    }
}

#[tokio::test]
async fn both_credentials_are_refused_on_either_route_set() {
    let router = gateway().await;
    for (route, method, uri, body) in routes() {
        for (token, bearer) in [
            (TOKEN, BEARER),
            (TOKEN, "forged-bearer"),
            ("forged-token", BEARER),
            ("forged-token", "forged-bearer"),
        ] {
            let credentials = Credentials::Both(token, bearer);
            let answer = send(&router, &method, &uri, body.as_ref(), credentials).await;
            assert_refused(route, credentials, &answer, "MIXED_CREDENTIALS");
        }
    }
}

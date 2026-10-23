#![cfg(all(unix, feature = "dev-authn"))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a setup failure is the test failure"
)]
//! `cf-types-registry-host` started as its own process (SPEC §8.4, P30): its own database,
//! platform-plane `shared_secret` authentication and the linked authentication stack. The SDK's two
//! REST clients drive it as a gear in another process would: a platform write, then tenant
//! reads served to a validated bearer and refused to a forged or absent one.
//!
//! The process without any tenant authenticator is the route-auth stack `oop_serve`
//! builds with none, covered in the registry's `platform_routes_auth_test`: linking
//! authn-resolver always registers one, so this binary cannot start without it.

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use secrecy::SecretString;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::{ClientConfig, CredentialState, InternalTokenProvider};
use toolkit_gts::gts_id;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use types_registry_sdk::{
    BatchGetEntitiesRequest, EntityKey, EntityLookup, IdempotencyKey, Operation, OperationStatus,
    PlatformTypesRegistryApi, PlatformTypesRegistryRestClient, Projection, PublisherContext,
    RegisterEntitiesRequest, RegisterItem, TypesRegistryApi, TypesRegistryRestClient,
};

const INTERNAL_TOKEN: &str = "types-registry-host-test-internal-token";
const BEARER: &str = "types-registry-host-test-bearer";
const CF_TYPE: &str = gts_id!("cf.core.example.hosted.v1~");

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// The static plugin maps [`BEARER`] only, at a priority that wins over OIDC; OIDC is
/// linked and configured as in production but never reached.
fn config_yaml(home: &Path, port: u16) -> String {
    format!(
        r#"server:
  home_dir: "{home}"
logging:
  default:
    console_level: warn
database:
  servers:
    sqlite:
      engine: "sqlite"
oop_http:
  listen_addr: "127.0.0.1:{port}"
  allow_loopback_advertise: true
  drain_timeout_secs: 1
  internal_auth:
    provider: shared_secret
    secret: "{INTERNAL_TOKEN}"
gears:
  types-registry:
    database:
      server: "sqlite"
      file: "types_registry.db"
    config: {{}}
  authn-resolver:
    config:
      vendor: "constructorfabric"
  static-authn-plugin:
    config:
      vendor: "constructorfabric"
      priority: 10
      mode: static_tokens
      tokens:
        - token: "{BEARER}"
          identity:
            subject_id: "00000000-0000-0000-0000-0000000000a1"
            subject_tenant_id: "00000000-0000-0000-0000-0000000000b1"
            token_scopes: ["*"]
  oidc-authn-plugin:
    config:
      priority: 100
      jwt:
        trusted_issuers:
          - issuer: "https://idp.invalid"
        claim_mapping:
          subject_id: "sub"
          subject_tenant_id: "tenant_id"
      s2s_oauth:
        discovery_url: "https://idp.invalid"
        default_subject_type: "gts.cf.core.security.subject_user.v1~"
"#,
        home = home.display(),
    )
}

/// Reaps the child on every exit from the test, an unwinding assertion included.
struct ChildReaper(std::process::Child);

impl Drop for ChildReaper {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            drop(self.0.kill());
            drop(self.0.wait());
        }
    }
}

/// One `/readyz` exchange, bounded so a listener that accepts and never answers cannot
/// hold the test past its deadline.
async fn readyz(addr: SocketAddr) -> Option<String> {
    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
        let request = "GET /readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.ok()?;
        Some(String::from_utf8_lossy(&buf).into_owned())
    };
    tokio::time::timeout(Duration::from_secs(2), exchange)
        .await
        .ok()
        .flatten()
}

/// Polls until ready. A child that exits first — a port taken between `free_port` and
/// its bind, a refused configuration — fails the test with its status.
async fn await_ready(child: &mut ChildReaper, addr: SocketAddr) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut last = None;
    loop {
        if let Some(status) = child.0.try_wait().expect("poll the child") {
            panic!("cf-types-registry-host exited before it was ready: {status}");
        }
        last = readyz(addr).await.or(last);
        if last
            .as_deref()
            .is_some_and(|answer| answer.contains("\"ready\":true"))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "never ready; last answer: {last:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn caller(bearer: &str) -> SecurityContext {
    SecurityContext::builder()
        .subject_id(uuid::Uuid::from_u128(1))
        .subject_tenant_id(uuid::Uuid::from_u128(2))
        .bearer_token(bearer.to_owned())
        .build()
        .expect("a context")
}

fn read() -> BatchGetEntitiesRequest {
    BatchGetEntitiesRequest {
        items: vec![EntityKey::GtsId(gts::GtsId::try_new(CF_TYPE).unwrap()).into()],
        projection: Projection::Default,
        fresh: false,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_host_serves_both_planes_with_its_linked_authn() {
    let home = tempfile::tempdir().expect("tempdir");
    let port = free_port();
    let config_path = home.path().join("types-registry-host.yaml");
    std::fs::write(&config_path, config_yaml(home.path(), port)).expect("write config");
    let mut child = ChildReaper(
        std::process::Command::new(env!("CARGO_BIN_EXE_cf-types-registry-host"))
            .arg("--config")
            .arg(&config_path)
            // Connected lazily and never reached: registration retries in the background.
            .env(
                toolkit::runtime::TOOLKIT_DIRECTORY_ENDPOINT_ENV,
                "http://127.0.0.1:1",
            )
            .env("HOME", home.path())
            .spawn()
            .expect("cargo builds the binary for this test"),
    );
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    await_ready(&mut child, addr).await;
    let base_url = format!("http://{addr}");

    let platform = PlatformTypesRegistryRestClient::new(
        ClientConfig::new(base_url.clone()).with_internal_token_provider(
            InternalTokenProvider::new(|| {
                CredentialState::Available(SecretString::from(INTERNAL_TOKEN.to_owned()))
            }),
        ),
    )
    .expect("a platform client");
    let ctx = PlatformSecurityContext::outbound_marker();
    let submitted = platform
        .register_entities(
            &ctx,
            IdempotencyKey::new("host-bootstrap").unwrap(),
            RegisterEntitiesRequest {
                items: vec![RegisterItem {
                    gts_id: gts::GtsId::try_new(CF_TYPE).unwrap(),
                    content: serde_json::json!({
                        "$id": format!("gts://{CF_TYPE}"),
                        "$schema": "http://json-schema.org/draft-07/schema#",
                        "type": "object",
                    }),
                    expected_resource_version: None,
                    skip_compatibility_check: false,
                }],
                dry_run: false,
                publisher: PublisherContext {
                    name: "types-registry-host-test".to_owned(),
                    version: "1.0.0".parse().unwrap(),
                },
            },
        )
        .await
        .expect("a platform write over the internal token");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while platform
        .get_operation(&ctx, submitted.operation_id)
        .await
        .map(|operation| operation.status())
        .expect("polled")
        != OperationStatus::Completed
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the write completes"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let Operation::Registration(_) = platform
        .get_operation(&ctx, submitted.operation_id)
        .await
        .expect("read")
    else {
        panic!("a registration");
    };

    let tenant =
        TypesRegistryRestClient::new(ClientConfig::new(base_url)).expect("a tenant client");
    let found = tenant
        .batch_get_entities(&caller(BEARER), read())
        .await
        .expect("a validated bearer is served");
    assert!(
        matches!(found.0.values().next(), Some(EntityLookup::Found { .. })),
        "{found:?}"
    );
    for refused in [
        tenant
            .batch_get_entities(&caller("forged-bearer"), read())
            .await,
        tenant
            .batch_get_entities(&SecurityContext::anonymous(), read())
            .await,
    ] {
        let refused = refused.expect_err("no validated bearer, no tenant read");
        assert!(
            matches!(refused, CanonicalError::Unauthenticated { .. }),
            "{refused:?}"
        );
    }
}

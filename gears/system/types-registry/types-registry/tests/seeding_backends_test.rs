#![cfg(feature = "integration")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "integration tests: a failed fixture or assertion step is a test failure"
)]
//! Startup seeding on SQLite, PostgreSQL and MySQL: cold start, equal restart and
//! two same-version seeders racing on an empty and on a populated database (T31).

mod common;

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use tokio_util::sync::CancellationToken;
use toolkit_db::{DBProvider, DbError};
use types_registry::config::TypesRegistryConfig;
use types_registry::domain::admission::OperationDispatch;
use types_registry::domain::policy::RegistrationPolicy;
use types_registry::domain::registry_service::{EntityKey, RegistryService};
use types_registry::domain::seeding::{SeedSet, seed};
use types_registry::domain::selection::FieldSelection;
use types_registry::infra::outbox::OutboxDispatch;

/// Two registry instances race: a message one sequences into a partition the
/// other's processor leases waits for that processor's idle poll (one minute under
/// the `low_latency` outbox profile), so the budget covers more than one poll.
const BUDGET: Duration = Duration::from_mins(3);
const BASE: &str = "gts.cf.core.seedback.base.v1~";
const CHILD: &str = "gts.cf.core.seedback.base.v1~cf.core.seedback.child.v1~";
const INSTANCE: &str =
    "gts.cf.core.seedback.base.v1~cf.core.seedback.child.v1~cf.core.seedback.one.v1";

struct Registry {
    service: Arc<RegistryService>,
    outbox: toolkit_db::outbox::OutboxHandle,
}

async fn registry(db: &Arc<DBProvider<DbError>>, backend: &str) -> Registry {
    let dispatch = Arc::new(OutboxDispatch::new());
    let service = Arc::new(RegistryService::new(
        db.db(),
        common::stores(),
        RegistrationPolicy::default(),
        TypesRegistryConfig::default(),
        Arc::clone(&dispatch) as Arc<dyn OperationDispatch>,
        common::metrics(),
    ));
    let outbox = types_registry::infra::outbox::start(db.db(), &service, &dispatch)
        .await
        .unwrap_or_else(|e| panic!("{backend}: start the admission outbox: {e}"));
    Registry { service, outbox }
}

fn seeds(description: &str) -> SeedSet {
    let documents = [
        json!({
            "$id": format!("gts://{BASE}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {"id": {"type": "string"}, "name": {"type": "string"}}
        }),
        json!({
            "$id": format!("gts://{CHILD}"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "description": description,
            "type": "object",
            "allOf": [{"$ref": format!("gts://{BASE}")}]
        }),
        json!({"id": INSTANCE, "name": "one"}),
    ];
    SeedSet::collect(documents, &TypesRegistryConfig::default().entity_id_fields)
        .expect("a valid seed set")
}

async fn revision(service: &RegistryService, gts_id: &str) -> Option<(i64, Value)> {
    let record = service
        .entity(
            common::caller(),
            &EntityKey::GtsId(gts_id.to_owned()),
            FieldSelection::full(),
        )
        .await
        .expect("read a seed")?;
    Some((
        record.origin.expect("managed").resource_version,
        common::doc(record.content.as_deref()).expect("content"),
    ))
}

async fn assert_seeding(db: &Arc<DBProvider<DbError>>, backend: &str) {
    let cancel = CancellationToken::new();
    let a = registry(db, backend).await;
    let b = registry(db, backend).await;

    // Two seeders race on the empty database: each document is admitted once.
    let set = seeds("first");
    let (left, right) = tokio::join!(
        seed(&a.service, &set, BUDGET, &cancel),
        seed(&b.service, &set, BUDGET, &cancel),
    );
    left.unwrap_or_else(|e| panic!("{backend}: left seeder on an empty database: {e}"));
    right.unwrap_or_else(|e| panic!("{backend}: right seeder on an empty database: {e}"));
    for id in [BASE, CHILD, INSTANCE] {
        assert_eq!(
            revision(&a.service, id).await.map(|(rv, _)| rv),
            Some(1),
            "{backend}: {id} admitted once"
        );
    }

    // An equal restart allocates nothing.
    let summary = seed(&a.service, &set, BUDGET, &cancel)
        .await
        .unwrap_or_else(|e| panic!("{backend}: equal restart: {e}"));
    assert!(summary.up_to_date, "{backend}: nothing submitted");
    for id in [BASE, CHILD, INSTANCE] {
        assert_eq!(
            revision(&a.service, id).await.map(|(rv, _)| rv),
            Some(1),
            "{backend}: {id} kept its revision"
        );
    }

    // Both race the same permitted change: one revision.
    let changed = seeds("second");
    let (left, right) = tokio::join!(
        seed(&a.service, &changed, BUDGET, &cancel),
        seed(&b.service, &changed, BUDGET, &cancel),
    );
    left.unwrap_or_else(|e| panic!("{backend}: left seeder on an update: {e}"));
    right.unwrap_or_else(|e| panic!("{backend}: right seeder on an update: {e}"));
    let (rv, content) = revision(&a.service, CHILD)
        .await
        .expect("the child is stored");
    assert_eq!(rv, 2, "{backend}: exactly one revision");
    assert_eq!(content["description"], "second", "{backend}");
    assert_eq!(
        revision(&a.service, BASE).await.map(|(rv, _)| rv),
        Some(1),
        "{backend}"
    );

    a.outbox.stop().await;
    b.outbox.stop().await;
}

async fn wait_for_tcp(host: &str, port: u16, timeout: Duration) {
    use tokio::net::TcpStream;
    use tokio::time::{Instant, sleep};

    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect((host, port)).await.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {host}:{port}"
        );
        sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn seeding_behaves_on_sqlite_control() {
    let dir = common::TestDir::new("tr-seed-backends");
    let dsn = format!(
        "sqlite://{}?mode=rwc&journal_mode=wal",
        dir.path().join("seed.db").display()
    );
    let db = common::provider_for_with_outbox(&dsn, 8).await;
    assert_seeding(&db, "sqlite").await;
}

#[tokio::test]
async fn seeding_behaves_on_postgres() {
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "app");
    let container = request.start().await.expect("start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("postgres port");
    let host = container
        .get_host()
        .await
        .expect("postgres host")
        .to_string();
    wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(1)).await;

    let db =
        common::provider_for_with_outbox(&format!("postgres://user:pass@{host}:{port}/app"), 8)
            .await;
    assert_seeding(&db, "postgres").await;
}

#[tokio::test]
async fn seeding_behaves_on_mysql() {
    let container = test_containers::mysql()
        .start()
        .await
        .expect("start mysql container");
    let port = container
        .get_host_port_ipv4(3306)
        .await
        .expect("mysql port");
    let host = container.get_host().await.expect("mysql host").to_string();
    wait_for_tcp(host.trim_matches(['[', ']']), port, Duration::from_mins(2)).await;

    let db = common::provider_for_with_outbox(&format!("mysql://root@{host}:{port}/test"), 8).await;
    assert_seeding(&db, "mysql").await;
}

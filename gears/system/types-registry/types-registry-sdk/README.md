# Types Registry SDK

Transport-agnostic contracts, models and helpers for the Types Registry gear, which stores
GTS (Global Type System) Type Schemas and Instances.

## Contracts

| Contract | Context | For |
|---|---|---|
| `PlatformTypesRegistryApi` | `PlatformSecurityContext` | the gear's own work: batch reads, discovery, registration, deletion, operation reads |
| `TypesRegistryApi` | a tenant's `SecurityContext` | reads made on a tenant's behalf |

Both are resolved from `ClientHub` as trait objects. Every method returns
`Result<_, CanonicalError>`; project an error with `TypesRegistryError::from` for typed
matching, and an operation item's failure with `AdmissionFailure::from_canonical`.

`PlatformTypesRegistryApiExt` and `TypesRegistryApiExt` add kind-narrowed reads
(`get_type_schema`, `get_instance`, their `_by_uuid` and `batch_get_*` variants),
identifier and Registry Reference resolution (`resolve_type_uuid`, `resolve_type_id`,
`resolve_instance_uuid`, `resolve_instance_id` and their plural forms) and
complete listings (`list_type_schemas`, `list_instances`). The platform extension also
reconciles explicit documents with `reconcile_entities_and_await`. Import the extension
trait to call them; with both imported, choose one by UFCS.

### Platform API over REST (`rest-client`)

`PlatformTypesRegistryRestClient` implements `PlatformTypesRegistryApi` against
`/types-registry/platform/v1/` for a gear in another process. Every attempt carries the
process's `X-ToolKit-Internal-Token` from `ClientConfig::internal_token_provider`,
resolved afresh, and never a bearer; the `PlatformSecurityContext` argument is not sent.
Each call is a safe read or carries its `Idempotency-Key`, so it retries under
`ClientConfig::retry`. A mutation returns its operation as read back, after the receipt's
`Retry-After` (bounded by `retry.max_delay`) when the operation is still running; a failed
read-back is `Aborted` with reason `OPERATION_READ_FAILED` naming the operation, and the
same key replays it. The `otel` feature adds trace propagation and RED client metrics.
The tenant client, the directory-resolving wrappers and `#[provides]` follow in T35.

The GTS identifier types the models use (`GtsId`, `GtsTypeId`, `GtsInstanceId`,
`GtsIdPattern`, `GtsIdSegment`) are re-exported, so building a request needs no direct
`gts` dependency.

## Publishing from a gear

A gear publishes the documents it owns from its own `init()` through the in-process
platform API, then reads them back. Its `Cargo.toml` depends on this SDK and on the
registry gear crate, `cf-gears-types-registry`: `deps = [types_registry]` re-exports that
crate to keep its registration linked, and orders the two `init()` calls.

```rust,no_run
# // This doctest cannot depend on the registry gear; stand the SDK in for it.
# pub extern crate types_registry_sdk as types_registry;
use std::time::Duration;

use toolkit::{Gear, GearCtx};
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsId, GtsTypeId, Origin, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, PublisherContext, ReadOptions, ReconcileOptions,
    ReconcileOutcome, ReconcileReport, Reconciliation,
};

// `deps` makes this gear's `init()` run after the registry's.
#[toolkit::gear(name = "example-events", deps = [types_registry])]
#[derive(Default)]
pub struct ExampleEvents;

#[async_trait::async_trait]
impl Gear for ExampleEvents {
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        // Fails when the registry has no database bound: there is no fallback.
        let registry = ctx.client_hub().get::<dyn PlatformTypesRegistryApi>()?;
        let platform = PlatformSecurityContext::outbound_marker();
        let publisher = PublisherContext {
            name: "example-events".to_owned(),
            version: env!("CARGO_PKG_VERSION").parse()?,
        };
        let desired = vec![(
            GtsId::try_new("gts.cf.example.events.user_created.v1~")?,
            serde_json::json!({
                "$id": "gts://gts.cf.example.events.user_created.v1~",
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object"
            }),
        )];
        let mut options = ReconcileOptions::default();
        options.deadline = Duration::from_secs(120);

        // The startup gate: up to date, or every desired identifier admitted.
        match registry
            .reconcile_entities_and_await(
                &platform,
                &publisher,
                &desired,
                &options,
                ctx.cancellation_token(),
            )
            .await
        {
            // Every document already matched; nothing was submitted.
            Ok(Reconciliation::UpToDate) => {}
            Ok(Reconciliation::Reconciled(outcomes)) => {
                for (id, _) in &desired {
                    match outcomes.get(id) {
                        Some(ReconcileOutcome::Admitted) => {}
                        Some(ReconcileOutcome::Rejected(cause)) => {
                            anyhow::bail!("{id} was rejected: {cause}");
                        }
                        Some(ReconcileOutcome::Pending(cause)) => {
                            anyhow::bail!("{id} did not settle: {cause:?}");
                        }
                        None => anyhow::bail!("{id} has no outcome"),
                    }
                }
            }
            Err(error) => {
                // Name what did not settle, then fail this gear's boot.
                if let Some(report) = ReconcileReport::from_canonical(&error) {
                    for (id, outcome) in &report.outcomes {
                        match outcome {
                            ReconcileOutcome::Admitted => {}
                            ReconcileOutcome::Rejected(cause) => {
                                tracing::error!(%id, %cause, "registration rejected");
                            }
                            ReconcileOutcome::Pending(cause) => {
                                tracing::error!(%id, ?cause, "registration not settled");
                            }
                        }
                    }
                }
                return Err(error.into());
            }
        }

        let type_id = GtsTypeId::try_new("gts.cf.example.events.user_created.v1~")?;
        let read = registry
            .get_type_schema(
                &platform,
                &type_id,
                ReadOptions {
                    projection: Projection::Select(FieldSelection::with(&[
                        EntityField::Content,
                        EntityField::Origin,
                    ])),
                    ..ReadOptions::default()
                },
            )
            .await?;
        if let Some(Origin::Managed { resource_version, .. }) = read.origin {
            tracing::info!(resource_version, "user_created is published");
        }
        Ok(())
    }
}
```

Success is `UpToDate`, or `Reconciled` with every desired identifier `Admitted`. An accepted
operation, or an `Ok` from a single `register_entities`, is not: an item can still fail in
admission.

The examples use vendor `cf`, the only one a registry admits without configuration. Another
vendor needs a `registration_policy` entry for its region in the registry's configuration:

```yaml
gears:
  types-registry:
    config:
      registration_policy:
        "gts.acme.core.*":
          allowed_vendors: [acme]
```

**What the local API is today.** The registry publishes both contracts in `ClientHub` from
its own `init()`, after it has admitted every linked inventory declaration (Type Schemas
from `#[toolkit_gts::gts_type_schema(..)]`, Instances from `gts_instance!` and
`gts_instance_raw!`) and every configured `entities` document into its database. Without a
database bound to `types-registry`, or when that seeding fails, neither contract is
registered and `ClientHub::get` fails; nothing falls back to the legacy client.

- `deps` orders `init()` only. Remote clients are wired after every `init()`, so a remote
  registry is not reachable from `init()`; calling the local API there is a transitional
  exception that ends with T41/T42, after which publication moves to a post-wiring hook.
- `PublisherContext` is required, but P0 neither forwards nor enforces it; that arrives
  with T44/T45, without an earlier guarantee for mixed-version publishers.
- The local client passes `SecurityContext` and `PlatformSecurityContext` through without
  validating them and applies no tenant scope (P0 ceilings C2/C6).
- Reads are uncached until T36: each call reads the database.
- The new API and the legacy client are independent catalogues until T38: the same linked
  declarations seed both, nothing written through one is visible through the other, and a
  document depending on an entity registered only through the legacy client cannot be
  published here.
- With several registry instances on one database, an admission accepted by one instance can
  wait for another instance's outbox poll, up to a minute. Keep reconciliation deadlines and
  the registry's `worker.operation_timeout` above that.

## Reading

```rust,no_run
use toolkit::client_hub::ClientHub;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsTypeId, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt, Projection, ReadOptions,
};

async fn read(hub: &ClientHub) -> anyhow::Result<()> {
    let registry = hub.get::<dyn PlatformTypesRegistryApi>()?;
    let ctx = PlatformSecurityContext::outbound_marker();
    let type_id = GtsTypeId::try_new("gts.cf.example.events.user_created.v1~")?;

    // Document-free by default: identity, kind, lifecycle and origin.
    let light = registry.get_type_schema(&ctx, &type_id, ReadOptions::default()).await?;

    // Select documents explicitly.
    let with_schema = ReadOptions {
        projection: Projection::Select(FieldSelection::with(&[
            EntityField::Content,
            EntityField::ResolvedSchema,
        ])),
        ..ReadOptions::default()
    };
    let schema = registry.get_type_schema(&ctx, &type_id, with_schema).await?;

    // Bypass a client cache for this read.
    let fresh = registry.get_type_schema(&ctx, &type_id, ReadOptions {
        fresh: true,
        ..ReadOptions::default()
    }).await?;
    Ok(())
}
```

`Projection::Default` is the document-free field set on single and batch reads, while the
list helpers give it their documents (content, and a Type Schema's materializations). An
explicit selection is always kept as given.

Single and batch typed read helpers take `ReadOptions`. Its default uses
`Projection::Default` and `fresh = false`. The options preserve the projection
and apply `fresh` to every request, including each chunk of a batch read. `fresh`
controls the client cache and is not sent on the wire. On a client without a
cache, this flag has no effect.

The `resolve_*` helpers map a registered entity's identifier to its Registry Reference
(`resolve_type_uuid`, `resolve_instance_uuid`) and back (`resolve_type_id`,
`resolve_instance_id`), with plural forms that batch like `batch_get_*`. They take no
options: each always reads the registry, selecting the mandatory fields only through the
normal client cache, and answers from the snapshot read. Active and deleted entities both
resolve. An unregistered identifier, an unknown Registry Reference, or one of the other kind
is `NotFound` on the single helper and `None` in the batch answer; a malformed or wrong-kind
identifier is `InvalidArgument` before any read. When only the deterministic UUID of an
identifier is needed, for instance before it is registered, use `GtsId::to_uuid`, which
reads nothing.

`EntityField::Provenance` selects a managed Type Schema's `provenance` group as a whole:
`TypeSchema::provenance` is `None` when unselected and `Some(Provenance { .. })` when
selected. Its member `compatibility_check_skipped` is a plain `bool`: `true` when
`skip_compatibility_check` waived an applicable cross-minor compatibility check, `false` when
it did not — not a claim that a check ran. `Instance` has no `provenance`.

## Listing

```rust,no_run
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    EntityFilter, GtsIdPattern, ListEntitiesRequest, PlatformTypesRegistryApi,
    PlatformTypesRegistryApiExt,
};

async fn list(registry: &dyn PlatformTypesRegistryApi) -> anyhow::Result<()> {
    let ctx = PlatformSecurityContext::outbound_marker();
    let query = ListEntitiesRequest {
        filter: EntityFilter {
            pattern: Some(GtsIdPattern::try_new("gts.cf.example.*")?),
            ..EntityFilter::default()
        },
        ..ListEntitiesRequest::default()
    };
    let schemas = registry.list_type_schemas(&ctx, query).await?;
    Ok(())
}
```

A listing helper follows every page; past `MAX_LIST_PAGES` it fails with
`ResourceExhausted`. A cursor resumes only the query that issued it.

## Reconciling

[Publishing from a gear](#publishing-from-a-gear) shows a complete call and its outcomes.
Reconciliation creates absent identifiers and updates differing ones; it never deletes. An
identifier listed twice with different documents is `Rejected` without being submitted.
`Ok` means every identifier is admitted. Otherwise `FailedPrecondition` carries a report;
decode it with `ReconcileReport::from_canonical(&error)` for the stop reason and every outcome,
including admitted identifiers. `Rejected` is terminal for the document; `Pending` may settle
on a later call. Caller cancellation and an unrepresentable deadline return `Cancelled` and
`InvalidArgument` without a report.

`ReconcileOptions::batch_size` must fit the registry's configured `limits.batch_candidates`
(both default to 100). A permanent submission refusal stops the call immediately:
`InvalidArgument`, `FailedPrecondition`, `Unauthenticated`, `PermissionDenied`, `NotFound`,
`AlreadyExists`, `OutOfRange`, `Cancelled`, or `ResourceExhausted` without a quota
`retry_after_seconds`. A `ResourceExhausted` with that hint resends the same request under the same
key after at least the longest hint, within the deadline; `Aborted`, `Internal`, `Unknown` and
`ServiceUnavailable` allow up to three attempts per batch. The canonical category and context
decide, the same for local and remote clients, never the HTTP status. Unjudged identifiers keep
earlier outcomes or become `Pending(Skipped)`; accepted writes continue.

## Operations

`register_entities` and `delete_entities` answer the accepted operation; poll it with
`get_operation` until `OperationStatus::Completed`. Each item carries an outcome with exactly
its state's data:

```rust,no_run
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{Operation, PlatformTypesRegistryApi, RegistrationOutcome};

async fn outcomes(
    registry: &dyn PlatformTypesRegistryApi,
    operation_id: uuid::Uuid,
) -> anyhow::Result<()> {
    let ctx = PlatformSecurityContext::outbound_marker();
    if let Operation::Registration(operation) = registry.get_operation(&ctx, operation_id).await? {
        for item in operation.items {
            match item.outcome {
                RegistrationOutcome::Succeeded { resource_version } => {}
                RegistrationOutcome::Unchanged { resource_version } => {}
                RegistrationOutcome::WouldSucceed => {} // a dry run: nothing committed
                RegistrationOutcome::Failed { error } => {}
                RegistrationOutcome::Pending | RegistrationOutcome::Running => {}
            }
        }
    }
    Ok(())
}
```

A deletion item's `DeletionOutcome` has the same states except `Unchanged`.

## Errors

```rust,no_run
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    GtsTypeId, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, ReadOptions,
    TypesRegistryError,
};

async fn classify(
    registry: &dyn PlatformTypesRegistryApi,
    type_id: &GtsTypeId,
) -> anyhow::Result<()> {
    let ctx = PlatformSecurityContext::outbound_marker();
    match registry.get_type_schema(&ctx, type_id, ReadOptions::default()).await {
        Ok(schema) => {}
        Err(error) => match TypesRegistryError::from(error) {
            TypesRegistryError::NotFound { .. } => {}
            TypesRegistryError::Validation { issues } => {}
            other => return Err(other.into()),
        },
    }
    Ok(())
}
```

## Observability

`reconcile_entities_and_await` traces with `tracing` at INFO; nothing needs DEBUG to be
correlated. Every log record has a stable `log_kind` field. Codes only: no document content, and no
free text of an error (`detail`, a violation's description, an admission message). The full
error reaches the caller in the returned `ReconcileOutcome` or `Err`; an accepted operation's
item failures are also persisted and read with `get_operation`. A synchronous refusal, a
conflicting duplicate or a read fault need not have created an operation: when an error names
one (an idempotency conflict, a lost read-back, a deadline while it ran), the batch span
records its `operation_id`; otherwise the caller's error is the only full text.

| Span (INFO) | Fields |
|---|---|
| `reconcile` | `reconcile_id`, `publisher`, `publisher_version`, `inputs` (entries given), `desired` (distinct identifiers), `passes`, `batch_size`, `deadline_ms` |
| `reconcile_batch` | `batch`, `pass`, `size`, `idempotency_key`, `attempt`, `operation_id` (once known) |

| Level | `log_kind` | Meaning |
|---|---|---|
| INFO | `reconcile_started` | the call began |
| INFO | `batch_submitted` | a new payload is about to be sent, under its `idempotency_key`; written before any answer |
| INFO | `batch_accepted` | the registry accepted the batch as `operation_id` |
| WARN | `candidate_refused` | one candidate refused synchronously (`gts_id`, `category`, `reason`, `violation_fields`); the call stops |
| WARN | `batch_refused` | the request as a whole was refused; its candidates stay pending and the call stops |
| WARN | `transport_exhausted`, `incomplete_read`, `item_unreported` | transport or protocol trouble |
| INFO / WARN | `reconcile_finished` | `stop`, `elapsed_ms`, `desired`, `admitted`, `rejected`, `pending`, `undecided`, `passes` |
| WARN | `entity_unsettled` | one per identifier not admitted, on a normal return: `outcome`, `cause`, `category`, `reason`, `dependency_id`, `dependency_kind`, `diagnostic_code`; `rejected` is final for that document, `pending` may settle in a later call |
| DEBUG | `transport_retry` | a transport failure retried under the same key (`category`, `reason`, `backoff_ms`) |

`reconcile_finished` is written exactly once — also when the call is cancelled, its future
dropped (`stop = "dropped"`, with what it had observed and the rest `undecided`) or its
deadline unrepresentable. It is INFO only for `up_to_date`, a `completed` call that admitted
everything, and cancellation by the caller's token; anything else is WARN. `stop` is
`up_to_date`, `completed`, `passes_exhausted`, `deadline`, `read_failed`, `invalid_input`, `cancelled`,
`invalid_deadline`, `dropped` or `panicked`.

The registry writes INFO `operation_accepted` once an operation is durable — `operation_id`,
`kind`, `dry_run`, `items`, `replayed` — with `idempotency_key` when the key is a canonical
UUID, as the SDK generates (any other key is logged as `idempotency_key_kind` and
`idempotency_key_len` only).

**Investigating a call.** Filter by `reconcile_id`. `reconcile_finished` and the
`entity_unsettled` rows say what ended how. Each `batch_submitted` names its batch's
`idempotency_key`, before any answer; the registry's `operation_accepted` with that key gives the
`operation_id`, even when the acknowledgement never reached the client (a replay is
`replayed = true`). The registry's `types_registry.admission.operation` and `unit` spans carry
that `operation_id` and each candidate's outcome, and `get_operation` reads an admission
refusal's full text. Rows from a synchronous refusal (`candidate_refused`,
`batch_refused`), a conflicting duplicate or `read_failed` may name no operation; their full
error is the one the caller received. With the local client the registry's acceptance runs inside the caller's spans, so its
event carries `reconcile_id` directly.

## Testing (`test-util`)

- `testing_platform::MockTypesRegistry` implements both contracts in memory. Seed it
  (`seed`, `Seed`, `seed_inventory`) or write through the contract; Type Schema documents
  resolve with `gts-rust` as admission does. `Fault` delays or fails calls, `reject` and
  `depends_on` steer admission, `strict()` panics on an unexpected call, and `calls`,
  `reads` and `submissions` record what happened. `install` registers it in a `ClientHub`
  under both contracts.
- `testing_platform::conformance::run` holds the contract checks that the mock, the
  registry's local client and the REST client over TCP all pass.

## Legacy API

**Removed at T38.** Existing consumers still use it until they migrate; new code uses the
contracts above. `TypesRegistryClient` reads and registers through the registry's in-memory
catalogue, which is independent of the database the new contracts serve. Its models
(`GtsTypeSchema`, `GtsInstance`, `AncestorIter`, `RegisterResult`, `RegisterSummary`, …) and
`testing::MockTypesRegistryClient` remain at the crate root until then.

```rust,no_run
use toolkit::client_hub::ClientHub;
use types_registry_sdk::TypesRegistryClient;

async fn legacy(hub: &ClientHub) -> anyhow::Result<()> {
    let client = hub.get::<dyn TypesRegistryClient>()?;
    let schema = client
        .get_type_schema("gts.cf.example.events.user_created.v1~")
        .await?;
    Ok(())
}
```

## License

Apache-2.0

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
(`get_type_schema`, `get_instance`, their `_by_uuid` and `batch_get_*` variants) and
complete listings (`list_type_schemas`, `list_instances`). The platform extension also
reconciles explicit documents with `reconcile_entities_and_await`. Import the extension
trait to call them; with both imported, choose one by UFCS.

The GTS identifier types the models use (`GtsId`, `GtsTypeId`, `GtsInstanceId`,
`GtsIdPattern`, `GtsIdSegment`) are re-exported, so building a request needs no direct
`gts` dependency.

## Reading

```rust
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::{
    GtsTypeId, PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, Projection,
};

let registry = hub.get::<dyn PlatformTypesRegistryApi>()?;
let ctx = PlatformSecurityContext::outbound_marker();
let type_id = GtsTypeId::try_new("gts.acme.core.events.user_created.v1~")?;

// Document-free by default: identity, kind, lifecycle and origin.
let light = registry.get_type_schema(&ctx, &type_id, Projection::Default).await?;

// Select documents explicitly.
use types_registry_sdk::{EntityField, FieldSelection};
let with_schema = Projection::Select(FieldSelection::with(&[
    EntityField::Content,
    EntityField::ResolvedSchema,
]));
let schema = registry.get_type_schema(&ctx, &type_id, with_schema).await?;
```

`Projection::Default` is the document-free field set on single and batch reads, while the
list helpers give it their documents (content, and a Type Schema's materializations). An
explicit selection is always kept as given.

## Listing

```rust
use types_registry_sdk::{EntityFilter, GtsIdPattern, ListEntitiesRequest};

let query = ListEntitiesRequest {
    filter: EntityFilter {
        pattern: Some(GtsIdPattern::try_new("gts.acme.core.*")?),
        ..EntityFilter::default()
    },
    ..ListEntitiesRequest::default()
};
let schemas = registry.list_type_schemas(&ctx, query).await?;
```

A listing helper follows every page; past `MAX_LIST_PAGES` it fails with
`ResourceExhausted`. A cursor resumes only the query that issued it.

## Reconciling

```rust
use serde_json::json;
use tokio_util::sync::CancellationToken;
use types_registry_sdk::{
    GtsId, PublisherContext, ReconcileOptions, ReconcileOutcome, Reconciliation,
};

let publisher = PublisherContext {
    name: "acme-events".to_owned(),
    version: env!("CARGO_PKG_VERSION").parse()?,
};
let desired = vec![(
    GtsId::try_new("gts.acme.core.events.user_created.v1~")?,
    json!({
        "$id": "gts://gts.acme.core.events.user_created.v1~",
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object"
    }),
)];
let options = ReconcileOptions::default();

let reconciliation = registry
    .reconcile_entities_and_await(&ctx, &publisher, &desired, &options, &CancellationToken::new())
    .await?;
// `Ok` does not mean admitted: only `UpToDate` or all-`Admitted` lets a dependent start.
if let Reconciliation::Reconciled(outcomes) = &reconciliation {
    let unsettled: Vec<_> = outcomes
        .iter()
        .filter(|(_, outcome)| !matches!(outcome, ReconcileOutcome::Admitted))
        .collect();
    if !unsettled.is_empty() {
        anyhow::bail!("types not admitted: {unsettled:?}");
    }
}
```

Reconciliation creates absent identifiers and updates differing ones; it never deletes. An
identifier listed twice with different documents is `Rejected` without being submitted.
`Rejected` is terminal for the document. `Pending` is not: a dependency, a concurrent
writer, an unreachable registry or a refused call, which a later call may settle. The call
itself fails only on an unrepresentable deadline or when the caller's token is cancelled, so
check every outcome.

## Operations

`register_entities` and `delete_entities` answer the accepted operation; poll it with
`get_operation` until `OperationStatus::Completed`. Each item carries an outcome with exactly
its state's data:

```rust
use types_registry_sdk::{Operation, RegistrationOutcome};

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
```

A deletion item's `DeletionOutcome` has the same states except `Unchanged`.

## Errors

```rust
use types_registry_sdk::TypesRegistryError;

match registry.get_type_schema(&ctx, &type_id, Projection::Default).await {
    Ok(schema) => {}
    Err(error) => match TypesRegistryError::from(error) {
        TypesRegistryError::NotFound { name, .. } => {}
        TypesRegistryError::Validation { issues } => {}
        other => return Err(other.into()),
    },
}
```

## Testing (`test-util`)

- `testing_platform::MockTypesRegistry` implements both contracts in memory. Seed it
  (`seed`, `Seed`, `seed_inventory`) or write through the contract; Type Schema documents
  resolve with `gts-rust` as admission does. `Fault` delays or fails calls, `reject` and
  `depends_on` steer admission, `strict()` panics on an unexpected call, and `calls`,
  `reads` and `submissions` record what happened. `install` registers it in a `ClientHub`
  under both contracts.
- `testing_platform::conformance::run` holds the contract checks that the mock and the
  registry's local client both pass.

## Legacy client

`TypesRegistryClient`, its models (`GtsTypeSchema`, `GtsInstance`, `AncestorIter`, …) and
`testing::MockTypesRegistryClient` remain at the crate root while consumers migrate. New
code should use the contracts above.

## License

Apache-2.0

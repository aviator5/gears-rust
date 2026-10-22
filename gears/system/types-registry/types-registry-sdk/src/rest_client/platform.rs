//! [`PlatformTypesRegistryApi`] over `/types-registry/platform/v1/` (SPEC §10.1, D17, D19).
//!
//! Hand-written on toolkit-contract's runtime helpers rather than `#[rest_contract]`:
//! submissions need the `Idempotency-Key` header and the receipt's status and headers,
//! which a generated method neither sends nor returns. Every attempt carries the process's
//! internal token, resolved afresh, and never a bearer: the `PlatformSecurityContext`
//! argument is a plane marker and is never serialized.

use std::time::Duration;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::client::build_default_http_client;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_contract::runtime::http::{attach_internal_token, map_http_error, parse_retry_after};
use toolkit_contract::runtime::retry::retry_with_backoff;
use toolkit_contract::runtime::transport_error::TransportError;
use toolkit_http::{HttpClient, HttpError, RequestBuilder};
use toolkit_security::PlatformSecurityContext;
use tracing::Instrument;
use uuid::Uuid;

use super::dto::{
    AcceptedDto, BatchGetBody, DeleteBody, LookupsDto, OperationDto, PageDto, SubmitBody,
    discovery_query, parse,
};
use crate::contract::PlatformTypesRegistryApi;
use crate::gts::OperationResource;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, DeleteEntitiesRequest, DeletionOperation,
    IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse, Operation, OperationStatus,
    RegisterEntitiesRequest, RegistrationOperation,
};
use crate::reason;

/// The platform API's base path; the registry serves it on every host (D17).
pub const PLATFORM_BASE_PATH: &str = "/types-registry/platform/v1";

/// The `rpc.service` of every span, as a generated client names its trait.
const SERVICE: &str = "PlatformTypesRegistryApi";

/// The platform contract over REST.
///
/// Every method is a safe read or carries an `Idempotency-Key`, so every attempt may be
/// retried under [`ClientConfig::retry`], each within [`ClientConfig::timeout`]. A mutation
/// returns its operation as read back through [`get_operation`](Self::get_operation)
/// (D19); a failed read-back names the accepted operation, so the caller can replay the
/// same key.
pub struct PlatformTypesRegistryRestClient {
    http: HttpClient,
    config: ClientConfig,
}

impl PlatformTypesRegistryRestClient {
    /// A client over the default `toolkit-http` transport, as a generated client builds it.
    ///
    /// # Errors
    /// Whatever the `toolkit-http` builder returns.
    pub fn new(config: ClientConfig) -> Result<Self, HttpError> {
        let http = build_default_http_client(SERVICE, &config)?;
        Ok(Self { http, config })
    }

    /// A client over a caller-built transport; only `config`'s call-time fields apply.
    #[must_use]
    pub fn with_http_client(http: HttpClient, config: ClientConfig) -> Self {
        Self { http, config }
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}{PLATFORM_BASE_PATH}{path}",
            self.config.base_url.trim_end_matches('/')
        )
    }

    /// One exchange under the retry policy: every attempt is built afresh and carries a
    /// freshly resolved internal token. A non-success status is a `TransportError` carrying
    /// the registry's Problem; a success keeps its status and the headers the caller reads.
    async fn exchange(
        &self,
        rpc: &'static str,
        build: impl Fn(&HttpClient) -> Result<RequestBuilder, TransportError>,
    ) -> Result<Answer, TransportError> {
        retry_with_backoff(&self.config.retry, || async {
            let builder = attach_internal_token(
                build(&self.http)?,
                self.config.internal_token_provider.as_ref(),
                rpc,
            );
            let attempt = async {
                let response = builder.send().await.map_err(map_send_error)?;
                let status = response.status().as_u16();
                let headers = response.headers();
                let retry_after = parse_retry_after(headers);
                let location = headers
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let body = response.bytes().await.map_err(TransportError::network)?;
                if (200..300).contains(&status) {
                    Ok(Answer {
                        status,
                        location,
                        retry_after,
                        body,
                    })
                } else {
                    Err(map_http_error(
                        status,
                        String::from_utf8_lossy(&body).into_owned(),
                        retry_after,
                    ))
                }
            };
            tokio::time::timeout(self.config.timeout, attempt)
                .await
                .map_err(|_| TransportError::Timeout(self.config.timeout))?
        })
        .await
    }

    /// `GET …/operations/{id}`, decoded.
    async fn fetch_operation(&self, operation_id: Uuid) -> Result<Operation, Fetch> {
        let url = self.url(&format!("/operations/{operation_id}"));
        let answer = self
            .exchange("get_operation", |http| Ok(http.get(&url)))
            .await
            .map_err(Fetch::Transport)?;
        parse::<OperationDto>("operation", &answer.body)
            .and_then(OperationDto::decode)
            .and_then(|operation| requested(operation_id, operation))
            .map_err(Fetch::Malformed)
    }

    /// Submit under `key`, then read the accepted operation back (D19).
    async fn submit(
        &self,
        rpc: &'static str,
        path: &str,
        key: &IdempotencyKey,
        body: &(impl serde::Serialize + Sync),
    ) -> Result<Operation, CanonicalError> {
        let url = self.url(path);
        let answer = self
            .exchange(rpc, |http| {
                http.post(&url)
                    .header(IDEMPOTENCY_KEY_HEADER, key.as_str())
                    .json(body)
                    .map_err(TransportError::serialization)
            })
            .await?;
        let receipt = Receipt::read(&answer)?;
        if let Some(pause) = receipt.pause(self.config.retry.max_delay) {
            tokio::time::sleep(pause).await;
        }
        let operation_id = receipt.operation_id;
        match self.fetch_operation(operation_id).await {
            Ok(operation) => Ok(operation),
            Err(Fetch::Transport(error)) => {
                let error = CanonicalError::from(error);
                tracing::warn!(
                    %operation_id,
                    %error,
                    "types_registry could not read back an accepted operation"
                );
                Err(read_back_failed(operation_id))
            }
            Err(Fetch::Malformed(error)) => Err(unrepresentable(operation_id, &error)),
        }
    }
}

const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";

/// A success: its status, the receipt headers and the body.
struct Answer {
    status: u16,
    location: Option<String>,
    retry_after: Option<Duration>,
    body: bytes::Bytes,
}

/// Why an operation read failed: the exchange, or what it returned.
enum Fetch {
    Transport(TransportError),
    Malformed(CanonicalError),
}

/// A submission's receipt, checked against the status and headers it came with.
struct Receipt {
    operation_id: Uuid,
    status: OperationStatus,
    retry_after: Option<Duration>,
}

impl Receipt {
    /// `202` for a new or still running operation, `200` only for a terminal replay; the
    /// `Location` names the operation. Anything else the registry never sends.
    fn read(answer: &Answer) -> Result<Self, CanonicalError> {
        let accepted: AcceptedDto = parse("receipt", &answer.body)?;
        let status = accepted.status();
        let terminal_replay = accepted.replayed && status == OperationStatus::Completed;
        let consistent = match answer.status {
            200 => terminal_replay,
            202 => !terminal_replay,
            _ => false,
        };
        let names_it = answer.location.as_deref().is_some_and(|location| {
            location.ends_with(&format!("/operations/{}", accepted.operation_id))
        });
        if !consistent || !names_it {
            tracing::error!(
                operation_id = %accepted.operation_id,
                http_status = answer.status,
                location = ?answer.location,
                "types_registry REST client received an inconsistent receipt"
            );
            return Err(unrepresentable(
                accepted.operation_id,
                &format_args!("an inconsistent {} receipt", answer.status),
            ));
        }
        Ok(Self {
            operation_id: accepted.operation_id,
            status,
            retry_after: answer.retry_after,
        })
    }

    /// The advised wait before the first read of an operation still in progress, bounded
    /// like any retry delay. A terminal one is read at once.
    fn pause(&self, ceiling: Duration) -> Option<Duration> {
        (self.status != OperationStatus::Completed)
            .then_some(self.retry_after?)
            .map(|advised| advised.min(ceiling))
            .filter(|pause| !pause.is_zero())
    }
}

fn map_send_error(err: HttpError) -> TransportError {
    match err {
        HttpError::Overloaded => TransportError::Overloaded,
        HttpError::Timeout(d) | HttpError::DeadlineExceeded(d) => TransportError::Timeout(d),
        other => TransportError::network(other),
    }
}

/// The local client's refusal after acceptance: retry with the same key (D19).
fn read_back_failed(operation_id: Uuid) -> CanonicalError {
    OperationResource::aborted(format!(
        "operation {operation_id} was accepted, but the read failed; retry with the same \
         idempotency key"
    ))
    .with_resource(operation_id.to_string())
    .with_reason(reason::aborted::OPERATION_READ_FAILED)
    .create()
}

/// `operation` if it is the one asked for; another one is an answer to another question.
fn requested(operation_id: Uuid, operation: Operation) -> Result<Operation, CanonicalError> {
    if operation.operation_id() == operation_id {
        return Ok(operation);
    }
    tracing::error!(
        asked = %operation_id,
        answered = %operation.operation_id(),
        "types_registry REST client read another operation than it asked for"
    );
    Err(CanonicalError::internal("the registry sent a malformed operation").create())
}

/// An accepted operation the registry described in a shape it never sends.
fn unrepresentable(operation_id: Uuid, error: &dyn std::fmt::Display) -> CanonicalError {
    tracing::error!(%operation_id, %error, "types_registry cannot represent an accepted operation");
    OperationResource::unknown(format!(
        "operation {operation_id} was accepted, but its record could not be represented"
    ))
    .with_resource(operation_id.to_string())
    .create()
}

/// An accepted operation that read back as the other kind.
fn wrong_kind(operation_id: Uuid) -> CanonicalError {
    tracing::error!(%operation_id, "types_registry read back an operation of the other kind");
    OperationResource::unknown(format!(
        "operation {operation_id} was accepted, but read back as an operation of an \
         unexpected kind"
    ))
    .with_resource(operation_id.to_string())
    .create()
}

/// The span a generated client opens for one method.
macro_rules! client_span {
    ($method:literal, $http_method:literal, $route:literal) => {
        tracing::info_span!(
            concat!("PlatformTypesRegistryApi.", $method),
            otel.kind = "client",
            rpc.system = "rest",
            rpc.service = SERVICE,
            rpc.method = $method,
            http.method = $http_method,
            http.route = concat!("/types-registry/platform/v1", $route),
            error = tracing::field::Empty,
        )
    };
}

/// Run `call` inside `span`, recording a failure on it as a generated client does.
async fn traced<T>(
    span: tracing::Span,
    call: impl Future<Output = Result<T, CanonicalError>>,
) -> Result<T, CanonicalError> {
    async {
        let result = call.await;
        if result.is_err() {
            tracing::Span::current().record("error", true);
        }
        result
    }
    .instrument(span)
    .await
}

#[async_trait]
impl PlatformTypesRegistryApi for PlatformTypesRegistryRestClient {
    async fn batch_get_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        let span = client_span!("batch_get_entities", "POST", "/entities:batchGet");
        traced(span, async {
            let body = BatchGetBody::encode(&request)?;
            let url = self.url("/entities:batchGet");
            let answer = self
                .exchange("batch_get_entities", |http| {
                    http.post(&url)
                        .json(&body)
                        .map_err(TransportError::serialization)
                })
                .await?;
            parse::<LookupsDto>("batch read", &answer.body)?.decode()
        })
        .await
    }

    async fn list_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        let span = client_span!("list_entities", "GET", "/entities");
        traced(span, async {
            let query = toolkit_contract::query::to_query_string(&discovery_query(&request))
                .map_err(|e| TransportError::serialization(e.to_string()))?;
            let url = match query {
                Some(query) => format!("{}?{query}", self.url("/entities")),
                None => self.url("/entities"),
            };
            let answer = self
                .exchange("list_entities", |http| Ok(http.get(&url)))
                .await?;
            parse::<PageDto>("discovery page", &answer.body)?.decode()
        })
        .await
    }

    async fn register_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
    ) -> Result<RegistrationOperation, CanonicalError> {
        let span = client_span!("register_entities", "POST", "/entities");
        traced(span, async {
            let body = SubmitBody::encode(request)?;
            match self
                .submit("register_entities", "/entities", &key, &body)
                .await?
            {
                Operation::Registration(operation) => Ok(operation),
                // `fetch_operation` checked it is the accepted operation.
                Operation::Deletion(operation) => Err(wrong_kind(operation.operation_id)),
            }
        })
        .await
    }

    async fn delete_entities(
        &self,
        _ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: DeleteEntitiesRequest,
    ) -> Result<DeletionOperation, CanonicalError> {
        let span = client_span!("delete_entities", "POST", "/entities:batchDelete");
        traced(span, async {
            let body = DeleteBody::encode(request)?;
            match self
                .submit("delete_entities", "/entities:batchDelete", &key, &body)
                .await?
            {
                Operation::Deletion(operation) => Ok(operation),
                Operation::Registration(operation) => Err(wrong_kind(operation.operation_id)),
            }
        })
        .await
    }

    async fn get_operation(
        &self,
        _ctx: &PlatformSecurityContext,
        operation_id: Uuid,
    ) -> Result<Operation, CanonicalError> {
        let span = client_span!("get_operation", "GET", "/operations/{operation_id}");
        traced(span, async {
            match self.fetch_operation(operation_id).await {
                Ok(operation) => Ok(operation),
                Err(Fetch::Transport(error)) => Err(error.into()),
                Err(Fetch::Malformed(error)) => Err(error),
            }
        })
        .await
    }
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod platform_tests;

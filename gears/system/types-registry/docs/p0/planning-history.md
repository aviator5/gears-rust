# Types Registry P0 — Planning history

This document preserves decisions P1–P31 verbatim in their **original task numbering**.
Those IDs are historical and must not be used as the current execution queue.
The current tasks and dependencies are in [plan.md](./plan.md) and [todo.md](./todo.md).
P32 below records the one-time conversion to the current numbering.

## Decisions taken during planning

These decisions were made here rather than in the spec, because all of them are consequences
of task ordering or of facts about the runtime that only surface once the work is sliced.
P1–P5 were taken before implementation started; P6–P10 came out of reviewing Phase 1 on its way
in, and the spec has been updated to match all five. P12 is a correction: it reverses a change T9
made to the existing v1 REST contract, and adds T9a and T27. P13 is a reordering — Instances
into Phase 1, and `make dylint` per phase instead of per task. P14 defines `x-gts-ref`
independently of the dependency graph. P15 replaces the revision-vector lock with the optimistic
guard and keeps the one thing that survives the argument: a serialized write path — one row,
claimed first by every commit — that orders commits, joined by every writer of entity state:
admission here, deletion at T20, purge under ADR-0013. P16 came out of reviewing Phase 3:
observability is a per-task obligation from T17 onward. P17, revised after T20, splits REST
completion into T20a (mutations, Phase 5) and T22a (reads, Phase 6), and moves T21 outbox
dispatch into Phase 5. P18 supersedes P4’s P0 scope: inventory push moves to P1, while
explicit-document reconciliation remains in P0. (P11 was a housekeeping close-out and is retired; the number is not reused.)
P19 adds `$select` across the three read routes before T23 fixes the SDK shape and T22d
adds validators; it supersedes P10's arbitrary-projection deferral, not P10's bounded
content-free discovery default.
P20 adds `depth` and `kind` to P0 discovery before T23 fixes `ListEntitiesRequest`; it supersedes
T22a's historical filter limit while keeping tenancy, availability and federation deferred.
P21 moves the validator task into Phase 6 as T22d (formerly T29 (retired)) and T23 into Phase 7, so Checkpoint 6 closes the complete P0 REST
contract and the SDK maps a computed validator rather than declaring an unfilled one.
P22 brings out-of-process operation into P0. It supersedes P18's deferral of per-gear push,
P4's `owning_gear` filtering and P8's stale codegen premise, adds T24 and T25, splits Phase 7,
and renumbers the open tasks.

### P1. The spec's §15 build order is replaced by vertical slices

§15 orders work horizontally — schema, then repositories, then "synchronous admission
core", then operations. Its slice 3 explicitly builds a synchronous admission path that
slice 6 then converts to the asynchronous one. That is the same work twice and throws away
slice 3's tests, so it is not followed.

Instead each phase from 1 onward delivers **one complete registration path**, async-shaped
from the first commit. Phase 1 registers a single dependency-free global Type Schema
end to end: migration → entity → repository → transient store → acceptance → worker → REST.
Later phases widen that path without reshaping it.

No decision from SPEC §3 changes. Only the order does.

### P2. Types-registry seeds itself through its own outbox

types-registry owns the `toolkit-gts` base types and its own control-plane types (DESIGN:
*"`toolkit-gts` base types default to `types-registry` ownership"*). It cannot register
those through a client — it *is* the registry, same process, same database.

`init()` collects one deterministic seed batch and submits it like any other request, so
acceptance and enqueue share a transaction and no seed operation exists without a driver.
It then awaits that operation and requires every item `succeeded` or `unchanged` before
publishing the client; a `failed` item fails boot. Seeding is therefore deterministic and
complete at publication, which is what makes the §13 no-polling rule satisfiable. At T24b,
P28 brings the seed set of all process-linked inventory plus `cfg.entities` into the
database. P22 makes that transitional: once every declaring gear publishes for itself,
T37 narrows the seed back to the
registry's own types, the `toolkit-gts` base types and `cfg.entities`.

### P3. The outbox worker starts inside types-registry's `init()`, before seeding

Verified phase order (`libs/toolkit/src/runtime/host_runtime.rs:6-15`):

```
pre_init (system) → DB migrations → init (all gears) → proxy-wiring
→ post_init (system) → REST wiring → gRPC → start/stop (stateful)
```

`init` of **every** gear precedes `start` of **any**. A worker living in the stateful
`start` entry would therefore not exist while consumers are initializing — and ~13 existing
call sites already register from their own `init()` and block on the result. Those would
hang.

That is avoidable. `GearCtx` exposes `cancellation_token()`
(`libs/toolkit/src/context.rs:166`) and `OutboxHandle::stop()` is an ordinary async
shutdown (`libs/toolkit-db/src/outbox/manager.rs:620`), so the worker starts from `init()`
with correct cancellation wiring and stops from the stateful `stop`. `init` order is
topological and types-registry is a declared dependency of its consumers, so its worker is
live before any consumer's `init()` body runs.

**Order inside types-registry's `init()`:** repositories → start the outbox worker →
seed the seed set (P2/P18) → await its items → publish the client. The worker starts
first because acceptance enqueues in its own transaction; publication requires every
seed item `succeeded` or `unchanged`, and a `failed` one fails boot. There is
no snapshot-load step: per P6 seeding builds its own transient store like any other
admission, and reads go to the database.

**Current sequencing (P28):** after types-registry's `init()`, a local consumer
with `deps = [types_registry]` may submit and await through the new API from T24b.
Remote clients are wired only after every `init`, so remote consumers use
`#[consumes]` and publish after wiring. T29 supplies that hook; T35–T37 move all
remaining transitional local calls out of `init()` as well.

**Who admits, and when.** Acceptance and admission are separate moments with separate
executors:

| Moment | Accepts | Admits |
|---|---|---|
| types-registry `init()` — its seed set: linked inventory + `cfg.entities` from T24b until T37, then its own types, the base types and independent `cfg.entities` (P18, P22, P28) | types-registry, after its worker starts | the outbox worker; `init()` awaits it before publishing the client (P2) |
| A transitional local consumer's `init()` (T24b–T37) | registry code reached through the local client | the already-running outbox worker; the consumer awaits bounded reconciliation |
| A gear publishes its declarations after wiring | registry code in the caller's task (local client), or the registry's REST handler (remote client) | the outbox worker |
| REST at runtime | types-registry's Axum handler | the outbox worker |

Acceptance is always synchronous, in the caller's task. Admission is performed by exactly
one outbox worker owned by types-registry — one in the system for a single-binary
deployment.

### P4. Registration moves from pull to push — P0 scheduling superseded by P18, then by P22

**Historical decision.** The rationale below describes the original plan. P18 moved this
inventory migration to P1; P22 brings push back into P0 in a different form — per-crate
collectors and explicit crate ownership instead of `owning_gear` filtering.

A gear does not know whether it runs in-process or out of process, and its code must not
depend on that. The registry-side **pull** violates this in the worst way: a gear's code is
unchanged, but its types silently vanish if it moves out of process, because
`all_inventory_type_schemas()` only sees `inventory` records linked into *this* binary.

The platform already runs the transparent mechanism for half of this. Roughly eleven plugin
gears already **push** their well-known Instances by calling `registry.register(...)` from
their own `init()` through the ClientHub-resolved SDK trait — in-process that is the local
client, out of process it would be a gRPC client, and the gear's code is identical either
way. Only inventory-declared *schemas* still travel by pull.

P0 therefore moves schemas onto the same path, as DESIGN already specifies: *"The SDK
filters records by `owning_gear` and reconciles them, replacing the current registry-side
process-wide pull with a per-gear push that works across processes."*

- types-registry seeds **only what it owns** — `toolkit-gts` base types and its own
  control-plane types — inline, per P2.
- Every other gear reconciles its own declarations with **one SDK call**. The five-step
  reconciliation workflow of DESIGN §3.3 lives in the SDK, not in each gear, so no gear
  hand-rolls batching, idempotency or retry.
- `InventoryTypeSchema` / `InventoryInstance` gain `owning_gear`, derived from the declaring
  crate's gear name, so the SDK can filter and attribution stops being a constant.
- **`cfg.entities` is outside P4's scope.** It carries operator-controlled identities whose
  GTS identifiers are deployment-specific and cannot be expressed as gear-owned inventory items
  (e.g. the platform-root tenant type). These are seeded into the database at startup through
  the same outbox admission path as types-registry's own inventory (T26). They are not
  reconciled through the SDK — no gear owns them; the deployment operator does.

**This is where registrant-side retry becomes real** — and it lives in the SDK helper. The
earlier answer that no retry was needed was conditional on keeping pull.

It also simplifies the cutover. Seeding no longer has to topologically order ~200 entities
across every gear at startup; types-registry seeds its own small set, and cross-gear
ordering is handled by the retry DESIGN sanctions: *"dependencies converge through retry."*

Cost: a `toolkit-gts` and macro change, plus one line in roughly fifteen gears. Benefit:
ceiling C3 disappears, out-of-process operation is unblocked, and the transparency
requirement actually holds.

### P5. The old SDK trait is removed in P0, not deprecated alongside the new one

Taken here first; SPEC **D6 now records it**, so the two agree. The version of D6 this
replaced kept both traits and deferred consumer migration to a separate commit.

Two facts forced the change. First, async admission makes the old synchronous
`register(Vec<Value>) -> Vec<RegisterResult>` unrepresentable: thirteen call sites call
`RegisterResult::ensure_all_ok(&results)` immediately and would start reading `pending` as
success. Keeping the old trait means keeping a blocking submit-then-await adapter behind a
signature that no longer describes what happens. Second, the old models cannot cross a wire
at all — `GtsTypeSchema.parent: Option<Arc<GtsTypeSchema>>` and
`GtsInstance.type_schema: Arc<GtsTypeSchema>` are in-process object graphs — so retaining
them retains an out-of-process blocker. P0 removes that model blocker; inventory push
from P4 is now deferred to P1 by P18.

So the old trait goes, and every consumer migrates inside P0. The real surface is larger
than the thirteen register sites: reads dominate (`list_instances` ~59 references,
`get_type_schema_by_uuid` ~31, `get_type_schemas_by_uuid` ~28), for roughly fifty files
across twenty-plus gears. Migration is split by gear group across two tasks so no single
task carries it all.

### P6. No store is held between admissions: transient store, reads from the database

This replaces the original SPEC D2 and §8.2, both of which have been rewritten. It changes
T5 and T8; nothing else in the graph moves.

The original shape was one immutable `ArcSwap` snapshot of the `gts-rust` store, loaded from
the whole entity table at init and rebuilt after every successful admission unit, serving
both semantic evaluation and consumer reads. Reviewing T5 before building it surfaced that
this conflates two needs with different answers.

**What actually needs a `GtsStore`** is semantic computation over related documents —
resolution, `compare_documents`, derivation chains, instance validation. All of it happens
inside admission, over one candidate and what that candidate consumes. That set is exactly
the dependency closure, which the `dependency` table already supplies (D5).

**What reads need is rows.** Pattern matching is a pure function of the parsed
identifier — it never asks a store a semantic question. Exact reads are keyed lookups. And
D3 already materializes `resolved_schema` / `effective_traits` / `effective_traits_schema`
on the current-state row. So a read is a `SELECT`; for discovery, one `SELECT` whose
pattern joins the admission-time parsed segments (P20, SPEC D14).

**And the snapshot was not merely unnecessary for reads, it was wrong for them.** SPEC §13
requires *"two pods, commit on A, B's first post-commit read sees it"*
(`nfr-multi-pod-correctness`). P0 has no cross-pod invalidation — no pub/sub, and the outbox
belongs to the committing pod — so B would serve its stale snapshot indefinitely. Admission
survives stale input because of the commit-time revision-vector guard (D4, T15); reads have
no guard, so for them staleness is simply incorrect. This would have shipped as a passing
implementation of a failing criterion.

Consequences, including the cost:

- Reads become a database round trip, which is what the SDK client cache is for. **The
  earlier decision to delete that cache is reversed** — see P7.
- Ceilings **C1** (whole entity set in memory) and **C4** (startup reads the whole table) are
  retired rather than deferred: there is no warm-up read and no process-lifetime store.
- `Mutex<GtsOps>` disappears instead of being replaced. A store owned by one worker
  invocation is never shared, so `GtsOps` not being `Sync` stops being a design constraint.
- The transient store is *cheaper* than what it replaces: the snapshot cost a full rebuild
  after every successful unit; the closure is bounded by the candidate's own dependencies.

### P7. The SDK client cache is kept, not deleted — DESIGN requires it

This reverses a removal both earlier revisions of the spec carried, and it adds **T30**.
SPEC §8.3 is new and records the contract.

DESIGN requires a client cache outright: `cpt-cf-types-registry-fr-client-cache`, *"bounded
per-client representation cache with batched conditional revalidation and fail-closed expiry
handling"*, with the full contract in DESIGN §3.3. The removal rested on two claims, and
neither holds.

The first was ours and expired with P6: *"an LRU in front of an in-memory snapshot is pure
overhead plus a staleness window."* True while reads came from memory; once reads are a
database round trip, a cache buys what it costs.

The second was a misreading, and worth naming precisely because it nearly shipped.
`nfr-cache-correctness` forbids *"an invalidated result accepted as current after the client
observes the mutation"* — it does not forbid a freshness window, and DESIGN §3.3 says so in
as many words: *"a remote mutation not yet observed may produce a stale snapshot within the
bounded window but is not described as an invalidated entry accepted as current."* The window
is the sanctioned trade. And it is a different NFR from `nfr-multi-pod-correctness`, which
governs the **registry's** reads — *"no process-local authority"* — and which P6 satisfies.
Two NFRs about two different sides of the wire were treated as one.

So P0 builds DESIGN's cache minus what needs absent inputs: bounded store, freshness window,
`fresh` bypass, invalidation on an observed terminal outcome, dual identifier/UUID indexing,
and DESIGN's list of what is never cached. Batched conditional revalidation against freshness
validators was first deferred here with tenancy, but P9 moves it into P0 once the validator
inputs are shown to be platform-plane computable. Deferred with tenancy, then, are only the
projection / visibility / Context-Tenant key dimensions. Recorded as ceiling **C7** rather
than left implicit.

Two things this changes beyond the decision:

- **The bound becomes bytes.** Today's default is `capacity: 1024` entries while §3.2 caps
  one resolved document at 1 MB, so the configured bound permits ~1 GB. DESIGN makes the
  same argument and picks 64 MB; adopted. This is a live bug in the current defaults, not a
  new requirement.
- **The cache cannot be carried over as-is.** It is typed on `GtsTypeSchema` / `GtsInstance`,
  which P5 deletes, so it is ported onto `Entity`.

**Ordering.** T30 lands last, after the cutover rather than with it, and that is deliberate:
the cache is an optimization over a read path that must be correct first, and T26 is already
the largest task in the plan. *(Superseded by P23, variant C′: the cache — now T27 — lands **before** the cutover,
on the new SDK, so consumers moved by T26 are cached from their first read and P0 has no
uncached window.)*

### P8. P0 ships the platform-plane API on the business listener; the plane is contract-deep, not transport-deep

SPEC §8.4 is rewritten and ceiling **C8** is added. No task moves; T9, T20a and T22a carry the criteria.

> **Partly superseded by P22.** The macro no longer rejects `&PlatformSecurityContext`, so the
> "later gRPC move" below does not follow; the SDK goes out of process over a hand-written REST
> client. Per-route caller authentication (T24) replaces "the routes keep the authentication
> they have", which would refuse every remote gear's call.

Registering a global entity is a platform-level operation, and P0 already treats it as one in
the data — §8.1 writes `plane = 1`, `tenant_id = NULL` on every operation record. The earlier
§8.4 nonetheless marked "Tenant REST" as the P0 surface and deferred everything platform,
which left the spec claiming a tenant-plane transport for platform-plane rows. So: **the P0
REST surface and SDK are the platform-plane API for global entities** — async registration and
reads, no tenant ownership — and that is what the e2e suites exercise.

The plane is not enforced by the transport, because the platform offers an in-process gear no
way to do it. Verified in the code, not assumed:

- `internal_auth_middleware` — inbound platform plane over HTTP — is installed only in
  `libs/toolkit/src/runtime/oop_serve.rs:390`, the per-gear server for a gear running *out of*
  process. api-gateway's `internal_auth` is an outgoing gRPC credential for DirectoryService
  (`gears/system/api-gateway/src/gear.rs:823-826`), not an inbound validator.
- api-gateway has one API listener; its only second listener is for health probes. ADR-0006/0008
  ask for a separate platform listener, which nothing implements.
- `OperationBuilder` has no `.platform()`, and the middleware is permissive — a missing token
  passes with no `PlatformSecurityContext` (`toolkit-http-middleware/src/auth.rs:220`).

**The routes therefore keep the authentication they have.** Switching them to `.anonymous()`
to signal "not tenant traffic" would be a security regression, not progress: with no platform
identity available it would let anything reaching the gateway register a global type. Same
shape as the PDP deviation — authenticated, not authorized, gap named (C6, now C8).

One trap avoided: advice to serve platform routes `.anonymous()` and **not** `.exposed()` is
written for a gear's own OoP listener. `exposed` defaults to `false` (internal-only), so
copying it here would make the routes unreachable for the e2e suites that must call them.
*(Superseded by P22: in embedded hosting `exposed` does not remove a route from the gateway
router at all — `host_runtime.rs:753` — so the "internal-only" premise was wrong; see SPEC C8.)*

**The later gRPC move is expected, not contingent.** A REST contract method taking
`&PlatformSecurityContext` first is rejected at compile time —  *"generated client cannot
source the internal token… serve over gRPC or write a manual client"*
(`toolkit-contract-macros/src/rest_contract_parse.rs:337-347`, UI test
`rest_platform_secctx_rejected.rs`). P0's client is REST-generatable precisely because it
carries no platform identity; adding identity closes REST codegen and leaves gRPC or a manual
client over `attach_internal_token_http`, which today has zero call sites in the repository.
Recorded in §8.4 so the P1 decision is a consequence someone already wrote down.

### P9. Freshness validators are in P0 — the deferral rested on a misread input table

SPEC §8.5 is new, ceiling C7 shrinks, and **T22d** (added as T29 (retired), renamed by P21) is added; T23 and T30 gain criteria.

Both earlier revisions listed *"freshness validators, `ETag` / `If-None-Match`, conditional
reads"* as out of P0 because they *"need the validator inputs tenancy supplies."* That reason
does not survive DESIGN §3.3's own input table
(`cpt-cf-types-registry-tech-freshness-validator`):

| Validator input | Managed | In P0 |
|---|---|---|
| `entity.resource_version` | ✓ | yes — CAS is already P0 (T11) |
| `type_schema.resolution_fingerprint` | ✓, Type Schemas only | yes — materialized by D3 (T8) |
| subject visibility-chain version | ✓ **tenant plane only** | **not applicable** — DESIGN: *"a platform read has no subject visibility chain"*, and every P0 read is platform-plane (P8) |
| Context Tenant availability-chain version | ✓, only when availability is selected | not applicable — availability is out of P0 |
| routing generation | — external only | not applicable — federation is out of P0 |
| `external_revision` | — external only | not applicable — Externally Managed Entities are out of P0 |
| normalized projection | ✓ | yes — no `$select` in P0, and DESIGN says absent `$select` *equals an explicit default set*, so it is a constant marker |

The tenant inputs are not missing in P0; they **do not participate** in a platform-plane read.
So a P0 validator is fully computable: a versioned digest over `resource_version`,
`resolution_fingerprint` and a projection marker. DESIGN even fixes the wire form — base64url
of a version byte and a 128-bit managed digest, 23 characters.

The framework is not a blocker either. `OperationBuilder::no_content_response` takes an
arbitrary status, so `304` is declarable, and `file-storage` already returns
`StatusCode::NOT_MODIFIED` with headers by hand
(`gears/file-storage/file-storage/src/api/rest/handlers.rs:212`). There is no ETag helper in
the toolkit — this is manual work with a working precedent, not missing capability.

**What made this urgent rather than merely available.** The validator has to be in the SDK
models from **T23**. Adding it afterwards is a breaking change to a contract that ~50 call
sites across twenty-plus gears will already have moved onto (T28, T29). Deferring validators
would therefore not have been a neutral scope cut — it would have bought a second migration.

Consequences:

- Ceiling **C7** shrinks from *"the cache expires rather than revalidates"* to just the
  missing key dimensions: T30's cache now does DESIGN's batched conditional revalidation and
  fail-closed expiry, which is what `fr-client-cache` actually requires.
- A `304` replaces a resolved document of up to 1 MB on the hot read path, which is the
  cheapest thing available for `nfr-lookup-latency` after D3.
- The digest must carry the projection marker from day one; otherwise a P1 `$select` token
  produces a false `unchanged` (RFC 9110 §8.8.3). The versioned wire form is the escape hatch,
  but paying one field now is cheaper than relying on it.

**Ordering.** T22a (read routes, Phase 6 per P17) → T23 (the field in the models) → **T22d**
(computation, `ETag`, `304`, per-key batch validators) → T30 (cache revalidates against them).
**Superseded by P21:** T22d now precedes T23 — T22a → T22b → **T22d** → T23 → T30 — and T23 maps
the value T22d computes. The constraint above, that the field precedes T28/T29, is unchanged.
T30 was renumbered from T29 (retired) to keep task numbers in dependency order; P21 later renamed the
validator task from T29 (retired) to T22d, and the number was retired then. P17 retires T27 (retired)'s
out-of-order identifier by splitting it into T20a and T22a; the later tasks kept their IDs.
P22 renumbers the open tasks sequentially and so reuses both numbers.

### P10. Discovery is paged and content-free in P0; `$select` was deferred here, expansion stays out

SPEC gains decision **D12** and a rewritten §10.2; §2's row is split. T4, T22a, T23 and T31
gain criteria; no task is added.

`Discovery cursors, $select projections, OData pagination, expand_type_filter` were listed out
of P0 with the reason *"P0 keeps the current flat list"* — a restatement of the decision, not a
reason for it. Examined item by item, the four are not one decision:

**Pagination and cursors are in.** Three facts settled it. SPEC §10.1's own trait already
returns `ListEntitiesResponse`, so deferring the cursor left a page that is a page in name only — the
spec contradicted itself. DESIGN specifies the route as *"`200` with one page and a cursor"*
over *"content-free discovery"*. And the cursor's inputs degenerate exactly as the validator's
did: of the six DESIGN binds into it — query, subject visibility context, Context Tenant,
authorization scope, routing generation, per-source position — P0 keeps
**two**, query and position, because the rest are tenant-plane, PDP, or federation. Position is
free: the read route already required ordering by canonical identifier, so the cursor is a
keyset over a unique immutable column, and `toolkit-odata` (`page.rs`, `pagination.rs`) already encodes
cursors as versioned base64url that refuse an unknown version.

**The current shape is also a live problem, not only a spec gap.** `GET /entities` returns
every match in one array with each item's full `content`; with artifacts materialized (D3)
that is *entity count* × up to 1 MB, and after the pull→push cutover the count is every gear's
declarations. A `limit` alone would not have fixed it — without a cursor the bound makes the
endpoint incomplete rather than large, which is why D12 lands both together.

**At this decision, the default projection was in and arbitrary `$select` was out.** The default field set is what
makes a page content-free, so it is not optional. Caller-chosen sets need optional fields
across the models plus a normalized field-set digest inside the validator, and buy nothing
while there is a single representation to select from. P19 later moves that half into P0;
the document-free discovery default remains.

**`expand_type_filter` is genuinely blocked**, and this is the one item whose original
placement was right for the wrong reason. Its DESIGN definition *is*
`$select=gts_uuid&lifecycle_status=active&availability=available`, with the filters fixed by the method
rather than supplied by the caller. Availability (ADR-0010) needs tenancy and is out of P0, so
a P0 method under that name would report retired contracts as usable. A same-named different
meaning is worse than absence; a caller wanting the traversal pages `list_entities` itself.

**The consequence to plan for, because it lands in consumer code.** `list_instances` and
`list_type_schemas` are helpers over `list_entities`, and their call sites read payloads from
the result. The helpers select those documents on the page (P19) or hydrate through an
optional `batchGet`, complete with respect to the traversal rather than to an instant — the
same trade DESIGN accepts for expansion.

### P12. v1 stays intact; the async surface ships as v2 and is promoted at T27

T9 repointed the two existing v1 routes — `POST /entities` and `GET /entities/{gts_id}` — at the
database path instead of adding new ones. That contradicts the invariant in the risk table below
(*"The DB path has no consumer until T26; no dual-write"*), and it costs more than a red e2e
suite:

* the `POST` body shape changed and gained a required `Idempotency-Key`, so existing callers
  get `400`/`422` — not the status-code break T31 was scoped for;
* `testing/e2e/gears/oagw/helpers.py` and `testing/e2e/gears/account_management/conftest.py`
  register over REST and then resolve through `TypesRegistryClient`, so both gears write to the
  database and read from process memory. That is a functional cross-gear regression, and no e2e
  edit repairs it — only T26 does;
* `GET /entities` (list, memory) and `GET /entities/{entity_key}` (database) gave one resource two
  sources of truth.

So the surface becomes **additive**: v1 is restored verbatim from `main` and keeps serving the
in-memory store, while T9's async surface moves to `/types-registry/v2/` (T9a). The two stores
stay unreconciled — no dual-write, no fallback read — which is P6 enforced rather than merely
intended: with a fallback, an admission that never happened would read as success.

**v2 is interim, and its retirement is planned rather than assumed.** T26 deletes the in-memory
repository, so the v1 routes reading it are deleted in that same task, and **T27** promotes v2
onto the v1 paths. **P17 fixes the order at T26 → T27 → T31:** T20a authors deletion in
Phase 5 and T22a completes reads in Phase 6, so every route exists before cutover and T27
promotes all seven at once. T28 follows T26 and T29 follows T28; both can proceed alongside
route promotion. *(P22: e2e migration, now T31, depends on T29 and T30, so it no longer runs
alongside the consumer migration.)* Nothing else in this decision changes — v1 and v2 stay separate stores until T26, and the promotion is still
where the break reaches a v1 caller.

**What the T26–T29 window owes `TypesRegistryClient`.** T28 and T29
migrate consumers off the *Rust trait*, not off `/v2/`, so T27's rename costs them nothing and
their placement after it is right. The gap is one task earlier: T26 deletes the in-memory
repository the old trait's implementation reads, while ~13 `register(...)` sites and every read
site stay on that trait until T28/T29. Its `register` is synchronous, and after T26 the only store
is the asynchronous one. **The decision, rather than a note that one is needed:** the old trait keeps its shape over the
window and its `register` becomes a **submit-then-poll shim** over the one database store, deleted
by T29 with the trait. It is not a dual path in P6's sense — one store, one write path, no
fallback read — and it is what keeps the workspace building while ~15 gears migrate one at a time.
The alternative, folding T28 and T29 into T26, is rejected: T28 is ten gears and their plugins and
T29 five more, which is not work that shares a task with the cutover, and a task that cannot
compile until all of it lands is not a task. T26 carries the criterion; *"repointing it at the
database would be a compatibility shim with no consumer"* (T27) stays true of the **routes** and
was never true of the trait. *(Superseded by P23, variant C′: the SDK now lands before the
cutover, so T26 moves every consumer onto it in the same change and no shim is written. What made
folding unworkable here — the behavioural startup move of T28/T29 — stays separate; only the
mechanical API move joins the cutover.)*

**What this buys, concretely.** `make e2e-local` stays green from here to T26 with no e2e file
edited, and the red window shrinks from ~19 tasks to the T26–T31 stretch, where the wire break is
real and unavoidable. What it does *not* buy is an earlier cutover: the SDK and every consumer
stay on the in-memory store until T26, which is P6's design and not a gap. The earliest honest
cutover needs Instances (T10), revisions (T11), reference and derivation edges (T13) and
dependency-aware batching (T19) — without them the first `register` from `oagw` or
`account-management` fails, since both push batches of derived schemas and instances.

### P13. Instances move into Phase 1; `make dylint` runs per phase, not per task

Two ordering changes, taken after Checkpoint 1's report.

**T10 moves from Phase 2 into Phase 1.** Instances are not a widening of the path — they are
what the platform pushes today. P4 counts *"roughly eleven plugin gears"* already registering
their well-known Instances from their own `init()`, so Instance support is on the critical path
to T26 and is the longest pole in it. T9's surface also accepts an Instance and then fails it in
the **worker** (`StoreBuildError::UnsupportedKind` → `WorkerError::StoreBuild` → opaque `500`, a
retryable class for a final decision); building the feature closes that hole rather than adding a
refusal for it, so no separate task is needed.

The move drags in three companions, listed in T10's own entry. The one worth naming here is the
**identifier-derived closure**, because it corrects a claim Phase 1 committed to code:
`DependencyRepo::closure` walks the `dependency` table only, so nothing until T13 could reach a
candidate's base — and `admission_worker_test.rs` asserts a derived Type Schema *fails*, blaming
T13's missing edges. That is half right. `GtsId::chain_ids()` and `get_type_id()` are pure
functions of the identifier, so a derivation base — and an Instance's conforming type — need no
edge table at all. Seeding the closure worklist with the chain as well as the edges is what makes
T10 cheap, and it admits derived Type Schemas in Phase 1 as a side effect. T13 supplies the
`$ref` targets needed by the forward closure and the direct edge set needed by T14's reverse
walk. `x-gts-ref` is neither resolved nor represented by an edge.

Phase 1 therefore delivers one global entity **of each kind**, and Phase 2 becomes revisions and
concurrency. T12's *kind* rule moves with T10 — a Type Schema `…ns.thing.v1~` and an Instance
`…ns.thing.v1` derive the same `family_key` — while shape and contiguity stay in T12.

**`make dylint` moves from per-task verification to the phase checkpoint.** It builds the whole
workspace, so per task it is the most expensive check on the list and the one that gets skipped;
per phase it is cheap enough to actually run. The exposure is bounded — phases 2 through 7 are two
to four tasks each.

**The per-task standing bar had to move with it.** `todo.md` required *"`make ci` green"* of every
task, and `make ci` ends in `dylint` — so the old bar cancelled this decision on
the line above it, which is why T1–T9 each recorded `make ci` as *partial*. The bar is now
`make fmt`, `make clippy` and gear tests per task; the full `make ci` — `dylint`, `deny`,
`lychee`, `gts-docs` and four container targets — is a checkpoint gate. Bare `make ci` lines are
gone from individual tasks, and where one carried something specific (`lychee` on the two
documentation tasks) that check stayed and only `ci` went.

**And `make test-db` was never the right command for this gear.** It runs `cf-gears-toolkit-db`'s
own suite and never builds `cf-gears-types-registry` — a task could satisfy that line without
executing one line of the gear, which T2, T4, T5, T7 and T8 each noticed separately. The two
container suites now have a target of their own, `make test-types-registry-db`,
in `make ci` beside `test-users-info-pg` and `test-usage-collector-pg`. `todo.md`'s Commands
section is the single definition; the tasks say *gear tests* and point at it.

The counter-evidence is real, and is why this is a decision rather than a convenience: the first
run happened at Checkpoint 1 with 26 violations standing across T1–T9, in three families
(DE0708, DE1302, DE0301). A phase is a far shorter accumulation window than nine tasks, and
layering violations are cheap to fix in bulk because they are mechanical.

**The per-task records go with the requirement.** T1–T9 each carried a `make dylint` line
recording that it had not run; with no per-task requirement there is nothing for those lines to
record, so they are removed rather than left standing as unmet criteria. Nothing observed is lost:
the workspace-wide run at Checkpoint 1 covers every one of those tasks, and it is where the
findings are recorded. Checkpoint 0's gate is ticked from that same run — Phase 0 is one task and
the run included its changes. Phase 1's run covers T1–T9 only, so Checkpoint 1 carries an explicit
**re-run** item for T9a and T10.

### P14. `x-gts-ref` is not a dependency edge

`x-gts-ref` constrains an instance value to match a GTS identifier pattern. It does not resolve
or inline the entity that the value names and therefore creates no dependency edge.

* **Validation.** `gts-rust` enforces the keyword by matching the value string against the
  pattern — `XGtsRefValidator::validate_value_matches_gts_pattern` parses the value, parses the
  pattern, compares. It never consults the store. So the constraint is satisfiable with nothing
  registered under the pattern.
* **Artifact refresh (T14).** An `x-gts-ref` target is not inlined — DESIGN §3.1 excludes it from
  the resolution closure by name. Revising the target therefore cannot change the constraining
  schema's artifacts. Including it in a reverse walk would spend
  `limits.activation_write_set` on branches whose effective artifact cannot change.
* **Deletion safety (T20).** The platform provides no referential-integrity guarantee for the
  keyword: an `x-gts-ref` naming `topic.v1~` will **not** block deleting `topic.v1~`. A value
  naming a deleted entity stays structurally valid, the registry sees no runtime data that would
  make the refusal meaningful. Instance values are likewise not scanned for identifiers they
  contain.
* **Admission policy.** The managed–external boundary classifies entity-naming patterns directly
  from candidate content when federation is introduced. Major-0 quarantine has no such check:
  changing a named v0 entity cannot change the constraining schema's accepted payloads.

The stored edge kinds are `1 schema_ref, 2 derivation, 3 instance_of`, and
`ck_tr_dependency_kind` admits exactly those values. The numbering is append-only after the
first release.

**Renumbering them in the initial migration is safe here, and this is why.** `main` carries
`1 schema_ref, 2 gts_ref, 3 derivation, 4 instance_of` and the same initial migration, so a
deployment on `main` with a database has applied that migration and will not re-apply the
edited one. Nothing is mis-decoded regardless, because **no supported production path in
`main` can have persisted a `dependency` row**: `DependencyRepo::replace_outgoing` has no
caller there outside tests — `main`'s own T13 entry says so — and this branch is where
admission first calls it. So no row exists whose `kind` could be reinterpreted, and the only
residue on an
already-migrated database is a laxer `CHECK` (`IN (1,2,3,4)` where the edited migration writes
`IN (1,2,3)`), which admits a superset of what the code can now produce. Once a release has
persisted an edge, this argument expires and the numbering is append-only, full stop.


### P15. Locking the revision vector is the wrong tool; serializing commits is right

SPEC §8.1 step 4.2 asked for two lock levels: the version family, then *"candidate and
revision-vector entity/current rows in canonical identifier order"*. T15 implements the first and
not the second — and the second is not a shortfall to be made up later. It is the wrong mechanism,
and DESIGN §4 has been corrected rather than deviated from.

**A lock cannot do the guard's job.** It guarantees only that nothing moves *after* it is taken,
and the movement that matters happens between evaluation and the lock — the phantom dependent
appears before any lock could be held. So the vector comparison is required whether or not rows
are locked, and the lock is purely additive: what it buys is that a contended admission waits
instead of rolling back. Liveness, not correctness.

**And it is expensive in exactly the place this design pays attention to.** One round trip per
vector member, inside the commit transaction, on a set `activation_write_set` allows to reach 512
— the cost T14 restructured the reverse read into a single CTE to avoid. It is also the only
reason step 4.2's canonical ordering needs to extend past families: order matters because the
locks are many. Remove them and the requirement disappears with them. Optimism is the shape of
the rest of this design anyway — `resource_version` compare-and-swap, a transient store per unit,
validation outside any transaction — and registrations are rare against reads, which is the
regime optimistic detection is for. That the secure API has no `FOR UPDATE` and `SQLite` has no
row locking is corroboration, not the argument: the argument stands if `FOR UPDATE` arrives
tomorrow.

**What holds instead.** The candidate's own row is serialized by the compare-and-swap that writes
it. A dependency that moves is serialized by the refresh its mover owes the dependants: that
refresh writes each affected dependant's `type_schema` row, the same row this commit writes, so
the two block on one another — which orders them and nothing more, since a refresh computes its
artifacts before it writes. What makes the loser notice is that the refresh's write is a
compare-and-swap on the revision and fingerprint it read; it rolls back and recomputes. Where the
change leaves a dependant's fingerprint unmoved, nothing is written and nothing was stale. SPEC §8.1
step 4.2 records the argument, the one window it leaves, and the liveness cost by name.

**One lock survives the argument, and it orders commits.** The argument covers everything that
meets on a row. What it cannot cover is an edge committed *after* a mover's reverse scan: adding an
edge moves no `resource_version` and writes only `dependency`, so the two commits write no row in
common, both pass their own guards, and the dependant keeps an artifact inlined from a revision
that is no longer current with a fingerprint that matches it. The requirement is therefore a **serialized write
path**: every commit claims the `entity_write_order` row of `types_registry__coordination_state`
as its transaction's first
statement, one commit at a time per installation. It works because the reverse-impact scan and
the vector guard already run inside the commit transaction: either the edge is visible to the
mover's scan, or the unit writing it has not committed and its own guard catches the mover. Two
cases, no third. A row rather than an advisory lock, because advisory keys live on a session
separate from the transaction's connection and losing it would release the key while the
transaction carried on. This is nothing like a lock over the vector, which stays the optimistic
guard for the window between evaluation and the claim. Every writer of entity state claims it:
admission here, deletion at **T20**, the purge job under ADR-0013. DESIGN §3.7 states it.

**The `unchanged` outcome is not guarded, deliberately.** Step 4.3 sits ahead of every write, and
an `unchanged` candidate performs none: no revision, no version move, no refresh. The one thing it
decides — that the authored content already equals the current revision — is decided from rows
read inside its own transaction, so no part of it rests on the evaluation's view. Guarding it
would take a genuine no-op re-submission, make it revalidate because a *neighbour* moved, and
after `worker.max_revalidation_attempts` such moves turn it into a failure. So the guard runs
after that branch, and `an_unchanged_resubmission_is_not_refused_by_a_moved_dependency` is what
would catch the mistake.

**One consequence worth naming:** `limits.activation_write_set` is now asked twice, because the
vector's reverse-impact read is the same read the refresh does. An over-bound candidate is
therefore refused at evaluation, before any transaction has written, under the same
`activation_write_set_exceeded` reason — strictly earlier and cheaper, and invisible to a client.
T14's refusal stays as the backstop for a set that grew in between. Both ask the same question,
because D5 states the bound over the set the walk *returns* rather than over the rows the
fingerprint filter ends up writing — the written set is a subset, and only the walked set is a
number either read has before it writes.

### P16. Observability is a per-task obligation from T17 on, not a second T16

SPEC **§8.6 is new** and records the contract this decision enforces; success criterion **16** is
added, so P0 does not finish with an undiagnosable decision on the write path.

T16 instrumented the admission path *as it stood at the end of Phase 3*. Every decision Phase 4
and Phase 5 add — a compatibility verdict, a forced waiver, a quarantine refusal, a deletion, a
dry run — is one T16's instruments either cannot see or cannot separate from something else.
Checked in the code rather than assumed:

* `AdmissionMetrics` (`domain/ports/metrics.rs`) has five methods and **no `kind` and no
  `dry_run` parameter**. So a deletion's success and a registration's success are one series, and
  a dry run that wrote nothing would increment `candidates_total{status="succeeded"}` beside the
  commits that did. Both spans already carry `kind` and `dry_run`, so the gap is in the metrics
  only — which is why this decision is about labels and not about spans.
* Acceptance-stage refusals are enumerable because `AcceptanceError::reason()` is an exhaustive
  match, and T16's claim that *"a refusal a later task adds cannot compile until it has a
  reason"* is true **of acceptance**. Admission-stage reasons are `ItemFailure::new("literal",
  …)` at ten-odd call sites, and nothing makes a new one appear in any vocabulary. `Unknown`,
  `blocked_by_*`, the quarantine refusals and deletion's dependent check would each be countable
  only if someone remembered.
* `Unknown` is the one verdict SPEC §16.12 requires to be distinguishable, and the only one a
  deployment has reason to alert on. Counted as one `reason` among a dozen it loses exactly what
  makes it special: it is a fail-closed refusal, not a candidate decided against.

**So each task instruments what it adds, in its own commit**, and there is no follow-up
observability task to defer. The rule, stated once here and carried as criteria in T17, T18, T19
and T20:

1. **Every new terminal outcome and every new refusal is countable under a closed vocabulary**,
   with no identifier ever a label. `refusals_total{stage,reason}` carries the refusals; a new
   instrument appears only where a label on an existing one would misreport — which is the case
   for the compatibility verdict, because `compatible` is not a refusal and has nowhere else to
   go.
2. **A series that blends writes with non-writes is wrong.** `dry_run` becomes a label wherever
   a series would otherwise mix a dry run with a commit, and `kind` wherever it would
   mix a deletion with a registration. T20 does that sweep in one commit, across every instrument
   that exists by then, because it is the task that makes both distinctions real.
3. **The admission reason vocabulary has one home, and it is compile-enforced.**
   `ItemFailure::new` takes `AdmissionFailureReason`, defined in `domain::admission::reasons`.
   Each task adds its refusal variants there. Stored and API codes remain strings;
   `ItemFailure::from_payload` restores known variants and preserves unfamiliar codes as
   `Unknown(String)`. Known reasons keep their metric labels after reading from storage;
   unknown codes map to the single `other` label.
4. **The evidence bar is T16's**, because that is what makes a dashboard contract real: rendered
   names, label keys and label *values* asserted against an `InMemoryMetricExporter`; the
   emission asserted end to end through the real `accept` / `run_operation`; and a mutation check
   that stripping the emission fails the tests.

**One correction to T16's record, while it is being extended.** `todo.md`'s T16 entry and its
commit message both argue for a process-global instrument set reached like `tracing`. The code
that shipped does not do that: the instruments are behind
`domain::ports::metrics::AdmissionMetrics`, the OpenTelemetry adapter is in `infra::metrics`, the
handle is injected from `init()` and carried down the call graph as an `Arc`, and the name prefix
is configurable. The port is the better shape — `de0301_no_infra_in_domain` cannot see an
infrastructure type that hides at the crate root — and it is the shape to extend, so the record
is corrected rather than the code. Only `observability.rs`'s two span constructors are free
functions, and its module header states why.

### P17. Complete mutations and dispatch in Phase 5; reads and the SDK contract in Phase 6

T27 (retired) is split into T20a (mutations) and T22a (reads); T21 moves into Phase 5.
The later tasks keep their IDs (until P22 renumbers the open ones).

- **Phase 5: T19 → T20 → T20a → T21 → Checkpoint 5.** T20a exposes single/batch
  deletion with dry run on all mutations (body for registration/batch deletion, query for
  single deletion). T21 adds outbox submission for database-backed mutations; T26 later
  moves startup seeding onto the same path (P3).
- **Phase 6: T22a → T23 → Checkpoint 6.** *(Historical: P19/P20 insert T22b/T22c, and P21
  replaces T23 with T22d and moves T23 into Phase 7 — see P21 for the current order.)* (T22 deferred by P18.) T22a adds `:batchGet` and bounded,
  content-free discovery with cursors and `$select` refusal. REST and SDK follow SPEC
  §10.1/§10.2 (`items`, `key`, `ListEntitiesResponse`).

T20a predates T21. T21 depends on T20; scheduling it after T20a enables
REST-to-outbox tests before Checkpoint 5. T22a needs database reads, v2 routes and T20a's
mutation docs for the seven-route completeness check. T23 needs T4 reads and T21 dispatch
for explicit-document reconciliation, and follows T22a by execution order. T22 is no longer
a P0 dependency (P18). T22d needs T22a and T23 — P21 drops the T23 dependency and moves T22d
into Phase 6 ahead of it.

Checkpoint 5 proves submit → poll → terminal outcome through the router and outbox for
all mutations in both modes, without direct worker calls. Dry runs persist outcomes but
change no entity state, revisions or versions. Checkpoint 6 verifies reads and completes
all seven routes.

T20a documents mutations; T22a completes OpenAPI and quickstart reads. Both use
`routes::V2` and internal-only mutations (C8), with router tests and manual `curl`.
P12 keeps e2e files unchanged and `make e2e-local` green until T26.

Cutover remains **T26 → T27 → T31**, alongside T28 → T29. T27 promotes all seven
routes and owns both v1-breaking changelog entries: one for the write protocol, one for
pagination and document-free defaults on read routes. T31 migrates the Python suites.

### P18. Defer per-gear inventory push to P1; retain explicit-document reconciliation

> **Deferral superseded by P22 (2026-09-30).** Per-gear publication is back in P0, through
> per-crate collectors rather than T22's `owning_gear` filter. T22 stays a transfer note that now
> points at T25. `owning_gear` on the wire, attribution correction and exposure on reads remain
> P1, as below.

**Accepted scope revision (2026-09-15).** Supersedes P4's P0 scheduling and rewrites SPEC
D11. T22 moves to [#4827](https://github.com/constructorfabric/gears-rust/issues/4827)
under [P1 #4628](https://github.com/constructorfabric/gears-rust/issues/4628)
alongside platform-plane authentication and client integration. `owning_gear` is attribution
and a local inventory selector, never authentication or authorization. The reason to group
this work is to verify the complete cross-process startup path together; metadata itself
has no authN dependency.

**Task boundaries and numbering.** Keep all existing IDs so issue links and recorded evidence
remain valid. P19 adds one active P0 task after this decision; T22 remains a transfer note.
Phase 6 is now T22a → T22b → T22c → T22d (P19/P20/P21), and T23 opens Phase 7. T23 keeps the new trait/models and a helper accepting explicit desired documents:
batch-read → compare → submit changes → poll, with bounded dependency retry. It neither
collects inventory nor deletes records omitted from the desired set. T28/T29 migrate existing
registration and read callers; they add no inventory registration call merely because a gear
has GTS declarations. T26 still deletes ready mode and the in-memory repository.

**P0 bootstrap.** T26 collects all linked Type Schema and Instance inventory, including other
gears, plus operator `cfg.entities`, starts the outbox worker, submits that combined set through
it and awaits every item before publishing the client (P3's order; the earlier "inline, before
starting the outbox" wording is corrected by P22, which also narrows the set at the end of T29). Keep one bounded seed batch: the combined set must fit
`limits.batch_candidates` and all other admission limits. Fail startup explicitly if it does
not; do not truncate or silently split dependency-related candidates. Admission already orders
the candidate graph. Verify real deployment inventories, cross-crate dependencies, the
combined-set limit, and unchanged repeat startup. This costs startup work proportional to
linked declarations, not a whole-table warm-up, and preserves C1/C4's closure.

**C3 remains open.** P0 persists `owning_gear = "types-registry"` as a documented compatibility
placeholder for admissions; it does not claim to identify their declaring gear. The field
and global NOT NULL constraint stay. Automatic inventory registration from another process
is unsupported until P1. This limitation does not widen C8's internal-only mutation surface.

**P1 acceptance boundary.** Add inventory metadata/filtering (former T22); compose it with the
platform client/security context and P0's reconciliation helper; migrate declaring gears to
push their own inventory, with dependency retry and readiness tests in both process layouts.
Reduce registry bootstrap to its own/base declarations plus `cfg.entities`. Correct existing
P0 attribution through the supported revision/provenance path even when authored content is
unchanged; a content-only `UpToDate` shortcut must not retain the placeholder. Preserve
operator/bootstrap attribution for `cfg.entities` and never infer owners from GTS namespaces.
Expose `owning_gear` on reads together with the ownership view; P0
persists it for this upgrade but returns it on no read and defines no ownership group, and
`provenance` stays `gts_spec_version`, `gts_impl_version` and `compat_forced`. Only then
close C3. Metadata acceptance and verification are tracked in #4827; integration
and migration remain epic obligations in #4628 for the P1 task breakdown.

### P19. Add field projection before the SDK and validator contracts

**Scope revision (2026-09-23).** T22b follows the completed T22a and precedes T23. It
implements `$select` on exact read, `:batchGet` and discovery. An absent selection on all
three returns a document-free P0 metadata set, following DESIGN §3.3's default; selected
documents are flat and individually addressable. P0 exposes only the managed `origin`
variant, and has no tenant availability or external origin to invent; the allowlist
contains only fields the P0 read path can actually answer.
The current 100-key batch ceiling and discovery page limits remain until a separately
specified response-byte budget justifies changing them.

This supersedes P10's deferral of arbitrary `$select`, SPEC §2's corresponding out-of-scope
row, and the fixed-projection part of ceiling C7. It also supersedes T22a's *end-state*
statements that exact/batch reads always return full documents and `$select` is refused;
T22a's completed implementation record remains intact. SPEC now fixes the P0 field
allowlist, default, DTO/SDK contract, cursor binding and validator input before coding.

**Order and boundaries.** Normalize one field set for all three reads. Exact/batch share
one projected lookup; discovery keeps one bounded keyset page and binds the normalized
selection into its cursor. Document-free reads avoid fetching and parsing documents in
storage; applying `toolkit::api::select::apply_select` after loading full documents would
change only response bytes. The result envelope, `kind` and tombstone lifecycle remain
mandatory outside selection. T23's reconciliation and hydration helpers explicitly request the
documents they consume; T22d digests the normalized selection rather than a fixed marker;
T30 keys cached representations by that same selection. T22b does not add tenant fields,
federation, or `expand_type_filter`.

**Implementation slices.** First land the SPEC/field-set contract and pure tests; next
project exact and batch reads with bounded, snapshot-consistent storage tests; last project
discovery, bind its cursor and verify OpenAPI/quickstart/router behavior. Each slice leaves
the gear building and passing its focused tests. Checkpoint 6 reviews the combined contract
before T26 begins the consumer cutover.

### P20. Add chain-depth and kind filters to P0 discovery

**Scope revision (2026-09-23).** T22c follows T22b and precedes T23. It adds only the
DESIGN §3.3 `GET /entities` filters `depth` and `kind` to the P0 read surface. `depth`
is an inclusive maximum length of parsed GTS identifier segments (`GtsId::segments()`;
one segment has depth 1), so `pattern` plus `depth` can bound a derivation or version
family without treating a greedy GTS wildcard as an exact chain level. `kind` is the
existing `type_schema`/`instance` enum. Both work without `pattern` and intersect with
it when supplied; discovery stays active-only by default. P0 still omits `origin`,
`availability`, `scope`, `tenant_id`, legacy segment filters and generic `$filter`.

**Boundary and order.** `gts-id` parses identifiers and patterns; admission stores
`chain_depth` and the parsed segments, and the repository compiles the parsed pattern into
exact per-segment joins (SPEC D14). Every filter, including `kind` and `lifecycle_status`,
is SQL before `LIMIT limit + 1` and `$select`. Extend the versioned
cursor with canonical optional `depth` and `kind`, rejecting continuation under a
changed filter; an absent field is distinct from an explicit value. This requires T22b's
cursor contract first and fixes `ListEntitiesRequest` before T23 publishes the SDK. T22d's
per-entity validator does not gain filter inputs: a validator describes one selected
entity, while a discovery page has none.

**Implementation slices.** First add `kind` through the query, repository, REST and
router tests. Then add `depth`, cursor binding and mixed-filter traversal tests on all
three backends. Last, migration 000005 materializes `chain_depth` and
`entity_gts_segment` (no backfill; it refuses a non-empty `entity`), the pattern compiles
to SQL, and a differential corpus pins it to `GtsId::matches_pattern` per backend. A
page with a cursor is full; no page is empty unless nothing matches. Checkpoint 6 reviews the
combined filter and projection contract; T27 promotes it with the other v2 routes.

**Amendment (2026-09-23).** Discovery adds `lifecycle_status=active|deleted|all`
(default `active`), an SQL predicate applied before the page limit and bound by the
cursor (absent equals `active`). All three reads always return `gts_id` and `gts_uuid`
beside `kind` and `lifecycle_status`, all required in OpenAPI and part of every normalized
selection. Exact
reads and `batchGet` are unchanged; SDK expansion requests `active` explicitly.

### P21. Conditional reads close Phase 6; the SDK contract opens Phase 7

**Scope revision (2026-09-27).** T22d moves from Phase 7 into Phase 6, directly after T22c;
it was T29 (retired) and is renamed so its ID matches its position.
T23 moves from the end of Phase 6 to the head of Phase 7, before T26. Phase 6 is now
T22a → T22b → T22c → T22d; Phase 7 is T23 → T26 → T27 → T31, with T28 → T29 alongside;
T30 followed T29 and could run alongside the e2e task (P22 later makes the e2e task, now T31, depend on T30). No task is added or split. T22d is the one rename P18's keep-every-ID rule allows:
T29 (retired) had no recorded evidence yet, and the epic and phase issues were updated with it. T29 (retired) is
retired rather than reused.

**Why.** Checkpoint 6 then closes the complete P0 REST contract on `/v2/` — all seven routes,
projection, discovery filters and conditional reads — rather than a contract whose validators
arrive after the cutover. The server work left after it is the cutover (T26) and the path
promotion (T27). Neither can join Phase 6: T26 opens the e2e red window P12 confines to
T26–T31, and Checkpoint 6 requires `make e2e-local` green with no e2e file edited.

**What T22d needed from T23, and why it no longer does.** Only the SDK model field. The server
half — the per-request digest in the domain service, `ETag` / `If-None-Match` → `304` on exact
reads, per-key `if_none_match` / `etag` / `unchanged` on `batchGet` — touches no SDK type, and
T22a already accepts and length-checks each item's `if_none_match` pending T22d. So T22d leaves
`types-registry-sdk` untouched, and T23 maps the validator T22d computes instead of declaring a
field no read fills yet.

**P9's constraint holds unchanged.** It requires the validator in the SDK models before T28/T29
move ~50 call sites onto them. T23 still precedes T26, T28 and T29, so the field still lands
first — now populated, which lets T23's mock-consumer tests exercise `unchanged` end to end.

**Cost.** Phase 7 gains T23 (M) and was already the largest phase. The REST validator shape is
fixed before the SDK exists, so a disagreement with SPEC §10.1 surfaces at T23 while the routes
are still behind `/v2/` with no consumer — the exposure T20a/T22a already accepted (P17).
T27 promotes conditional reads with the other routes; they are additive and need no changelog
break entry.

### P22. Out-of-process SDK and per-gear publication

**Accepted 2026-09-30.** Production uses a standalone registry (Profile 3).
P22 supersedes P18's push deferral, P4's `owning_gear` filter and P8's stale
codegen premise. SPEC D15–D17 and §8.4 define the contract.

- `PlatformTypesRegistryApi` is a toolkit contract with platform context on every
  method, serde-free models and separate wire DTOs. Helpers stay outside contract
  IR; default bodies would denote optional methods.
- The REST client is hand-written: codegen loses `Idempotency-Key`, `Location`,
  `Retry-After`, replay status and `ETag`/`304`. It uses runtime helpers,
  `DirectoryResolvingClient` and live-route contract tests. Moving idempotency into
  the body would hide it from POST retries and permanently change v1.
- Each declaring crate exposes plain-data `gts_declarations()` through
  `declare_gts_inventory!()`. The owning gear lists crates and publisher in
  `gts(crates = […], publisher = …)`; the macro records ownership and publishes
  after wiring. Linking a crate publishes nothing; independent collectors avoid
  version splits. Toolkit defines the publisher interface without naming the SDK.
- A collector hosted only in the SDK would invert gear → SDK dependencies.
  A registrar gear adds no benefit; manual type lists risk omissions; build-time
  bundles couple registry and gear releases.
- Publication runs in the background and contributes readiness. The cache becomes
  an SDK decorator over any local/REST implementation.

**Auth proposal, superseded by P24/P25.** P22 proposed either-plane listener auth
and bearer-only gateway auth. P24 enforced platform auth on every host; P25 removed
this either-plane axis before use. SPEC D17 now requires one plane per route.

**Migration.** Dual per-crate/global collection preserves coverage until every gear
publishes; identical submissions yield `unchanged`. Only then can the global pull
end and the seed set narrow to registry types, base types and configured entities.
P23 assigns the final task sequence below.

**Original phase split.** Phase 7 added SDK/REST/toolkit foundations and a review
checkpoint before consumers moved; Phase 8 performed cutover, publication, cache
and e2e migration. Database consumers stayed on the legacy path until cutover.
Open tasks were renumbered; completed task IDs remain evidence references:

| Before P22 | After P22 |
|---|---|
| T23 SDK trait and reconciliation | T23, contract-shaped |
| — | T24 REST path for gears in other processes (new) |
| — | T25 toolkit collectors, post-wiring, readiness (new) |
| T24 cutover | T26 |
| T24a (retired) retire v1, promote v2 | T27 |
| T25 system gears | T28 |
| T26 domain gears, delete the old trait | T29, plus the end of the pull |
| T30 client cache | T30, as an SDK decorator |
| T28 e2e | T31, plus the out-of-process run |

P22 reuses retired T27/T29 numbers; historical references mark them “(retired)”.
T22 remains a transfer note. Attribution on the wire, principal recording,
authorization and a separate platform listener remained deferred here; P23 revises
publisher metadata and release ordering.

### P23. Publication ordering, read-back operations and startup

**Accepted 2026-10-01; naming/requirement revised 2026-10-02.** Review found four
gaps in P22. SPEC D18–D22, C11–C13 and the amended D11/D17 record the decisions.

1. **Release ordering (D18).** CAS cannot stop an old release or queued operation
   from restoring older content. A per-entity publisher stamp is checked under
   `entity_write_order` before preconditions and any final refusal, including
   compatibility refusal. Lower versions are superseded; equal/higher versions
   use ordinary admission; higher identical content confirms metadata only.
2. **Read-back operations (D19).** Receipts lack items, including terminal replay.
   Both adapters submit then read `get_operation`; read failures name the accepted
   operation for same-key replay.
3. **Startup (D21).** Audit every startup phase, not just `init` (T28). Bootstrap
   and plugin dependencies become supervised work; publication gates `Required`
   readiness. T34 prevents caching while a locally registered same-contract/vendor
   plugin remains invisible. oagw root-tenant resolution and account-management
   bootstrap are known consumers of the old barrier.
4. **Earlier OoP validation.** T35 introduces a two-process fixture/cold-start pilot
   before consumer migration; its registry binary cannot pull fixture inventory.
   T49 repeats it with mixed versions and the guard enabled.

**Ordering tradeoffs.** Per-entity stamps let partial admission converge; an
owner-wide watermark would let A@2 block still-pending B@2 over B@1. An older
release may still create an identifier a newer one never published (C12). Equal
versions may change configuration-built Instances without rebuilding, at the cost
of overwrites between differently configured replicas of one release (C11).
No version override: rollback/hotfix content must ship above the last published
version. Overrides risk permanent accidental supersession.

Rejected: per-gear migration journals (not atomic with registry writes), mandatory
external upgrade runners (ordinary replicas can publish safely), and reader-floor
epochs (cannot stop already-running readers). Per-crate collectors remain behind
`declare_gts_inventory!()` / `gts_declarations()`; entries must be crate-local
newtypes, leaving `linkme` a possible local implementation change.

**Readiness.** Superseded-live is ready with warning/metric, matching an older pod
that never restarted; superseded-deleted holds readiness. Older-reader support is
the N−1 release obligation (D22). `ReportOnly`, runtime overrides, optional
consumption and stricter supersession (O6) remain P1.

**Historical sequencing (variant C′), superseded by P26.** The original queue was:
- **Phase 7, T23–T31:** SDK, REST client and cache precede T29's database cutover
  and mechanical migration of every consumer. Delete the old trait without a
  shim or uncached window. Local registration stays synchronous in `init`; T28
  audits the change from staged to immediate validation before cutover. T31
  narrows the e2e red window to T29–T31.
- **Phase 8, T32–T39:** per-gear publication leaves `init` in T28's dependency order;
  supervised prerequisites gate readiness. T38 ends the pull and moves dependent
  `cfg.entities` after wiring, avoiding an empty-database bootstrap cycle.
- **Phase 9, T40–T49:** rename, metadata, required wire field and guard land together.
  C11 applies from independent publication until the guard is complete; P0 ships
  only after Checkpoint 9.

T29 is large (~30 consumer crates) but mechanical, split into gear-group commits
and gated by compilation/tests. The publisher signature comes first; the former
combined toolkit task splits into T23, T25, T32 and T33. Retired T43–T45 work moved
to T33, T36/T44 and T37/T38.

**Renumbering.** Completed evidence keeps its IDs; open tasks are sequential again:

| Before the renumbering | Now | | Before the renumbering | Now |
|---|---|---|---|---|
| T25a | T23 | | T25b | T32 |
| T23 | T24 | | T25c | T33 |
| T25d | T25 | | T33b | T34 |
| T24 | T26 | | T32 | T35 |
| T30 | T27 | | T34a | T36 |
| T33a | T28 | | T28 | T37 |
| T26 | T29 | | T29 | T38 |
| T27 | T30 | | T31a | T39 |
| T31 | T31 | | T35–T42 | T40–T47 |
| T46 | T49 | | T34b, T47 | retired on 2026-10-02 |
| | | | T43, T44, T45 | retired, no successor ID |

**2026-10-02 revision.** `owning_gear` becomes `publisher_name`: the registry has
no gear semantics, and “owner” already denotes tenant ownership. Every global
platform mutation requires request-level `publisher { name, version }`, present
in SDK models from T24 and enforced in T48. The bodyless single-key DELETE leaves
P0. No unversioned branch, activation switch, writer inventory or adoption manifest
remains; first successful publication claims pre-migration rows. Registry seeds
and configured entities use `types-registry` name/version. Columns remain nullable
for history; the API enforces the requirement.

P1 binds names through a workload-identity allow-list (several gears may share one
identity), with SPIFFE version attestation. Phase 9's former adoption/activation
T40/T50 are retired; T41–T48 become T40–T47 and new T48 requires publisher.
O5 (bootstrap publishers) remains assigned to T36; O6 is P1.

### P24. Platform-only is enforced on every host, the gateway included

**Decision.** T25 initially enforced `.platform_only()` only on the gear listener;
api-gateway treated it as bearer-required. This let tenant bearers mutate through
the gateway (C8) until T48's handler check. T25 was reworked to enforce platform
authentication on every host:

- api-gateway resolves a platform-only route to its own requirement: a presented bearer is
  validated but not required, and a gate shared with `oop_serve` (`caller_plane_middleware`)
  requires a validated internal token from the gateway's inbound authenticator. With none
  configured the route is refused. `auth_disabled` synthesizes no tenant context on it, and
  tenant scope rules do not apply to it.
- Its OpenAPI operation declares an `internalToken` apiKey scheme, and gateway discovery never
  publishes an operation that cannot be satisfied without it — the edge strips the token, so a
  proxied platform-only route could only degrade to bearer-only.
- The either-plane axis was unchanged here, and P25 then removed it: a route serves one
  plane, and `.platform_only()` became `.platform_authenticated()`.

**Consequences.** D20/C6/C8 prohibit tenant-bearer mutations on every host. T26
moves mutation routes and Profile 1 e2e callers together; `config/e2e-local.yaml` enables
gateway `internal_auth: shared_secret`, and callers send `X-ToolKit-Internal-Token`.
Proxy routing is still path-based: a platform method sharing a published read's
path is forwarded bearer-only and refused at the registry. Method-aware proxying
remains separate work.

### P25. One plane per route: a platform route set and a tenant read set

**Decision.** types-registry offers two APIs, one per plane, and each REST route serves exactly
one of them. T25's either-plane axis (`.authenticated_or_platform()`, D17 as first written) is
removed before any route used it.

- **Platform API** — `PlatformTypesRegistryApi`, `PlatformSecurityContext`, the full operation
  set. Its routes live under `/types-registry/platform/v1/...` and use
  `.platform_authenticated()` (T25's `.platform_only()`, renamed): a validated
  `X-ToolKit-Internal-Token` is required on every host, a bearer alone is `401`, the OpenAPI
  operation declares `internalToken`, and gateway discovery never publishes it. The hand-written
  REST client (T26) is its only HTTP consumer besides operators and e2e.
- **Tenant API** — `TypesRegistryApi`, `SecurityContext`, reads only. Other gears call it while
  serving a tenant's HTTP request; out of process its client forwards the caller's bearer, which
  every hop re-validates (ADR-0008). Its routes are `.authenticated()` under
  `/types-registry/v1/...` and can be exposed through the gateway like any tenant route. P0
  authenticates only; authorization arrives with the PDP (C6).

**Why.** ADR-0008 assigns each request one plane; contract codegen already selects
it from the context type. Either-plane routes required `ValidatedCaller`, gateway
special handling and ambiguous handlers. Separate APIs add read routes and a trait,
but give each handler one plane, support tenant reads through the token-stripping
Profile 3 edge, and allow a future separate platform listener (C8) without path changes.

**T25 changes.** `.platform_authenticated()` replaces `.platform_only()`;
`OperationSpec::auth_plane: AuthPlane { Tenant, Platform }` replaces `platform_plane`.
`RouteAuth::Platform`, `platform_route_middleware` and direct context extensions
replace either-plane variants, their gate and `ValidatedCaller`.
`#[toolkit::rest_contract]` registers platform-context methods as platform-authenticated
and rejects `#[anonymous]` on them. The only current method, authz-resolver's
`evaluate`, moves from anonymous to platform authentication.

**Paths through the cutover.** The legacy v1 routes hold `GET /types-registry/v1/entities` and
`GET /types-registry/v1/entities/{gts_id}` until T30, so the tenant reads cannot take `/v1/`
before then:

| Step | Platform routes | Tenant read routes | Legacy v1 |
|---|---|---|---|
| T26 | all seven move from `/v2/` to `/types-registry/platform/v1/` | added on `/types-registry/v2/` | unchanged |
| T30 | unchanged; the REST client already targets them | move from `/v2/` to `/v1/` | deleted |

T26's REST client targets `/types-registry/platform/v1/` from the start, so T30 no longer moves
it. The tenant reads are the three entity reads — exact, `batchGet` and discovery;
`get_operation` stays platform-only, because a tenant cannot submit.

**Consequences.**
- D17 now reads "one plane per route"; D20's mutations exist only on platform routes; C8's
  missing piece is the separate listener, no longer a route marker.
- e2e: mutations and operation polling move to the platform paths with `platform_headers`;
  reads stay on a bearer, on `/v2/` until T30 (T26, T31).
- `oop_serve` installs the tenant plane only when a bearer authenticator is configured, so a
  tenant route on a registry without one must still answer a canonical `401` — T26 adds that
  gate rather than relying on the handler's extractor.

### P26. Both clients early, Account Management as the first real gear, a consolidated queue

**Historical decision, amended by P28–P31:** seeding/local publication is T24b
and full local AM is T24c. P30 consolidates the later task owners; P31 removes
the separate pilot/package/early process gate described below. Those pilot
paragraphs are history, not implementation instructions. The current queue and
real AM process proof are defined by P31, the live graph and `todo.md`.

**Accepted 2026-10-07; consolidated the same day.** The 39-task draft (T26–T64,
Phases 7A/7B/8/9, five internal checkpoints) was too granular. P26 groups it into
**17 feature tasks, T26–T42, across three phases**, preserving every decision and
acceptance criterion. As in Phases 1–6, each task delivers one verifiable outcome;
technical steps are commits. P26 replaces P23's queue while retaining its correctness,
authentication, cache, cutover and publisher-ordering guarantees.

**Required P0 outcome.** Every gear uses the persistent registry; types-registry runs out of
process; both the platform and the tenant API have usable local and REST clients early;
Account Management is the first real gear proved on the new path — publication, root
bootstrap and tenant create/read through the new client — with its IdP, Resource Group and
authentication prerequisites; mixed-version rollout support stays in P0 and comes last.

**Decision — the queue.**

- **Phase 7 (T26–T32).** T26 platform API over REST; T27 tenant API and resolving clients
  (closing the REST contract); T28 the platform cache; T29 the generic post-wiring lifecycle;
  T30 the isolated pilot and cold-start handoff → **Checkpoint 7A**. Then T31 the startup audit
  and atomic cutover, T32 one REST version and migrated e2e → **Checkpoint 7**.
- **Phase 8 (T33–T38).** T33 collectors and the `gts(…)` attribute; T34 publication ownership,
  dependent configured entities after wiring and late-safe plugin selection; T35 Account
  Management and its IdP publish and bootstrap after wiring; T36 AM's host prerequisites and
  the real-gear proof, local and remote → **Checkpoint 8A**. Then T37 every remaining gear and
  the end of the pull, T38 the out-of-process run, HA and chart → **Checkpoint 8**.
- **Phase 9 (T39–T42).** T39 publisher state, T40 commit-time ordering, T41 every writer sends
  and `publisher` is required, T42 the mixed-version proof → **Checkpoint 9**.

**Historical order, amended by P29.** Its host prerequisites (resource-group,
authz-resolver, tenant-resolver and IdP) also read the registry. Moving AM first
would require a legacy shim or leave AM writing the database while peers read the
in-memory catalogue. T31 therefore switches every gear atomically; T32 verifies
AM's new client through e2e. AM then leaves the startup barrier in T35 and runs
with a remote registry in T36, before the remaining fleet (T37). The isolated
T30 pilot provides the earlier client handoff.

**Pilot isolation.** Its registry uses a separate database and real outbox admission
for control-plane/base types, without linking or pulling consumer declarations.
Local and REST clients share the database service; remote consumers have no fallback.
The embedded host retains the legacy catalogue until T31. The pilot reuses production
code without dual writes, shims, admission forks or production feature switches.

**Pilot build and CI.** The `publish = false` package `cf-gears-types-registry-pilot`
(`testing/fixtures/types-registry-pilot/`) owns `pilot_registry`, `pilot_consumer` and
the test, gated by `required-features = ["pilot-fixtures"]`. `CARGO_BIN_EXE_*` provides
cargo/nextest paths. Bin imports use optional normal dependencies; dev-dependencies
are unavailable to bins. Only the harness depends on gear crates (AM from T36);
no gear depends on the harness, and the registry never depends on AM.
`make test-types-registry-pilot` is included in `make ci` and the CI workflow so
checkpoint gates run the pilot.

**Topology and authentication.** The pilot runs registry and consumer application
processes plus the master host's `DirectoryService`. Until T36 it uses an independent
signed-token development authenticator. T27 records the production linked/remote
topology in SPEC §8.4. T36 uses `AuthNResolverBearerAuthenticator` with the real
authn-resolver and its post-wiring plugin for both AM and the remote registry's
tenant plane. The pilot keeps authn-resolver outside the registry process; T38
verifies a production linked topology after T37 ends the pull. Development auth
never satisfies Checkpoint 8A.

**Child tenant types.** AM owns its configured root type; deployment-specific child
types remain registry `cfg.entities`. T34 adds post-wiring publication for configured
entities whose dependencies are outside the inline seed set, as in the pilot and
all registries after T37. The registry publishes them with its own context and
retries until dependencies such as AM's base type are admitted. Only consumers
wait for these items; registry readiness does not. T37 reuses this path.

**Root binding and bootstrap failures.** AM's stored root `tenant_type_uuid` must
match its configured type; this check stays lifecycle-fatal in `init`, regardless
of `bootstrap.strict`. Post-wiring root-type refusal instead holds readiness,
reports identifier/reason and prevents bootstrap without exiting (D21). A delayed
registry holds readiness and is not a saga failure. Business, database and IdP
saga failures retain `handle_bootstrap_failure`: fatal when strict, logged/skipped
otherwise. RG's sealed, init-only `ResourceGroupTypeBootstrap` remains in AM's
`init`, using RG's own database and exposing no REST surface.

**e2e windows.** T26 and T27 move routes and their e2e callers in the same commit, so the
async-surface suite stays green through Checkpoint 7A — the draft's T27–T32 route/auth red
window is gone. The legacy v1 window is T31–T32. No pilot work enters it.

**Boundaries.** Tenant REST uses the interim `/v2/` path until T32; callers see the semantic
API. The SDK's `PublisherContext` is required from T24, but adapters forward it only from T39
and the guard is required in T41. The early handoff promises no mixed-version mutation safety;
final P0 deployment requires Checkpoint 9.

**Unscheduled work.** A generic multi-gear harness, directory co-hosting in the pilot
consumer (requires verified lifecycle ordering), method-aware gateway proxying (P24), contract codegen
for these REST clients, and a tenant client cache are not approved here. A separate
platform listener, authorization and workload-bound publisher identity remain P1.

**Renumbering.** Only unfinished tasks change IDs; completed evidence is preserved. A
reference to the pre-P26 REST task (T26) means T26+T27; to the pre-P26 pilot (T35) means T30.

| Now | Work | P26 draft | Pre-P26 (P23/P25) |
|---|---|---|---|
| T26 | Platform API over REST, platform routes and their e2e callers | T26, T27, T28, platform half of T32 | platform half of T26 |
| T27 | Tenant API, resolving clients, REST contract closed | T29, T30, T31, tenant half of T32, T33 | tenant half of T26 |
| T28 | SDK client cache | T34 | T27 |
| T29 | Toolkit post-wiring lifecycle: hook, supervision, `Required` readiness | T35, T36, T37 | T33 (hook, readiness, supervision) |
| T30 | Isolated pilot harness and cold-start handoff | T38, T39 | T35 |
| T31 | Startup audit and atomic cutover | T40, T41 | T28, T29 |
| T32 | One REST version and e2e on the `202` contract | T42, T43 | T30, T31 |
| T33 | Per-crate collectors and the `gts(…)` attribute | T44, T45 | T32, T33 (attribute) |
| T34 | Ownership, dependent `cfg.entities` after wiring, plugin selection | T46, T47 (+ dependent-entity path from T53) | T34, T36 (+ from T38) |
| T35 | AM and static IdP publish and bootstrap after wiring | T48, T50, AM half of T49 | AM part of T37 |
| T36 | AM host prerequisites and real-gear proof, local and remote | prerequisite half of T49, T51 | prerequisite part of T37 |
| T37 | Every remaining gear after wiring; end of the pull | T52, T53 | T37, T38 |
| T38 | Out-of-process e2e, HA, chart | T54 | T39 |
| T39 | Publisher state: stamp, migration, durable context, wire field | T55, T56, T57, T60 | T40, T41, T42, T45 |
| T40 | Commit-time ordering for registration and deletion | T58, T59, T61 | T43, T44, T46 |
| T41 | Every writer sends its publisher; `publisher` required | T62, T63 | T47, T48 |
| T42 | Mixed-version rollout proof | T64 | T49 |

The draft's internal Checkpoints 7A.1–7A.5 are retired; Checkpoints 7A, 7, 8A, 8 and 9 remain,
each with the full gate.

**Planning horizon (amended by P27).** Keep later acceptance criteria complete.
Queue changes require a demonstrated correctness, security or compatibility blocker,
with its invariant and smallest affected task recorded, or review regrouping that
preserves criteria and checkpoint gates. Additive features require a numbered plan
decision and unchanged gates; convenience refactors do not expand them.

### P27. Every trait and local client in one pull request; T25 lands on its own

**Accepted 2026-10-08.** P27 regroups work for review without dropping P26 criteria
or moving checkpoint gates. It amends the planning horizon to allow such regrouping
and additive features named in a plan decision. The sole addition is
`TypesRegistryApiExt` in T24a; Checkpoint 7A and later gates stay unchanged.

- **T25 is its own toolkit pull request.** The platform-authenticated axis touches only
  toolkit and api-gateway, and nothing before T26's route move uses it, so it is reviewed by
  its owners on branch `toolkit-platform-route-auth`. Its criteria stay unchecked in the task
  list until it merges into `main`. T26's DTO and client commits need only T24; its route move
  and TCP contract test need T25 in `main`.
- **T24a — the tenant contract, its extension helpers and its local client** move out of T27
  (its former commit 1), so T24 + T24a deliver every P0 trait and local client in one pull
  request. T24a adds `TypesRegistryApiExt`, the platform's read conveniences over the tenant
  API, sharing one implementation of the helper logic; it is additive and changes no platform
  shape.
- **T27a — resolving clients and the contract's closure** split out of T27: both
  `DirectoryResolvingClient` wrappers, `rest-server` and `#[provides]`, the full auth matrix on
  both hosts, and the QUICKSTART tenant flows. T27 keeps the tenant REST client, the tenant
  routes and the standalone authenticator with its SPEC §8.4 topology decision.
- **The REST contract becomes a normative reference.** Its items lose their checkboxes and
  name their owning task (T24a, T26, T27 or T27a), so no item is tracked twice.

T24a reuses a number retired before P22 (*"T24a retire v1, promote v2"*, now T32); that
historical reference is marked "(retired)". The sequence becomes
T24a → T25 → T26 → T27 → T27a → T28 → … ; Checkpoint 7A and every later task are unchanged
except that T28 and T30 depend on T27a instead of T27.

### P28. Make the local SDK usable by a new gear at T24b

**Later ordering/venue is amended by P30/P31; the T24b scope is unchanged.
There is no separate T30 pilot in the current plan.**

**Requested 2026-10-09.** T24/T24a implement the contracts and local adapter, but
`TypesRegistryGear::init()` publishes only the legacy client and seeds inventory
and `cfg.entities` only into memory. A new gear therefore cannot resolve the new
API, and its database-backed admission cannot see the base/configured types.
T24b closes that complete local path before REST, cache, post-wiring publication
or the fleet cutover. This amends P26/P27's queue and their ban on coexistence;
it does not move final P0 deployment or any existing checkpoint gate earlier.

**Dependency order and task ownership (amended by P29).** T24 + T24a → T24b → T24c → T25 → T26 → … .
T24b takes the database seeding and local ClientHub publication out of T31.
T31 keeps the fleet-wide startup audit, migration of all remaining consumers,
deletion of the legacy trait/store/routes and regression verification of T24b.
The existing `docs/p0/plan.md` and `todo.md` remain the only execution artifacts;
no repository-root `tasks/` plan is created.

**Deliverable.** In a database-bound embedded host, registry `init()` starts its
outbox, admits all linked Type Schema/Instance inventory (including `toolkit-gts`
bases and registry control-plane declarations) plus `cfg.entities`, and awaits
success before registering both `dyn PlatformTypesRegistryApi` and
`dyn TypesRegistryApi` over the same `RegistryService`. A new local gear can then
resolve the platform API and reconcile its own explicit documents in `init()`.
The database binding already present in quickstart/e2e is a host prerequisite,
not a new persistence mechanism. A host without a database exposes neither new
API; legacy-only no-db hosts keep their existing behaviour.

**Four sequential implementation slices, each sized S/M.**

1. **Seed audit and deployment budgets.** Measure the actual linked seed population
   for quickstart/example/e2e feature sets and set explicit deployment limits that
   fit it; the current default of 100 is not evidence that those builds fit. Audit
   actual derivation/`$ref`/Instance-of dependencies, resolving any supplied only by
   later legacy registration before the seed barrier is introduced. `x-gts-ref`
   matches identifier syntax/patterns and does not require a stored target (P14):
   e2e's customer `allowed_parent_types` does not require the later AM root type.
   Prove that distinction with the actual configured schema; do not duplicate AM
   root publication or bring T34/T35 forward on that assumption. Expected files:
   `config/quickstart.yaml`, `config/e2e-local.yaml` and the seed audit in `todo.md`.
2. **Database admission.** Implement deterministic, idempotent seeding through
   `RegistryService` and the real outbox, not the SDK helper that automatically
   batches/bisects submissions. Keep the combined-set limit and one graph-ordered submission
   from P2/P3: no truncation, partial per-gear filter or silent batch splitting.
   Repeated equal content creates no new revision; changed configured content
   uses read/CAS reconciliation and remains subject to ordinary admission rules.
   One monotonic deadline starts before seed reads and uses
   `cfg.worker.operation_timeout` as the whole seed budget (default five minutes,
   not multiplied by delivery attempts); cancellation uses the runtime token.
   Seed refusal, unresolved dependency, deadline or cancellation fails startup
   before either new API is exposed. Same-version concurrent seeders re-read
   create/CAS conflicts and converge under the same deadline; mixed-version
   ordering remains Phase 9. T31 still audits the entire fleet. Expected files:
   `gear.rs`, `domain/{mod,seeding}.rs`, `tests/seeding_test.rs` and
   `tests/seeding_backends_test.rs` in the registry crate.
3. **ClientHub-to-consumer path.** Publish the two local trait objects after the
   seed barrier. A fixture gear declares `deps = [types_registry]`, resolves the
   platform API directly and uses `reconcile_entities_and_await` with its own
   `PublisherContext`, `PlatformSecurityContext::outbound_marker()`, an explicit
   deadline and `ctx.cancellation_token()`. Only `UpToDate` or all-`Admitted`
   outcomes permit boot; `Rejected` and `Pending` name their identifier/reason.
   Prove this through the real host lifecycle/ClientHub, not a manually constructed
   client: use an explicit `RegistryBuilder` with only the production registry and
   fixture consumer (core/DB/system/stateful capabilities; omit REST for this local
   fixture), `HostRuntime::new` with `DbOptions::Manager`, and public
   `run_gear_phases()`. Observe consumer-init assertions before later phases and
   cancel/join the host through test synchronization. Do not use process-global
   gear discovery or add a test-only lifecycle API. Expected files: `gear.rs` and
   `tests/local_client_boot_test.rs` (reuse existing test support).
4. **Integration handoff.** Make the new APIs the primary SDK README surface:
   explain platform/tenant contracts and show complete hub resolution, publication,
   outcome handling and read examples. Move every old-client/model/example/error
   description into a separate `Legacy API` section, retained until T31. Document
   the no-db failure and transition boundaries; run the real quickstart/e2e composition and
   the focused restart/update/negative scenarios. Compile the main README examples
   as `no_run` doctests included by the SDK, replacing its duplicate crate-level
   introduction; keep tenant C2/C6 limitations explicit. Any missing example-only
   imports are workspace dev-dependencies in the SDK manifest.
   E2e discovery assertions use their own namespaces and registration tests avoid
   assuming the now-seeded database is empty. Expected files:
   `types-registry-sdk/README.md`, its `src/lib.rs`, `QUICKSTART.md` and, if needed,
   the SDK `Cargo.toml`; namespace
   corrections to e2e tests are separate small commits inside this handoff.

Review after slices 1–2 and after slice 3 before the handoff; T24b is complete only after all four
slices pass their criteria and the standing per-task bar. The detailed checklist
and commands live in T24b in `todo.md`.

**Temporary coexistence, removed at T31.** Both API generations may be registered
in the embedded host from T24b. Existing consumers and legacy v1 routes keep the
old in-memory service; the new APIs and v2/database routes use the database. The
initial inventory/configured documents populate both paths while legacy remains;
this is not a continuous dual-write, read fallback, shim or synchronization layer.
A new gear uses the new API for all its registry reads/writes. Its mutable IDs
must not also have legacy writers, and dependencies produced only by later legacy
calls are unavailable to it. Static inventory IDs can exist in both seed sets;
do not promise that subsequent writes in one catalogue appear in the other.
T31 removes this exception by migrating every remaining consumer and deleting
the legacy path. T30's isolated pilot still has only the new catalogue.
P29 extends the early handoff to the complete AM/IdP/TR group at T24c; existing
consumers outside that group remain legacy until T31.

**Boundaries and risks.** Immediate seed validation may expose dependencies that
legacy staging postponed until `post_init`; the first slice must resolve and test
them before publishing clients, rather than weakening validation or waiting on a
later registrant. `deps` orders `init`, not `post_init`/`start` or proxy wiring.
The already-running worker prevents an admission wait from depending on `start`.
This handoff is local and initially uncached (T28 supplies the cache); it does not
add REST/resolving clients, `#[provides]`/`#[consumes]` wiring, `post_wiring`,
`publish_gts`, automatic per-crate publication or publisher ordering. The publisher
field is required by SDK models but is forwarded/enforced only in T39–T41.
T35–T37 remove the transitional local calls from `init()`; remote callers always
wait until after wiring. No mixed-version mutation guarantee is introduced.

### P29. Fully migrate Account Management locally in T24c

**Amended by P30/P31:** former T36 belongs to T35, which now also owns the full
real AM process proof instead of the removed T30 pilot. The T24c scope is unchanged.

**Requested 2026-10-09.** T24c follows T24b and moves Account Management entirely
to `PlatformTypesRegistryApi`, with one resolved platform client shared by its
services. Do not leave a metadata-only migration, a legacy client hidden behind
an adapter, fallback or mirrored writes. The proof is the existing production AM
REST workflow, not a synthetic business consumer or an otherwise-unused licensing
SDK driver. No licensing migration is added to this queue.

**One coherent local group.** Migrate seven gears in one atomic merge:

- `account-management`: root registration, bootstrap reads, tenant/user/service-account
  validation and UUID hydration, metadata, IdP discovery, and its optional TR
  plugin's instance registration and schema reads;
- `static-idp-plugin` and `keycloak-idp-plugin`: both write the instances AM's
  migrated discovery reads;
- `tenant-resolver`, `static-tr-plugin`, `single-tenant-tr-plugin`, `rg-tr-plugin`:
  the core must discover AM's now-database-only TR instance, and all supported
  standalone writers must follow that reader.

The public AM, IdP and tenant-resolver business SDKs are unchanged. AuthZ, RG's
own implementation and unrelated instance families can remain on legacy until
T31. Audit external readers/writers of AM-owned IDs: a real dependency crossing
that boundary must join the migration or be resolved before merge, never ignored
or bridged by dual writes. AM's user-group type bootstrap uses RG's own DB and
`metadata_schema = None`; it does not justify a full RG migration. Preserve its
sealed init-only contract. The pre-existing RG startup issue #4568 stays outside
T24c; retain the existing reduced test profile and test RG plugin publication
separately.

**Execution.** Six ordered stages below each land as small S/M implementation
commits (around five files each); split a stage's fixture/mechanical changes
further when necessary. They form one delivery/merge boundary, not six independently
deployable mixed-catalogue states:

1. **Audit and DB bindings.** Record all affected compositions, their effective
   configuration/feature sets and cross-catalogue prerequisites. Bind a registry
   database wherever the new TR/IdP writers run, including no-db legacy profiles
   that now require persistence. Distinguish overlays from complete configurations;
   do not modify unrelated standalone/mock hosts by assumption.
   Audit registration regions for configurable GTS IDs (especially a custom
   root suffix): permit only explicitly intended vendors/regions and diagnose
   closed ones. `PluginV1.vendor` selects a backend; it is not the last-segment
   vendor that registry admission checks, and fixed `cf` instance IDs need no
   allowance merely because their selection vendor is `constructorfabric`.
2. **Tenant-resolver family.** Move discovery and all three standalone TR writers
   together. Use the SDK's complete list helper, select content, retain vendor /
   priority semantics and prove the winning instance can be on a later page.
3. **AM reads/models.** Move every schema reader and materialization consumer,
   including metadata and payload validation, UUID batch lookups and optional TR
   queries. Replace `GtsTypeSchema`/`GtsInstance` with new snapshots; explicitly
   select content/resolved schema/effective traits. Preserve absent-vs-failed
   semantics; never discard a failed batch or recompute artifacts from parents.
4. **AM root and bootstrap.** Reconcile the configured root type and optional
   TR instance through the same new client. Await all required outcomes before
   dependent initialization; bootstrap reads the admitted database state. Preserve
   the existing stored-root UUID check **before any root reconciliation/write**:
   binding mismatch fails with no extra registry entity/revision, under either
   strict policy. Bootstrap reads and ordinary failure handling remain intact.
5. **IdP family.** Migrate lazy IdP discovery and both writers together, using
   each writer's own publisher context and scoped-client publication barrier.
   Keycloak backend behavior and external integration remain unchanged.
6. **Real application proof.** Move AM e2e schema registration to the current
   database REST contract with operation polling, narrow vendor policy for test
   schemas and no weakened assertions. Run tenant CRUD/type enforcement, metadata
   validation/inheritance/UUID hydration and static IdP provisioning, then real
   host restart/configuration-drift tests and the affected host boots.

Review after the config/TR stages and after the AM/IdP stages before the final
application gate. T24c is complete only when the final AM crate and all six peers
have no legacy SDK/client/model use in production or their migrated tests, and
the existing application requests succeed against database-backed types.

**Lifecycle boundary.** This is an embedded/local cutover, without new REST
clients or a post-wiring publisher. Keep `deps = [types_registry]`; the new worker
and seed barrier already exist in T24b. Registration uses bounded, cancellable
reconciliation: only `UpToDate` or all-`Admitted` permits continuation. Accepted
operations and `Pending` are not startup success. Root-identifier drift against
stored AM state remains fatal, while permitted content drift uses CAS and ordinary
admission/compatibility rules. The publisher guard is still Phase 9. T28 adds the
shared SDK cache; T24c introduces no private schema cache.

**Later ownership.** T31 records/rechecks these seven gears and migrates only
the remaining legacy consumers before removing legacy globally. T35 moves AM and
both IdP plugins after wiring and adds publication/bootstrap readiness, reusing
T24c's adapters/tests. T36 moves host prerequisites (including the already-new
TR family) after wiring and proves real AM with a remote registry and production
authentication. T37 skips those completed lifecycle migrations. T30 remains a
bounded SDK transport/lifecycle harness; real application evidence now starts
at T24c and extends to remote AM at T36. Its fixtures do not substitute for the
AM REST gate or claim remote AM is already supported. Existing checkpoint gates
and the final Checkpoint 9 deployment boundary remain unchanged.

### P30. Consolidate the remaining queue after the early local migrations

**Amended by P31:** T30's separate pilot/application and its early process gate
are removed below. The five task merges, full local AM scope and other owners
remain; unique process proofs move to real AM in T35.

**Requested 2026-10-09; reviewed against all remaining tasks.** P28/P29 moved
real implementation out of later work: T24b owns seeding/local publication, and
T24c owns the complete local AM/IdP/TR SDK migration. Repeating that work in T31
or T35 is removed. Lifecycle/remote adoption is still new work, as are caching,
per-crate collectors, dependent configured publication, selector races, HA and
the publisher guard. Consolidate the remaining delivery boundaries from **21
to 16 open tasks**, preserving all original acceptance and verification coverage.
This is not a claim that merging IDs eliminates the underlying implementation.

| Former open task | Current owner | Delivery boundary |
|---|---|---|
| T27a | T27 | Tenant REST, both resolving clients/provides and the complete real-host auth matrix |
| T32 | T31 | Remaining SDK/legacy cutover, tenant v2→v1 promotion and all e2e callers in one merge |
| T36 | T35 | AM/IdP lifecycle, selected host-prerequisite closure, production authentication and remote application proof |
| T40 | T39 | Publisher state/migration/adapters plus serialized check/claim/confirmation/deletion |
| T42 | T41 | Writer/SDK activation, required publisher and the complete mixed-version rollout proof |

Retired IDs remain as short transfer notes in `todo.md`; they are not checked off,
reused or executed independently. Historical evidence keeps its original IDs.
The live graph/index/queue and SPEC references use current owners. Merged tasks
retain their implementation stages as small commits/reviews; a large feature
does not become a single giant implementation commit.

**Remove the red window.** T31 now merges deletion of the old SDK/store/routes,
promotion of all three tenant reads and migration of remaining v1 callers together.
There is no accepted T31→T32 interval with broken e2e. Preserve the complete
former T32 criteria: path-only promotion and the tenant client constant, operation
IDs/conditional semantics, symbol-specific legacy deletion, one-release changelog
entries, shared polling, filters/projections and the existing REST regression tests.
T24c's schema helpers were already moved earlier and are regression-checked here.

**One AM lifecycle/remote feature.** T35 reuses T24c's adapters and real REST
scenarios, adds publication/bootstrap readiness, closes the selected host's
startup dependency graph and proves the remote registry under production auth.
The graph includes callers of a delayed publication, not only AM's named direct
dependencies: e.g. oagw's eager `post_init` root lookup cannot be left waiting on
an instance moved asynchronously out of static-tr's `init`. Move each dependent
startup step before or with its publisher to supervised/lazy execution. Never
defer that break to T37 or hide it with an early proxy/local fallback. Checkpoint
8A follows the completed feature; it retains every former T35/T36 gate.

**Historical P30 remote-venue proposal — superseded by P31.** T30 was retained for real
process/directory wiring, both cold-start orders, no local fallback/inventory pull,
late cross-process dependencies, rejected→not-ready, recovery and shutdown.
Full auth matrices belong to T26/T27 and cache/parity/validator contracts to
T28 and the existing SDK/TCP tests; T30 reuses those helpers and checks actual
process composition, rather than creating second implementations of each suite.
Reuse the existing OoP process/runtime tooling, with two minimal isolated host
entry points in the feature-gated test package. The existing Payment example
binaries cannot be used unchanged: their `oop_module` features link the registry
implementation and would mask an unintended local client or inventory pull.
Two distinct binaries are also needed for the T33 collector-isolation proof.
No additional business application or general-purpose harness framework is built.
T33, T35 and T41 extend this same venue; the existing make/CI gate is retained.

**Avoid an unrelated AuthN RPC migration.** The current `AuthNResolverClient` is
an async SDK trait without a REST contract/adapter. T27 therefore records the
supported linked production topology: each host authenticates through its local
`AuthNResolverBearerAuthenticator` and linked authn-resolver/plugin. T35 does not
invent a remote AuthN SDK solely to keep every gear out of the registry process.
Its isolation assertion excludes AM/RG/TR declarations; linked authentication
declarations are expected. After wiring, the registry host's aggregate readiness
waits for its authentication declarations/plugin to be admitted. Platform
internal-token admission remains available independently, so this does not form
a publication cycle. T38 rechecks this topology after global pull is removed.
This amends P26's requirement to keep authn-resolver outside the registry host;
new AuthN transports remain outside this Types Registry scope.

**No duplicated later rollout suites.** T34 reuses the owner assignments already
made in T24b/T24c and only closes the remaining assignments/implementation;
dependent configured entities and the incomplete-selection guard are unchanged.
T38 owns the end-of-pull production topology, two-replica same-key failover and
Profile 3 chart/PDB. It reruns the established remote/auth/readiness scenarios
against that topology through shared tests, rather than writing another suite.
T39 implements the whole server guard; T41 updates every writer/SDK, closes the
unversioned bypass and runs all former T42 mixed-version scenarios. No intermediate
Phase 9 commit is deployable. Final P0 deployment still requires Checkpoint 9.
For operations accepted before durable publisher metadata existed, T41 fails only
unfinished items with the named terminal reason `publisher_required`, before any
new entity write. Already-committed outcomes and the operation identity remain.
An exact replay of the original accepted body is a read of that stored operation,
not permission for an unversioned write; adding publisher to the same-key body
changes its fingerprint and conflicts. A fresh operation requires a fresh key
and publisher. No publisher/version is inferred for historical pending items.

**Checkpoints and dependency graph.** Keep the full five gates, changing only
their owning task: 7A after T30, 7 after T31, 8A after T35, 8 after T38 and 9 after
T41. Each retains its full CI/backend/application scope. T29 depends technically
on T23/T24, not the T28 cache; T28 stays its queue predecessor and both feed T30.
The executable queue is T24b → T24c → T25 → T26 → T27 → T28 → T29 → T30 → T31 →
T33 → T34 → T35 → T37 → T38 → T39 → T41. The detailed task list is authoritative.

### P31. Remove the separate pilot; prove remote operation on real AM

**Requested 2026-10-09.** A dedicated T30 pilot is no longer the selected
application proof. Remove the task, its synthetic consumer/provider application,
`cf-gears-types-registry-pilot` package, `pilot-fixtures` build feature and
`make test-types-registry-pilot` gate from the executable plan. None has been
implemented by this planning change. The queue has **15 open tasks**; the T30
ID remains a transfer note, not a completed task.

**Proof owners.** T26/T27 own the SDK REST/TCP/auth/resolving contracts and the
production standalone registry entry with linked AuthN. T28 owns cache/parity;
T29 owns generic hook/supervision/readiness. T33 independently proves collectors
with compiler/release-LTO/isolation fixtures; it depends on none of the removed
process application and does not run a remote business workflow. These are
focused tests, not a replacement pilot application.

T35 owns the complete cross-process application proof on production Account
Management: both cold-start orders, directory wiring with no local fallback,
AM startup while registry is absent, admission/rejection and readiness, late
AM-base→registry-configured-child dependency, restart/persistence and graceful
shutdown. Drive existing tenant, metadata and IdP REST scenarios against real
AM/registry/authz/databases. A publication refusal holds real AM not ready with
its diagnostic; there is no invented business consumer to stand in for that.
T38 reruns those flows on the end-of-pull/HA deployment; T41 extends the same
venue for the complete publisher/mixed-version proof.

**Real host entries, not a renamed pilot.** T27 supplies the deployable registry
standalone entry through existing OoP/bootstrap tooling. T35 supplies the
deployable AM host composition with its audited prerequisite closure, directory
and linked production AuthN. The AM host must not link the registry implementation:
remove both the dependency macro edge and the implementation-crate dependency
from every member of its closure as startup calls leave `init`, not just from
AM itself. Verify with `cargo tree` and no local provider in its ClientHub.
The current all-in-one example/Payment binaries cannot be used unchanged.
The registry host may link its authn declarations but not AM/RG/TR declarations
that would conceal the consumer's publication. Do not add a fake application,
a production role switch for tests or a new AuthN RPC transport.

Place these entries in separate production composition crates
(`apps/cf-types-registry-host` and `apps/cf-account-management-host`), not in the
registry gear library: linking authn-resolver from that library would introduce
a cycle because authn currently depends on the registry gear. T31 regression-checks
the registry host after its linked authn consumers move off legacy.

**Explicit gate change.** Checkpoint 7A follows T29 and promises real local AM
plus SDK/TCP/resolving/auth/cache contracts and generic lifecycle tests. It no
longer promises a full process-level cold-start/recovery proof. That guarantee
is made only at Checkpoint 8A after real AM passes T35. Checkpoints 7, 8 and 9
remain after T31, T38 and T41. Retain all CI/backend gates and add
`make e2e-am-remote` to CI when T35 creates the real application target; it is
then part of later gates. This moves the process proof, not removes it or claims
it passed early. Root/configured metadata semantics, security and publisher
guard requirements are unchanged.

**Current queue:** T24b → T24c → T25 → T26 → T27 → T28 → T29 → T31 → T33 → T34 →
T35 → T37 → T38 → T39 → T41. Local SDK usability remains T24b/T24c; remote AM
adoption is T35. No remaining task builds or runs a pilot application.

## P32. Sequential task numbering and removal of transfer notes

Requested 2026-10-09. The executable list contains exactly **45 tasks**: T1–T30
complete and T31–T45 open. Existing acceptance criteria, evidence, task status,
delivery boundaries and checkpoint gates are preserved. Remove merged-task,
removed-pilot and deferred-task headings from the executable list. Attribution
work remains in P1 [#4827](https://github.com/constructorfabric/gears-rust/issues/4827),
without a P0 task ID. The earlier merges/removal are recorded in P30/P31 above.

The table maps actual tasks from the final pre-P32 queue, not task IDs from older
journal revisions. All live references use the new numbering; the journal above
uses the historical numbering. Checkpoints retain 0–9, 7A and 8A: they identify
review gates rather than implementation tasks.

| Task before P32 | Current task | Subject |
|---|---|---|
| T1 | T1 | Upgrade to `gts-rust` 0.12.0 via `[patch.crates-io]` |
| T2 | T2 | Migration for the 9 tables |
| T3 | T3 | SeaORM entities for the core six |
| T4 | T4 | Repositories on `DBRunner` |
| T5 | T5 | Transient `gts-rust` store built from database rows |
| T6 | T6 | Typed configuration |
| T7 | T7 | Acceptance path and operation records |
| T8 | T8 | Admission worker — one dependency-free candidate |
| T9 | T9 | REST — `POST /entities`, `GET /operations/{id}`, `GET /entities/{entity_key}` |
| T9a | T10 | Restore the v1 REST contract; the async surface moves to `/types-registry/v2/` |
| T10 | T11 | Registered Instances |
| T11 | T12 | Content revisions and compare-and-swap |
| T12 | T13 | Version-family shape and contiguity rules |
| T13 | T14 | Dependency edge extraction and writes |
| T14 | T15 | Reverse-impact traversal and artifact refresh |
| T15 | T16 | Revision-vector guard and bounded retry |
| T16 | T17 | Observability for the admission path |
| T17 | T18 | Compatibility against one baseline |
| T18 | T19 | Derivation chain and major-0 quarantine |
| T19 | T20 | Dependency-aware partial admission |
| T20 | T21 | Deletion and Dry Run |
| T20a | T22 | REST deletion and dry run |
| T21 | T23 | Outbox dispatch wiring |
| T22a | T24 | REST batchGet and discovery |
| T22b | T25 | Field projection on all three read routes |
| T22c | T26 | Discovery filters by GTS chain depth and entity kind |
| T22d | T27 | Freshness validators and conditional reads |
| T23 | T28 | Toolkit — publisher signature and publication status |
| T24 | T29 | `PlatformTypesRegistryApi` contract, models, local client, reconciliation and publication |
| T24a | T30 | `TypesRegistryApi` tenant contract, its extension helpers and local client |
| T24b | T31 | Database seeding and local ClientHub handoff — a new gear publishes in `init()` |
| T24c | T32 | Full Account Management local migration — one registry client and real REST proof |
| T25 | T33 | Toolkit — a platform-authenticated auth axis; one plane per route |
| T26 | T34 | Platform API over REST |
| T27 | T35 | Tenant REST and both resolving clients — complete SDK transport handoff |
| T28 | T36 | SDK client cache — freshness window, byte bound, `fresh` bypass |
| T29 | T37 | Toolkit post-wiring lifecycle — hook, supervision and `Required` readiness |
| T31 | T38 | Remaining-fleet cutover and one REST version — delete legacy, keep e2e green |
| T33 | T39 | Per-crate GTS collectors and the gear's `gts(…)` publication attribute |
| T34 | T40 | Publication ownership, dependent configured entities after wiring, and late-safe plugin selection |
| T35 | T41 | AM after wiring and remotely — host dependencies, bootstrap and readiness |
| T37 | T42 | Every remaining gear publishes after wiring; end of the pull |
| T38 | T43 | Out-of-process e2e run, HA and the deployment chart |
| T39 | T44 | Publisher guard — durable state, wire context and commit-time ordering |
| T41 | T45 | Publisher activation and mixed-version rollout proof |

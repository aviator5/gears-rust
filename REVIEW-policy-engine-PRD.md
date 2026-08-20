# PRD Review — Policy Engine

**Document**: `gears/system/policy-engine/docs/PRD.md` (1124 lines)
**Reviewed against**: `docs/spec-templates/gears-sdlc/PRD/template.md`, `docs/checklists/PRD.md`, `docs/arch/authorization/`, and the sibling written in the same commit (`gears/system/admission-control/docs/PRD.md`)
**Date**: 2026-08-21
**Reviewer**: gear-spec-review

## Verdict by axis

| Axis | Verdict | Findings |
|------|---------|----------|
| PRD-1 Industry alignment | CONCERNS | 9 (5 HIGH) |
| PRD-2 Contradictions | FAIL | 23 (1 CRITICAL, 4 HIGH) |
| PRD-3 Logical gaps | FAIL | 12 (5 HIGH) |
| PRD-4 Gear boundary | CONCERNS | 10 (2 HIGH) |
| PRD-5 Layer discipline | CONCERNS | 8 (2 HIGH) |
| Domain sweep (SEC/REL/DATA/COMPL/OPS/TEST/PERF) | FAIL | 12 (1 CRITICAL, 11 HIGH) |

This is the most carefully argued PRD in the repository after `types-registry`, and its two hardest boundaries —
against `authz-resolver` and against the gateway — are drawn deliberately and from both sides. Three things stop
it being ready to build from. Its p1 latency target is arithmetically unreachable by a configuration that
conforms to its own p1 cost-bound defaults, and the 20 ms it budgets for a hierarchy miss is not a latency
figure at all but the point at which the gear gives up and refuses. Its highest-volume interface accepts the
subject and tenant as payload fields, is registered unscoped so any in-process gear can resolve it, and no
requirement authorizes the caller — a position both sibling system gears take explicitly and oppositely. And a
recurring structural pattern runs through the whole document: DESIGN.md repeatedly invents the rule that a
missing requirement should have supplied — activation atomicity, the violations tenant boundary, decision
readiness, record immutability, snapshot consistency — so the design is more complete than the requirements it
traces to, and each invention is a design choice a later design is free to drop.

One correction carried over: finding 4 of `REVIEW-admission-control-PRD.md` claimed the two PRDs disagreed
about who owns the gate↔engine contract. They do not — `cpt-cf-policy-engine-contract-admission-engine` (§7.2)
agrees the gateway owns it. That finding is withdrawn there. The residual, which is real and belongs here, is
finding 3 below.

## Fix first

Sixty findings are not a work queue. Six are roots.

1. **Finding 1** — reconcile the latency arithmetic. It is p1, it is a stated release gate, and the gear's own
   default configuration violates it by 60 percent. Findings 24 and 31 close with it.
2. **Finding 2** — authorize the decision surface and derive subject and tenant from `SecurityContext`. One
   requirement, and the audit trail is forgeable without it. Findings 12 and 22 are the same defect on the read
   and management paths.
3. **Finding 5** — specify what a backend returns and how it maps to the closed outcome set. Everything
   downstream of `fr-outcome-combination` rests on a shape no document defines, and the facility that would
   define it does not exist. Findings 15, 26 and 30 all become tractable once it is fixed.
4. **Finding 9** — state management-path failure semantics and make activation atomic. Findings 10, 20 and 27
   are consequences of the same silence, and the failure mode they share is the one the gear exists to prevent:
   a silent transition to ungoverned permits.
5. **Finding 6** — put the resource tenant in the evaluation input contract. Three p1 requirements depend on a
   value the input does not carry and the gear is forbidden to look up.
6. **Finding 21** — fix the authorization/admission distinguishing test. The mechanism is sound; the criterion
   that justifies it is refuted by the authorization subsystem's own documents, and no counterpart document
   exists to correct it.

Findings 16, 17, 18, 21, 33 and 34 are conversations rather than edits: they ask whether the gear is scoped and
tiered right, not whether it is written right.

**A note on volume.** This report carries full detail for the two CRITICAL and twenty-eight HIGH findings, and
compressed entries for MEDIUM and LOW. That is a deliberate cut, not a complete list of everything the passes
raised: roughly fourteen further LOW observations about wording precision were dropped as below the bar.

## Findings

### 1. The p95 latency target is unreachable at the gear's own default configuration, and the miss budget is a refusal threshold rather than a latency figure

**Axis**: PRD-2 Contradictions
**Severity**: CRITICAL
**Location**: §6.1 line 746 · §5.7 line 560 · §5.7 line 569

**Issue**: Three p1 requirements set mutually unsatisfiable numbers, and the error has two independent halves.

**Evidence**:
> "p95 within 25 ms and p99 within 50 ms for a single evaluation, measured at the gear boundary over **all** evaluations, including those whose hierarchy lookup misses cache … A miss is bounded at 20 milliseconds by `cpt-cf-policy-engine-fr-dependency-timeouts`, which leaves the rest of the evaluation roughly 5 milliseconds on that path." (§6.1 `nfr-decision-latency`, p1)

> "Defaults are 5 milliseconds per document and **20 milliseconds per evaluation**." (§5.7 `fr-evaluation-cost-bounds`, p1)

> "The bound is operator-configurable with a default of 20 milliseconds, which is the figure `cpt-cf-policy-engine-nfr-decision-latency` allocates to a hierarchy miss." (§5.7 `fr-dependency-timeouts`, p1)

**(a) The arithmetic.** At shipped defaults an evaluation that misses hierarchy cache may consume 20 ms of
hierarchy wait plus 20 ms of evaluation and remain fully conformant with both FRs — 40 ms against a 25 ms p95
the NFR says the miss path must meet. The NFR asserts the residual is "roughly 5 milliseconds"; the FR
governing that residual defaults it to 20. Nothing caps the per-evaluation bound on the miss path.

**(b) The category error, which is the sharper half.** The 20 ms the NFR spends as the *cost* of a miss is not
a cost figure. `fr-dependency-timeouts` defines it as the point at which the gear stops waiting and **refuses**.
So on the NFR's own arithmetic, a hierarchy miss that consumes its allocated budget is a fail-closed refusal,
not a decision that met p95. The document states no expected miss-path latency anywhere — only a 2 ms p95 on a
cache *hit* (`nfr-hierarchy-latency`) and a timeout.

The error has already propagated: DESIGN.md line 114 repeats "what leaves the ~5 ms remaining beside a 20 ms
hierarchy miss" as its allocation rationale.

**Why it matters**: `nfr-decision-latency` is p1 and §12 makes it a release gate ("Hold the latency and
availability targets as release gates"). A gate whose pass condition is violated by the gear's own defaults
cannot be held: either implementation ships non-conformant defaults, or the bounds are silently re-cut and the
FR defaults become dead text. The budget is also an allocation out of IRM's 500 ms (verified: IRM PRD line
1286) shared with the gateway's 5 ms (admission-control PRD line 423), so a 15 ms unresolved error is spent
from a consumer's budget nobody is tracking.

**Proposal**: Change the FR side. Re-default `fr-evaluation-cost-bounds` so the per-evaluation bound fits inside
the residual the NFR allocates on the miss path, and show the arithmetic. Separately, split
`fr-dependency-timeouts` into an *expected* hierarchy-miss latency — the figure `nfr-decision-latency` may
allocate against — and a refusal timeout strictly greater than it. One 20 ms figure cannot be both.

---

### 2. The decision surface has no authorization posture, and the caller asserts the subject and tenant in the payload

**Axis**: Domain sweep · **Checklist**: `SEC-PRD-002`
**Severity**: CRITICAL
**Location**: §5.6 lines 509-513 · §7.1 lines 860-868 · §5.8 line 605

**Why applicable**: Both sibling system gears specify this at p1 and take the opposite position. `quota-enforcement` §5.13: "The caller's tenant identity and subject identity **MUST** always be derived from the SecurityContext (`subject_tenant_id`, `subject_id`, `subject_type`), never from request payloads. Operations submitting tenant or subject identifiers in payloads **MUST** be rejected." `types-registry` line 485: "Tenant ownership **MUST** derive from `SecurityContext`; a payload attempting to name an owner or global scope **MUST** be rejected."

**Issue**: `fr-evaluation-input` accepts "the subject and the subject's tenant" as request fields.
`interface-decision-client` is "registered in ClientHub **without scope**", so any in-process gear can resolve
it — not only the declared sole consumer. `fr-admin-authorization` scopes itself to "every **management**
operation". No requirement authorizes an evaluation request, verifies the caller's right to assert that
subject, or states that this is an on-behalf-of delegation with a trusted relayer.

**Evidence**: `grep -niE 'authoriz|SecurityContext|PDP|PEP|authenticat'` over the PRD hits §1.4, §4.2, §5.6,
§5.8, §11 and the §10 dependency row — none of them authorizes an evaluation. `fr-cross-tenant` bounds the
request against "the subtree the subject's context is entitled to reach", which is a scope check over the same
caller-supplied context, not an authorization of the caller.

**Why it matters**: Three consequences. Decision records attribute evaluations to a caller-asserted subject, so
the compliance evidence base can be forged by any in-process caller. `fr-denial-reason` returns "the identity of
the policy responsible", making the decision surface a disclosure oracle over another tenant's policy posture
that `fr-content-isolation` — scoped to "policy content reads and writes" — does not cover. And
`fr-emergency-access` is triggered by a request that "explicitly asserts emergency access", with only the
entitlement check between the assertion and the override. `fr-authorization-boundary` is written to make the
gear structurally unusable as an authorization path, but it constrains only what the input carries *about the
subject's entitlements*; it says nothing about whether the caller is entitled to speak for that subject. The
document's structural-boundary argument has a matching hole on the other side.

**Proposal**: Add a p1 `cpt-cf-policy-engine-fr-evaluation-authorization`: the gear **MUST** authorize every
evaluation request against the calling principal's own `SecurityContext`; the subject and subject tenant used
for scoping, recording and cross-tenant checks **MUST** derive from that context or from an explicitly
authorized on-behalf-of delegation whose relayer is itself authorized, and **MUST NOT** be taken from an
unauthenticated payload field; a caller not entitled to a tenant **MUST** receive the same result whether policy
exists there or not. Add the §9 criterion.

---

### 3. Two decision surfaces to one consumer, with incompatible registration scope and stability, and the document contradicts itself about which the gateway calls

**Axis**: PRD-2 Contradictions
**Severity**: HIGH
**Location**: §7.1 lines 862-866 · §7.2 line 917 · §2.2 line 139 · admission-control PRD §7.1 lines 504-507 and §5.3 line 290

**Issue**: Three incompatibilities on one p1 pair.

1. `interface-decision-client` says the gateway consumes it. `contract-admission-engine` says the engine trait
   is "**the only path** by which an evaluation reaches this gear". §2.2 says "Every evaluation this gear
   performs arrives through the gateway." All three cannot hold: either the gateway calls the unscoped decision
   client, falsifying "only path", or it does not, leaving a p1 stable interface with no declared consumer.
2. The gateway resolves its engine "through the types registry by its GTS identifier"
   (`cpt-cf-admission-control-fr-engine-selection`, p1), which under this platform's plugin convention is a
   *scoped* ClientHub resolution. An unscoped registration is not resolvable that way, so the decision client
   as typed cannot be what `fr-engine-selection` selects.
3. The stability tiers are opposite — stable/major here, unstable/minor on the gateway's plugin contract — for
   what §7.2 concedes is the same wrapped semantics ("the decision surface it wraps is specified here"). That
   undercuts `fr-deferral-outcome`'s reservation argument ("this contract is stable, and widening a shape
   callers already match on is a major version"): on the surface the gateway actually calls, widening is minor.

Additionally, `interface-decision-client` enumerates exactly two caller conformance expectations and neither is
a back-off signal — yet `nfr-decision-record` (p1) requires the gear to "signal the condition to callers as one
to back off from", and the gateway's `fr-engine-backoff` (p1) requires it to honour one. A p1 signal with
nowhere to travel on the surface this PRD says the gateway consumes.

**Why it matters**: This is the gear's principal contract and its only integration point. DESIGN.md has already
committed to "the decision surface is in-process only" without resolving which trait that is. An implementer
facing a stable/unscoped trait and an unstable/GTS-scoped plugin trait with overlapping semantics will build
both and hand-sync them — the drift both documents claim to prevent.

**Proposal**: Change this document, not the sibling. Stop claiming the gateway consumes
`interface-decision-client`; the gateway consumes `cpt-cf-admission-control-interface-engine-plugin`, which this
gear implements under `contract-admission-engine`. If a directly-callable decision client is genuinely wanted
for the §1.3 harness, say its consumer is the harness, register it under the same GTS instance scope, and align
its stability to the plugin contract's so the deferral and obligation reservation arguments stay true. Add the
back-off signal to whichever surface survives.

---

### 4. `fr-emergency-access` violates `fr-permit-provenance`, has no lawful permission cause, and is carved out of a threshold but not its acceptance criterion

**Axis**: PRD-2 Contradictions
**Severity**: HIGH
**Location**: §5.5 line 485 (p3) vs §5.5 line 411 (p1), §5.5 line 438 (p1), §9 line 1036, §5.9 line 636, §6.1 line 756

**Issue**: Four collisions.

1. The requirement names its exceptions precisely — `fr-denial-precedence` and the `nfr-fail-closed` threshold —
   and omits the one it most directly breaks:
   > "The system **MUST NOT** produce a permission from a failure. **No** error condition, unevaluable document, unreachable dependency, or unresolvable tenant context may yield a permit of either cause; **every one of them refuses**." (`fr-permit-provenance`, p1)

   against the threshold's own words: "permitting under a failed evaluation is precisely what it exists to do".
2. An emergency permit has no lawful cause. `fr-outcome-combination` closes the set at governed and ungoverned;
   a permit issued *over* a prohibition is neither. `fr-decision-records` then requires "the decision, and for a
   permission its cause: governed or ungoverned" for a decision no member fits.
3. `fr-decision-records` (p1) mandates "a marker where the decision was reached through the emergency path" —
   a field only a p3 requirement can populate, against a dependency §10 records as "no component provides this
   today".
4. Acceptance criterion §9 restates the fail-closed threshold **without** the carve-out the threshold carries,
   so the criterion and the NFR disagree.

**Why it matters**: `fr-permit-provenance` and `nfr-fail-closed` are the gear's central safety property, and this
is the one requirement that punches through it. An exception list that is explicitly enumerated and omits the
requirement it contradicts survives review and then gets implemented as written — a p1 MUST NOT that an
implementer must violate to satisfy a p3 MUST.

**Proposal**: Add `fr-permit-provenance` to the enumerated exception list, and add a third permission cause —
`emergency` — to `fr-outcome-combination`, reserved unpopulated at p1 on the pattern the deferral variant
already uses. Amend the §9 criterion to carry the same exclusion its NFR carries. Alternatively, if the path is
genuinely deferred, remove its p1 footprint: drop the record field and the threshold carve-out and reintroduce
both with the requirement.

---

### 5. What a backend returns, and how it becomes one of the closed outcomes, is specified nowhere

**Axis**: PRD-3 Logical gaps (fresh-reader)
**Severity**: HIGH
**Location**: §5.1 line 222 · §5.7 `fr-responsibility-boundary` line 578 · §10 line 1055

**Issue**: §5.1 says "Document content is opaque to this gear: it is source text the declared backend
interprets, not a structure the gear parses." `fr-responsibility-boundary` (p1) says the gear "**MUST** validate
every outcome a backend returns against the closed outcome set before combining it, and **MUST** reject a result
it cannot map to that set." Nothing defines that result shape — whether a backend yields permit/prohibit/observe
directly, a boolean, a named rule, or a structured object — nor how obligations (`fr-obligations`) and the
responsible-document identity (`fr-denial-reason`) escape opaque content. The facility that would define it
"does not exist yet, in any form, anywhere in the repository".

**Evidence**: A fresh reader given only this document recorded it as the single largest invention forced on an
implementer: "Two implementers will pick differently — one a boolean plus convention, one a structured object —
and every requirement downstream of combination silently changes with that choice."

**Why it matters**: Outcome combination, short-circuiting, denial reason, obligations and every decision-record
field that names a determining document all sit downstream of this mapping. `fr-responsibility-boundary`'s own
rationale says validating the returned outcome "matters more with a policy language than it would with a bare
expression evaluator, because a policy document can return an arbitrary shape rather than a boolean" — the
requirement identifies the hazard precisely and then does not say what the shape is.

**Proposal**: Specify the gear-facing result contract in `fr-responsibility-boundary`: the outcome discriminant,
how the responsible document identity is carried, and how obligations attach — independently of which backend
produces it, since the whole point is that the mapping is the gear's and the language is the facility's. If the
shape genuinely cannot be fixed before the facility exists, say so and add it to §13 with an owner, because four
p1 requirements are currently written against it.

---

### 6. The resource tenant is not in the evaluation input contract, and three requirements depend on it

**Axis**: PRD-3 Logical gaps (fresh-reader)
**Severity**: HIGH
**Location**: §5.6 `fr-evaluation-input` line 511 · §5.9 line 632 · §5.3 line 353 · §5.8 line 598

**Issue**: `fr-evaluation-input` enumerates "the subject and the subject's tenant, the action, the resource type
and the resource identifier where one exists, the tenant context including barrier handling and status
filtering" — the resource's tenant is not among them. Against which:

> "the resource's tenant, **which is the tenant the record is scoped and filtered by and is not always the subject's**" (`fr-decision-records`, p1)

> "order them by **proximity to the resource**" (`fr-nearest-tenant`, p1)

> "refuse an evaluation whose **resource tenant** lies outside the subtree" (`fr-cross-tenant`, p1)

And the gear cannot obtain it another way: the same requirement forbids retrieving resource state from the
consumer.

**Why it matters**: Assignment resolution, record scoping, the violations projection's tenant filter and the
highest-consequence refusal in the gear all key off a value the input contract does not carry. An implementer
will either default it to the subject's tenant — which `fr-decision-records` explicitly says is wrong — or
invent a lookup the requirement forbids.

**Proposal**: Add the resource tenant to `fr-evaluation-input` explicitly, and state whether "proximity to the
resource" in `fr-nearest-tenant` means tenant-tree distance from it. State the behaviour for a resource that is
not tenant-scoped or carries no identifier.

---

### 7. A p1 selection gate points at a field that does not exist in the contract it names

**Axis**: PRD-3 Logical gaps (fresh-reader)
**Severity**: HIGH
**Location**: §5.7 `fr-evaluation-isolation` line 549 vs §7.2 `contract-gts` line 893

**Issue**:
> "The system **MUST NOT** select a backend whose registration in `cpt-cf-policy-engine-contract-gts` does not declare a sandbox property covering both capability isolation and determinism." (p1)

`contract-gts` describes what the gear registers ("the policy resource types it exposes for management and its
error type family") and what it *resolves* ("the evaluation backend each document declares, the concrete
resource types its targets name, and the obligation identifiers its decisions carry"). It contains no backend
registration at all, and therefore no sandbox property. §13's first open question confirms the field has no
owner: "Which component owns the sandbox-and-determinism declaration…?"

**Why it matters**: A p1 gate on the gear's only sandbox guarantee references a field nobody writes, in a
contract that does not describe it. An implementation either treats the gate as vacuous — selecting any backend
— or invents a registration shape and an owner.

**Proposal**: Either extend `contract-gts` to describe the backend-build registration and its sandbox property,
naming who writes it, or move the declaration to the evaluation facility's own enumeration and restate the
selection gate against that. Widen §13's question to cover the denylist as well as the declaration (see finding 34).

---

### 8. Short-circuit evaluation destroys the shadow-evaluation mechanisms it silently outranks

**Axis**: PRD-3 Logical gaps (fresh-reader)
**Severity**: HIGH
**Location**: §5.5 `fr-short-circuit` line 449 (p1) vs §5.3 `fr-non-enforcing-assignment` line 362 (p2) and §5.5 line 438

**Issue**:
> "The system **MUST** stop evaluating the applicable set once no remaining outcome can change the result." (`fr-short-circuit`, p1)

> "A non-enforcing assignment **MUST** be evaluated and recorded exactly as an enforcing one, and its outcomes **MUST NOT** contribute to the result returned to the caller." (`fr-non-enforcing-assignment`)

A non-enforcing outcome can never change the result, so a literal short-circuit is entitled to skip it entirely
— destroying the requirement's whole purpose, which its own rationale states as measuring "a candidate against
live requests". The same collision hits the `observe` outcome ("An observe outcome never contributes") and the
p3 after-the-operation phase, which by construction cannot affect the result. No requirement orders the two.

**Why it matters**: The choice is externally visible, because `fr-decision-records` requires "the count of
documents that matched and the count actually evaluated" and `fr-deterministic-ordering`'s rationale treats a
difference between two records of the same decision as the specific harm it exists to prevent. Two defensible
readings produce different record contents.

**Proposal**: State the ordering in `fr-short-circuit`: short-circuiting applies to outcome contribution only,
and documents whose outcomes cannot contribute — non-enforcing assignments, observe outcomes, after-phase
documents — are evaluated and recorded regardless. Or say the opposite explicitly and accept that shadow
evaluation is partial and order-dependent.

---
### 9. The management surface has no failure semantics, and a failed activation leaves a bundle with zero active versions

**Axis**: PRD-3 Logical gaps
**Severity**: HIGH
**Location**: §5.2 `fr-lifecycle-states` line 266 · §6.1 `nfr-fail-closed` threshold line 756

**Issue**: Activation is a multi-step transition — deprecate the incumbent, promote the draft, publish, record
the administration event — and no requirement makes it all-or-nothing or says what a failure between steps
leaves behind. More broadly no requirement describes *any* management-path failure: what happens when
`authz-resolver` (p1) is unreachable, when the content store (p1) is unavailable, or when an activation fails
after its administration event is written. Every failure requirement in the document is written about evaluation;
`nfr-fail-closed`'s nine injected conditions are all evaluation-path conditions.

**Evidence**: `grep -inE 'atomic|half-appl|midway|rollback|transaction'` → one hit, "one atomic verdict" (batch
evaluation). DESIGN.md line 593 supplies "the transactional boundary that makes activation and its outbox
enqueue atomic" — a design invention with no requirement behind it.

**Why it matters**: An activation that deprecates the incumbent and then fails leaves the bundle with no active
version. Assignments survive activation by `fr-tenant-assignment`, so every evaluation under that bundle
silently becomes an ungoverned permit — a permission produced by a failure, which `fr-permit-provenance`
forbids at p1 and `nfr-fail-closed` was written to make impossible. The failure is invisible: it reports as
ordinary policy silence, and §5.3 already concedes an administrator "would have to notice the absence of
refusals to notice the fault."

**Proposal**: Add `cpt-cf-policy-engine-fr-management-failure-semantics` (p1): every lifecycle transition,
assignment change and its administration event applies atomically; a transition that does not complete leaves
the previously active version and assignment set unchanged and **MUST NOT** leave a bundle with no active
version where one was active; a management operation is refused, distinguishably from a validation failure, when
the authorization path, the types registry or the content store is unavailable. Add both to the injected set.

---

### 10. Optimistic concurrency does not reach activation, deprecation or assignment, so two concurrent activations can both succeed

**Axis**: PRD-3 Logical gaps
**Severity**: HIGH
**Location**: §5.2 `fr-optimistic-concurrency` line 293

**Issue**: The requirement covers "every modifying operation against existing policy **content**". But
`fr-lifecycle-states` is explicit that content is frozen at activation, so activation and deprecation change
lifecycle state rather than content — and an assignment is not policy content at all. Nothing states that
activation, deprecation, assignment creation or withdrawal, policy-priority change, effective-window change or
barrier-reach change carries a precondition.

**Evidence**: `grep -in 'precondition'` → `fr-administration-audit` ("the precondition token the caller supplied
**where the operation required one**"), this requirement, and the §8 use-case flow that *assumes* activation is
protected ("The activation is rejected on the precondition check"). `grep -in 'concurrent activ|race'` → no match.

**Why it matters**: Two administrators each holding a distinct draft of bundle B both activate; each precondition
passes because each is checked against a different row; both succeed — breaking "At most one version of a bundle
**MUST** be active at a time" and making the active version non-deterministic, which defeats the reproducibility
`fr-content-integrity`, `fr-version-history` and every decision record depend on. Concurrent policy-priority
changes silently discard one another, and priority is the tie-break `fr-nearest-tenant` uses to decide which
document a refusal names.

**Proposal**: Extend the requirement to cover lifecycle-state changes and assignment mutations, and require an
activation to take a precondition against the bundle's current active version so two activations racing on one
bundle cannot both succeed. Remove or qualify "where the operation required one".

---

### 11. `types-registry` is placed on the evaluation path with no timeout, no injected failure condition and no latency allowance

**Axis**: PRD-3 Logical gaps
**Severity**: HIGH
**Location**: §2.2 line 157 · §5.1 line 253 · §6.1 line 756

**Issue**: §2.2 states "the registry is on the authoring path **as well as the evaluation path**", and §5.1
describes resolution as evaluation behaviour. Yet `fr-dependency-timeouts` bounds only "each hierarchy
provider"; the injected set names the backend and the hierarchy provider but not the registry; and
`nfr-decision-latency` allocates 20 ms to a hierarchy miss and "roughly 5 milliseconds" to everything else, with
no allowance for a registry call. On the management path the gap is sharper: `fr-content-validation` requires
validation to cover registry resolution and is silent on whether an unreachable registry fails the validation or
skips the check.

**Why it matters**: If validation skips an unresolvable check, content activates with a target that can never
match — precisely what `fr-target-binding`'s rationale exists to prevent ("a typo… the failure mode an author is
least able to detect, because nothing happens"). On the decision path an unbounded registry call inside a 25 ms
budget is an unbounded stall whose outage behaviour nobody specified. DESIGN.md resolves the decision-path half
by moving all resolution to activation — which contradicts §2.2 and means the PRD's own claim about where the
registry sits is untested by any requirement.

**Proposal**: Make an unreachable registry a distinguishable validation failure that blocks activation. Extend
`fr-dependency-timeouts` to bound every synchronous dependency call, naming the registry. Add registry
unavailability to the injected set — or state that all resolution happens at activation and correct §2.2.

---

### 12. No requirement governs who may read decision records or the violations projection, or which tenant boundary applies

**Axis**: PRD-3 Logical gaps
**Severity**: HIGH
**Location**: §5.8 `fr-admin-authorization` line 607 · §5.9 `fr-violations` line 653

**Issue**: `fr-admin-authorization` enumerates four capabilities — read content, author drafts, activate and
withdraw, assign — and none for reading decisions or violations, though both `interface-management-client` and
`interface-rest-api` expose them. `fr-content-isolation` scopes "policy content reads and writes", not records.
The only statement of entitlement is inside a use case: "the prohibiting decisions … that the administrator is
entitled to see" — an entitlement no requirement defines.

**Why it matters**: The boundary question is not academic. `fr-decision-records` scopes a record by the
**resource** tenant while `fr-content-isolation` scopes content by the owning tenant, and §10 confirms "the
tenant a record is published under … is not always the subject's". An ancestor's bundle refusing an operation in
a descendant produces a record whose resource tenant is the descendant and whose determining content belongs to
the ancestor — so the two boundaries genuinely differ for exactly the entries a tenant administrator most wants.
`nfr-tenant-isolation` names the projection "the easiest place to leak" and asserts a zero threshold, but the
suite has no requirement to assert against. DESIGN.md line 117 invents the rule.

**Proposal**: Add read-decisions and read-violations as distinct capabilities in `fr-admin-authorization`, and
state in `fr-violations` that every projection read is authorized against the caller's own security context,
restricted to records whose resource tenant lies within the caller's entitled subtree, with entries outside it
indistinguishable from nonexistent. State which boundary governs when the two differ.

---

### 13. The refusal surfaces disclose an ancestor tenant's policy identity to a descendant, and the isolation requirement does not say whether that is permitted

**Axis**: PRD-2 Contradictions
**Severity**: HIGH
**Location**: §5.8 line 589 and §6.1 line 779 vs §5.9 line 653, §5.5 line 458, §5.3 line 355, §2.1 line 124

**Issue**: Inheritance means a descendant is routinely refused by a bundle owned by an ancestor.
`fr-denial-reason` requires the refusal to identify that document; `fr-violations` requires the projection to be
filterable by it; §8's use case has the administrator "inspect an entry to identify the policy that refused";
and `fr-nearest-tenant`'s rationale assumes the descendant is shown it ("rather than at an ancestor's guardrail
they cannot" change). The actor need states it outright: "visibility into which inherited policies constrain
them". Against which `fr-content-isolation` requires content of tenants the caller cannot manage to be
"indistinguishable from content that does not exist", and `nfr-tenant-isolation` sets "Zero cross-tenant reads …
including … the violations projection".

Strictly, `fr-content-isolation` governs *content* and a bundle identity is not content — so this is an
unresolved ambiguity rather than a flat contradiction. That is the finding: the document never says whether
disclosing an ancestor policy's identity to a descendant is a permitted exception or a leak, on the property it
calls "the highest-consequence failure in a multi-tenant platform".

**Why it matters**: The isolation test suite and the violations acceptance criterion cannot both be written
until this is settled, and an implementer will resolve it silently — either descendants get an unexplained
refusal naming nothing they can see, breaking the delegation story `fr-nearest-tenant` exists to deliver, or
ancestor policy identities flow into a tenant-scoped API.

**Proposal**: State the exception in `fr-content-isolation`: the identity and version of an ancestor bundle that
determined a refusal against the reader's own resources is visible to that reader; its content is not. Mirror it
in the `nfr-tenant-isolation` threshold so the suite encodes it. Do not weaken `fr-denial-reason` or
`fr-violations`.

---

### 14. Four bounds are required and the ones that govern cost are missing; the applicable-set bound can only be breached in production

**Axis**: PRD-3 Logical gaps
**Severity**: HIGH
**Location**: §5.10 `fr-operational-limits` line 720 · §13 line 1104

**Issue**: The requirement names four bounds — content size, document count per bundle, applicable set per
evaluation, batch size — and §13 asks only for their values. Three quantities that decide the cost those bounds
control are unbounded: the caller-supplied operation context (`grep` for `request size|payload` → no match),
targets per document and filters per target, and assignments per tenant. Applicable-set size is a property of
the assignment graph across a tenant chain, not of any one bundle, so `fr-content-validation`'s authoring-time
limit check structurally cannot evaluate it.

**Why it matters**: The applicable-set bound is the only one whose first breach lands on a live gated operation,
and `fr-operational-limits` requires that operation to fail. A tenant administrator makes every gated operation
across their subtree fail by assigning one bundle too many, with no authoring-time signal, and the failure
surfaces in the consuming gear. `fr-applicable-set`'s rationale claims the applicable set "is what makes
evaluation cost independent of total policy volume" — which holds only if *matching* cost is bounded, and
nothing bounds it.

**Proposal**: Add bounds on operation-context size, targets per document and filters per target, assignments per
tenant, and ancestry depth traversed. Require that any bound breachable only at evaluation time also be exposed
as a per-tenant utilisation metric and checked at assignment time, so the operation that crosses a bound is an
administrative one rather than a gated one.

---

### 15. Two acceptance criteria assert guarantees the requirements they test explicitly disclaim

**Axis**: PRD-2 Contradictions
**Severity**: HIGH
**Location**: §9 line 1047 vs §5.6 line 504 · §9 line 1049 vs §5.7 line 551 and §12 line 1084

**Issue**: (a) §9 states "no policy document can decide by role, permission, or group membership", while
`fr-authorization-boundary`'s own rationale records that "an author may still write a rule naming a subject
identifier, and a caller may put anything into the operation context … **Neither is closed here**." (b) §9
states "two evaluations of identical input produce identical outcomes", while `fr-evaluation-isolation` records
that determinism rests on a build-specific denylist which the §12 risk register says goes stale silently
("Nothing fails, no content is rejected"). The first clause of that criterion is also outside the gear entirely
— honouring the sandbox declaration is assigned to the facility, which §4.2 places out of scope.

**Why it matters**: Criteria that overstate their requirements either get signed off falsely — someone tests
only the closed half — or block release forever. Both sit on the gear's two stated security boundaries.

**Proposal**: Change the criteria, not the requirements; the requirements' honesty about their limits is the
better text. Restate as "The evaluation input carries no entitlement of the subject, so no policy document can
read a role, role binding, permission, scope or group membership from the gear" and "No capability is passed
into evaluation, and content referencing any builtin on the denylist for the backend build in use is rejected at
validation and activation."

---

### 16. No advice channel: obligations exist, the non-binding half does not, and a p1 consumer requirement asks for it by name

**Axis**: PRD-1 Industry alignment
**Severity**: HIGH
**Location**: §5.6 `fr-obligations` line 527 (p3) · §7.1 line 865

**Evidence**: `grep -ni "warning\|advice"` over the PRD → **zero matches**. The only caller-directed payload is
the obligation, whose conformance rule is "An obligation whose identifier the caller does not recognise makes
the decision prohibiting". Against: `cpt-cf-infrastructure-resource-manager-fr-policy-gating` (**p1**, IRM PRD
line 1134): "an allow verdict **MAY** carry obligations **or warnings** from the decision service, and the
system **MUST** deliver them to the caller unaltered."

XACML 3.0 names the distinction: §2.12 requires a conformant PEP to deny unless it can discharge all
obligations; §2.13 says advice "may be safely ignored by the PEP". The admission family carries the same split
— Gatekeeper's `enforcementAction: warn`, Kubernetes `validationActions: [Warn, Audit]`.

**Why it matters**: The only enforcing gear this document names has a p1 requirement for a payload this gear
cannot produce at any tier. An author who wants to say "allowed, but you are near the limit" must either emit an
obligation — which turns the permit into a refusal at any caller that does not recognise the identifier — or say
nothing. It also forecloses the safest rollout path in the comparable set: ship a rule as advice, watch what it
would have said, then convert it.

**Proposal**: Add `cpt-cf-policy-engine-fr-advice` at the tier the consumer requires (p1): a decision of either
outcome may carry advice items with GTS identifiers resolved on the same terms as obligations, with the inverse
caller-conformance rule stated in `interface-decision-client` — an unrecognised advice identifier is ignored and
does not change the decision. Reserve the field from v1 as obligations and deferral already are. Cite XACML
§2.12/§2.13 so the split is not re-litigated in DESIGN.

**Sources**: XACML 3.0 — https://docs.oasis-open.org/xacml/3.0/xacml-3.0-core-spec-os-en.html · Gatekeeper — https://open-policy-agent.github.io/gatekeeper/website/docs/violations/

---

### 17. No exemption mechanism, in a model where deny-overrides plus tree inheritance makes one structurally necessary

**Axis**: PRD-1 Industry alignment
**Severity**: HIGH
**Location**: §5.3 lines 336-374 · §5.5 `fr-denial-precedence` line 420

**Evidence**: `grep -ni "exempt\|waiver"` → **zero matches**. The combination rule is absolute, and §5.3
forecloses the usual escape by design: "a prohibition anywhere in the chain refuses regardless of where it sits
… position cannot be allowed to decide anything, or a descendant could remove a constraint its ancestor is
accountable for." The only override is `fr-emergency-access` (p3), which is subject- and incident-scoped and
requires an entitlement §13 says nobody provides.

Every comparable that assigns policy down a hierarchy ships a first-class exception object. Azure Policy
exemptions carry a category (Waiver / Mitigated), `expiresOn`, `resourceSelectors`, approval metadata, a
dedicated `exempt/Action` permission, and a compliance substate showing what the state would be without the
exemption. Kyverno's PolicyException is motivated by precisely this platform's shape — "a team responsible for
policy authoring and administration may not be the same team responsible for submission of resources".

**Why it matters**: Today the only way to relieve a descendant from an ancestor guardrail is to edit the
guardrail's targets, which changes the rule for everyone under it, leaves no record of who was excepted or why
or until when, and makes a scope narrowing and an exception grant indistinguishable in the audit trail. That is
the defect §5.2 argues against for rollback, applied to the inheritance axis. It also means a legitimately
accepted non-compliance stays in the refusal projection forever.

**Proposal**: Add `cpt-cf-policy-engine-fr-exemption` (p2): a scoped, expiring, attributable exception naming the
assignment it suspends for a named subtree or resource, with a required category and justification, granted
under a capability distinct from read/author/activate/assign and never grantable by the subtree it exempts,
recorded as an administration event, and surfacing on the decision record as a third permission cause so an
exempted permit is not counted as ungoverned. If exemptions are deliberately excluded, say so in §4.2 and record
how an ancestor grants a one-off relief instead.

**Sources**: https://learn.microsoft.com/en-us/azure/governance/policy/concepts/exemption-structure · https://kyverno.io/docs/guides/exceptions/

---

### 18. The author's only pre-activation feedback loop is deferred past p1, in a category where shadow mode ships in v1 universally

**Axis**: PRD-1 Industry alignment
**Severity**: HIGH
**Location**: §5.9 `fr-dry-run` line 707 (p3) · §5.3 `fr-non-enforcing-assignment` line 360 (p2) · §5.2 `fr-version-comparison` line 318 (p2)

**Issue**: The PRD states the problem twice and tiers the fix out of first release both times — "Activation is
otherwise the only way to learn what a policy refuses, which makes every rollout a production experiment"
(p3) and "Activating a new bundle is otherwise the only way to learn what it does to real traffic, which makes
every rollout a production experiment" (p2). The p1 set leaves one shadow mechanism, the `observe` outcome,
which §5.3 itself disqualifies as "authored into content and cannot be turned off without editing it". The
Policy Author actor's stated need — "the ability to see what a policy will refuse before it takes effect" — has
no p1 requirement serving it.

Gatekeeper ships `enforcementAction: [deny, dryrun, warn]` and recommends it for testing; Kubernetes puts
`validationActions: [Deny | Warn | Audit]` on the **binding**, the direct analogue of this gear's assignment;
Azure Policy assignments carry `enforcementMode: DoNotEnforce`. In none is the shadow mode a later phase.

**Why it matters**: The gear fails closed and sits in the admission path of every gated operation. A tenant
administrator's first mistake in a new guardrail refuses production traffic, and at p1 there is no way to
discover that except by causing it. Promotion is cheap because the modelling is already done: §5.2 states scope,
window and enforcing flag compose independently, so `fr-non-enforcing-assignment` is a predicate on outcome
contribution, not a second evaluation path.

**Proposal**: Promote `fr-non-enforcing-assignment` to p1 and `fr-dry-run` to p2. If either stays, add a
sentence to §12 stating why this platform can accept a first release in which activation is the only way to
discover what a rule refuses, given that no comparable does.

**Sources**: https://open-policy-agent.github.io/gatekeeper/website/docs/violations/ · https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/ · https://learn.microsoft.com/en-us/azure/governance/policy/concepts/assignment-structure

---

### 19. Wholesale suppression of context values in decision records is an outlier, and the PRD's own downstream claims depend on the difference

**Axis**: PRD-1 Industry alignment
**Severity**: HIGH
**Location**: §5.9 `fr-record-confidentiality` line 662

**Issue**: The requirement bans "the values of caller-supplied operation context" outright, with no opt-in at
any tier. The consequences are conceded in three places: `fr-version-comparison` ("a recorded decision cannot be
replayed against a candidate version"), §11 ("the record shows that the plan was read, never which plan it
was"), against `nfr-decision-record`'s promise of records "carrying enough context to reconstruct the decision
without access to the original request".

OPA's decision-log model is the reference for the opposite trade: a masking policy returns JSON Pointers naming
fields to erase, the default is to record, redaction is declared, and "the erased paths are recorded on the
event itself" — so a reader can tell redaction from absence. The rationale for the inversion ("the gear has no
schema for them, and a gear that cannot classify a value cannot decide whether it is safe to keep") is sound
about the *default* and does not support the absence of a *declaration mechanism*: OPA has no schema for its
input either, and solves it by making the author declare.

**Why it matters**: Three requirements are weakened and only one says so. The reconstruction claim is
unachievable for any value-dependent decision — the entire guardrail class the gear exists for.
`fr-version-comparison` is forced into a sound-but-imprecise static over-approximation because it cannot replay
real traffic. And the Security Auditor's stated need cannot be met by the field set.

**Proposal**: Keep deny-by-default, add a declaration. Let a document or target name a bounded set of context
property names as recordable, validated at activation; record values only for declared names; require the record
to carry the list of names read-but-suppressed so a reader can distinguish redacted from unread. Then either
state that `nfr-decision-record`'s reconstruction guarantee holds only over declared-recordable fields, or weaken
the threshold to match what the record carries.

**Sources**: https://www.openpolicyagent.org/docs/management-decision-logs

---

### 20. A self-recorded content digest is corruption detection, not tamper-evidence

**Axis**: PRD-1 Industry alignment
**Severity**: HIGH
**Location**: §5.2 `fr-content-integrity` line 282

**Issue**: The digest is computed by the gear, stored by the gear, and checked by the gear against a value in
the same database as the content it protects. That detects a bit flip. It does not detect an actor with write
access to that store, because such an actor rewrites the digest row in the same transaction — and the rationale
names that actor: "Undetected modification, whether from storage corruption **or tampering**, silently changes
what the platform permits." OPA's answer is a signed bundle: `.signatures.json` carries JWTs binding the file
list and per-file hashes, verified against a key supplied out-of-band. The key is the missing part: an integrity
claim is tamper-evident only if the verifier holds something the tamperer does not.

**Why it matters**: The rationale makes a security claim the mechanism does not support, and the requirement is
well-formed enough that a designer will implement exactly what it says and consider the threat covered.
`nfr-fail-closed` already enumerates "malformed or digest-mismatched policy content", so the fault-injection
harness passes against a mechanism that stops nothing. For a gear whose administration surface it calls "a
high-value target", overstating the integrity control is the wrong direction to be wrong in.

**Proposal**: Pick one and say it. Either narrow the rationale to storage corruption and record the residual
tampering risk in §12 with the store's access controls as the compensating control; or require the digest to be
signed at activation with a key the gear does not keep in its content store, with signature failure joining the
`nfr-fail-closed` set. Name OPA's bundle-signing model either way.

**Sources**: https://www.openpolicyagent.org/docs/management-bundles

---

### 21. The authorization/admission distinguishing test is refuted by the authorization subsystem's own model and shipped SDK

**Axis**: PRD-4 Gear boundary
**Severity**: HIGH
**Location**: §5.6 `fr-authorization-boundary` rationale, line 504

**Issue**: The structural mechanism — withholding entitlements from the input — holds and is load-bearing. The
criterion offered to justify it does not:
> "a rule decidable from the subject's grants alone is authorization, and a rule needing the proposed values of the change is admission"

Both halves fail. **Proposed values already reach the authz PDP**: the shipped SDK carries the shape today —
`gears/system/authz-resolver/authz-resolver-sdk/src/models.rs:117`, `properties: HashMap<String, serde_json::Value>`,
"Additional resource properties for policy evaluation" — and `docs/arch/authorization/DESIGN.md` states "For
CREATE operations: typically `true` (PEP validates INSERT against constraints — tenant isolation, ownership)",
with `AUTHZ_USAGE_SCENARIOS.md` S05 supplying `owner_tenant_id` from the POST body. **Authorization is not
decidable from grants alone**: `PERMISSION_GTS_TYPE.md` permits "Query Language predicates (GTS §3.3) —
`gts.cf.core.ai_chat.chat.v1~[category='support']`. Allows ABAC-style attribute constraints."

**Why it matters**: As written the test classifies the authz-resolver's own CREATE flow as admission, and an
attribute-predicate permission as something other than authorization. It is the only guidance a future author
gets for deciding which component a new rule belongs in, and a wrong test produces exactly the outcome §5.6
exists to prevent — two components answering overlapping questions with independent lifecycles and no obligation
to agree — while the document reads as though the boundary were closed. `gears/system/authz-resolver/` has no
`docs/`, so no counterpart exists to correct the record and this PRD's test becomes the platform's de facto rule.

**Proposal**: Replace the input-based test with one the authorization model agrees with, and which this
requirement already contains: a rule that changes what a subject is permitted to do is authorization; a rule
that can only narrow an operation the subject is already authorized to perform is admission — which is precisely
the "a decision **MUST NOT** widen access" clause. Then work S05 explicitly as the case where both subsystems
see the proposed values, and say which owns the rule and why.

---

### 22. Management and REST surfaces do not state which authorization plane they use, and §7.2 carries no platform auth contract

**Axis**: Domain sweep · **Checklist**: `SEC-PRD-002`
**Severity**: HIGH
**Location**: §7.1 lines 870-884 · §7.2 · §5.8 line 605

**Issue**: None of the three interface entries mentions authentication or authorization. `fr-admin-authorization`
specifies one undifferentiated mechanism ("the caller's own security context") for an actor set that spans both
planes — `actor-platform-operator` deprecates any tenant's bundle and marks assignments as reaching through
barriers, `actor-tenant-policy-admin` is confined to a subtree. `grep -ci "plane"` over the PRD → **0**. The
platform defines the split: `docs/arch/toolkit-oop/ADR/0008-cpt-cf-adr-two-plane-auth.md` and `ADR/0006`, neither
referenced here. `types-registry` does it properly — "Type: **Authenticated** REST API", a p1 plane-split
requirement, and a §7.2 `contract-platform-auth` entry.

**Why it matters**: The four capabilities `fr-admin-authorization` separates are the levers deciding what the
platform refuses. The plane question decides whether operator-level cross-tenant guardrail management is
authorized by a tenant PDP grant — which a tenant administrator might obtain — or by a platform identity.
Leaving it to DESIGN means the first implementation picks, and picking the tenant plane for a platform-plane
operation is a privilege-escalation path into the platform's policy authority.

**Proposal**: State per capability which plane authorizes it and by what mechanism, that cross-tenant operations
are unreachable from the tenant REST surface, and that unauthenticated requests are rejected before any
authorization check. Mark both client interfaces and the REST API as authenticated and name the plane each
serves. Add a §7.2 `cpt-cf-policy-engine-contract-platform-auth` (p1) referencing ADR-0008.

---

### 23. No integrity or non-repudiation requirement on decision records or administration events, while policy content gets a p1 digest

**Axis**: Domain sweep · **Checklist**: `SEC-PRD-004`
**Severity**: HIGH
**Location**: §5.9 line 625 · §5.2 line 273 · contrast §5.2 line 282

**Issue**: `grep -niE 'tamper|immutab|append-only|non-repudiation|signature'` → `tamper` once, inside the
content-integrity rationale; `immutab` only about bundle versions; `append-only` and `non-repudiation` zero.
`fr-decision-records` enumerates a field set whose only write-once property is "an evaluation identifier, unique
per evaluation, under which the record is written exactly once" — a de-duplication property, not tamper-evidence.
`fr-administration-audit` requires retention and attribution but no protection from modification. DESIGN.md line
1217 asserts the store is "append-only by construction" with no requirement behind it.

**Why it matters**: An audit trail that can be edited without detection is not evidence. The §1.3 goal
"Decisions are reconstructable after the fact", the violations projection, the emergency marker whose rationale
says "An unmarked one is indistinguishable from an attack", and the Security Auditor actor all rest on a store
with no stated integrity property. It is also the one control that would detect the attack
`fr-admin-authorization` exists to prevent: an escalating administrator who activates a permissive version and
then removes the administration event leaves nothing behind. The asymmetry with content is explicit and
unexplained.

**Proposal**: Add p1 `cpt-cf-policy-engine-fr-record-integrity`: decision records and administration events are
write-once within the gear, with the retention sweep and the pseudonymisation of `fr-subject-data-handling` as
the only permitted modifications and both recorded as administration events; no interface mutates a written
record; tampering or gaps are detectable. State non-repudiation, or delegate it explicitly in §6.2 naming the
component that discharges it.

---

### 24. Retention removal and subject erasure are specified only over the gear's own store, not the exported stream

**Axis**: Domain sweep · **Checklist**: `SEC-PRD-003`
**Severity**: HIGH
**Location**: §2.2 line 163 · §5.9 lines 671 and 680 · §10 event-broker row

**Issue**: The gear exports every decision record — subject identifier, subject tenant, resource tenant — to a
topic whose "storage backend … owns their retention, compaction, and deletion" and which §10 says adds
"retention **beyond this gear's window**". Both data-protection controls are scoped to the copy the gear
controls: `fr-record-retention` removes records from its own store; `fr-subject-data-handling` erases "from **the
record store**". Nothing propagates either to the export. §6.2 relies on erasure to dispose of the whole
data-subject-rights category.

**Why it matters**: An erasure control leaving a full-fidelity copy of the same personal data in a
longer-retained downstream store does not discharge the obligation it was written for, and "irreversibly" is
true of one copy only. The same gap makes the retention period unenforceable — the export is exactly the
unbounded retention the requirement's rationale argues against. Because the topic is shared with
`admission-control`, the ambiguity spans two gears with no stated owner.

**Proposal**: Either emit an erasure/tombstone event to the audit topic for a pseudonymised subject and name the
owner of the topic's retention, or export only pseudonymous subject references from the outset. Record the
disposition in §6.2, and add a §13 question if the owner is undetermined — as §13 already does for the emergency
entitlement.

---

### 25. Policy content and administration events are retained forever, with no disposal, archival or tenant-offboarding path

**Axis**: Domain sweep · **Checklist**: `DATA-PRD-003`
**Severity**: HIGH
**Location**: §5.2 lines 266, 300-302, 275 · §5.10 line 719

**Issue**: "delete a draft, which is the only deletion the lifecycle permits"; retained versions are unbounded;
administration events must be retained "at least as long as the content versions they describe", i.e. forever.
`fr-operational-limits` bounds nothing cumulative. `grep -niE 'offboard|tenant deletion|purge|dispose|archiv'` →
**zero hits**. Decision records get a full retention lifecycle; the other data set the gear owns gets none.
`quota-enforcement` §6.2 carries an explicit retention table with a reclamation mechanism per data class and
marks where indefinite retention is a deliberate choice.

**Why it matters**: A multi-tenant store that only grows, with no offboarding path, is a compliance exposure — a
departed tenant's content and the administration events naming its administrators remain indefinitely — and an
unbounded storage commitment. It also degrades `nfr-durability`, whose one-hour restore objective is stated for a
corpus with no ceiling and verified by an exercise that gets slower every release. The gear stores tenant-authored
opaque source text it cannot classify, so the undeletable store may hold anything an author wrote.

**Proposal**: State whether activated versions are permanently immutable by design, and if so say so and note the
accepted growth. Specify a tenant-offboarding path. Add a retention/reclamation statement per data class in the
shape of quota-enforcement's table, and add version-history depth to the bounds §13 already asks for.

---

### 26. No recovery objective for decision records, administration events, or the violations projection

**Axis**: Domain sweep · **Checklist**: `REL-PRD-002`
**Severity**: HIGH
**Location**: §6.1 `nfr-durability` lines 835-841

**Issue**: The requirement is scoped to "policy content, versions, and assignments", and its rationale disposes
of the rest in one sentence: "Decision records are covered by their own retention requirement and are not in
scope here." Retention is a deletion policy, not a durability guarantee, so the exclusion substitutes an
unrelated requirement for the obligation. Administration events — which the document argues must outlast every
decision a rule produced — are covered by neither.

**Why it matters**: Loss of the record store destroys two §1.3 goals with no stated tolerance. Worse,
`nfr-decision-record` makes the same store a **serving condition**: "Where the system cannot bring a new decision
inside that window … it **MUST** stop returning permitting decisions." So record-store loss is simultaneously an
audit-evidence loss and a total refusal of every gated operation across every consuming gear — the gear's
highest-impact failure, and the one with no recovery objective.

**Proposal**: State RPO and RTO for decision records and administration events, verified by the same restore
exercise, and note that the violations projection inherits the record objective. Replace the
"covered-by-retention" sentence with the actual disposition. If the exported topic is the intended recovery
source, say so and give that procedure's objective.

---

### 27. The gear's only durability requirement is p2, so first release may ship with no recovery capability for content whose loss disarms it

**Axis**: Domain sweep · **Checklist**: `REL-PRD-002`
**Severity**: HIGH
**Location**: §6.1 `nfr-durability` line 835 · §1.3 line 65

**Issue**: All five §1.3 outcomes are due at "p1 complete", defined as every p1 requirement in §§5-6. The
distribution is 47 p1 / 6 p2 / 7 p3, so p2 is genuinely a lower tier. Against which the requirement's own
rationale: "Loss of policy content does not degrade the gear, **it disarms it**: with no content every decision
becomes an ungoverned permit … The gear is also the only holder of the version history that audit depends on,
and that history cannot be reconstructed from any other system."

**Why it matters**: Every other property protecting against the same class of harm is p1 — fail-closed, content
integrity, cache safety, decision-record completeness — each because a silent transition to permissive behaviour
is what the gear exists to prevent. Durability is the only barrier between storage loss and exactly that
transition, and the only backup/restore obligation in the document. The failure mode is uniquely quiet: content
loss produces ungoverned permits, not errors, so the estate keeps operating with every guardrail removed and
nothing alarms except a counter someone must be watching. §12 carries no risk row for content loss.

**Proposal**: Raise to p1, or state why recovery is acceptable to defer past "p1 complete" given its own
rationale — naming a platform-level backup obligation that covers the store before first release. If it stays
p2, add the consequence to §12 with an owner.

---

### 28. No availability requirement is placed on any dependency, though the gear's own target is argued from compounding

**Axis**: Domain sweep · **Checklist**: `ARCH-PRD-004`
**Severity**: HIGH
**Location**: §6.1 `nfr-availability` lines 763-770 · §7.2 line 897 · §10

**Issue**: The gear argues its own target upward — "a dependency in its admission path that merely matched it
could not leave the consumer room to reach its own figure, **since independent outages compound**" — and never
applies the reasoning downward. `contract-hierarchy-read` states only the protocol and additive-change
tolerance; §10 describes what each dependency provides with no service expectation; `nfr-hierarchy-latency`
constrains only the cache-hit path and the hit rate the gear itself must sustain.

**Why it matters**: At the 90 percent hit rate the gear permits, one evaluation in ten depends on
`tenant-resolver` being up, and the gear refuses when it is not — so the gear's availability is arithmetically
bounded by a dependency nobody has been asked to guarantee, and a `tenant-resolver` outage is total refusal of
every gated operation across every consuming gear. The same holds for the record storage that
`nfr-decision-record` turns into a serving condition, and for the evaluation facility. The target is
unachievable and unverifiable as written: it can be missed entirely through a dependency meeting whatever
unstated target it holds, with neither team in breach.

**Proposal**: State per p1 dependency the availability and latency the gear requires and what it does when they
are not met — at minimum `tenant-resolver` with the miss-path share explicit, the record storage, and the
evaluation facility. Put the figures in §7.2 or §10, and add the shortfall to §12 if the dependencies cannot
commit before first release.

---

### 29. Two p1 confidentiality and data-protection requirements have neither an acceptance criterion nor a verification method

**Axis**: Domain sweep · **Checklist**: `TEST-PRD-001`
**Severity**: HIGH
**Location**: §5.9 lines 662 and 680 · §9 lines 1032-1049

**Issue**: `fr-record-confidentiality` and `fr-record-retention` are both p1 and both carry no **Verification
Method** — a field the document uses where it means it (`nfr-fail-closed`: "Fault injection across the
enumerated failure conditions"; `nfr-durability`: "Restore exercise from backup into a clean deployment"). §9's
eighteen criteria contain no hit for credential, token, confidential, redact or property value; the only
retention criterion verifies the projection's read path, not the removal obligation or the volume metric. Every
other security-relevant p1 property does have a criterion — fail-closed, dependency-vs-policy distinguishability,
isolation, self-grant prevention, entitlement withholding, sandbox and determinism.

**Why it matters**: The §5 preamble's blanket coverage target does not close this: coverage does not verify a
negative about record content, and both requirements are exactly the kind whose violation is invisible in
passing tests — one extra field in a record, or a sweeper that never runs.
`fr-record-confidentiality`'s own rationale argues its whole value is being "checkable by inspecting the record
shape"; if nobody is required to inspect it, the check does not happen. And §6.2 leans on `fr-record-retention`
to dispose of the entire regulatory category.

**Proposal**: Add a Verification Method to both — record-shape inspection against an allow-list derived from
`fr-decision-records` plus a negative test submitting a credential-shaped context value; and a time-advanced
sweeper test asserting removal and observability. Add matching §9 criteria.

---

### 30. No alerting or incident-response requirement, although the document repeatedly relies on alerting existing

**Axis**: Domain sweep · **Checklist**: `OPS-PRD-002`
**Severity**: HIGH
**Location**: §5.9 `fr-metrics` line 689 · §6.2

**Issue**: `fr-metrics` requires exposure only. `grep -niE 'alert|on-call|runbook|escalat'` → `alert` twice,
never as a requirement: once in a rationale ("Conflating them **hides outages from alerting**") and once as a
§12 risk mitigation ("treat a policy that never matches as an **alertable condition**"). §6.2's seven exclusions
do not mention monitoring, alerting or incident response. Checked for inheritance:
`docs/ARCHITECTURE_MANIFEST.md` §12 lists OpenTelemetry init, gateway request IDs, `/health` endpoints and rate
limiting — no alerting or incident-response model to inherit.

**Why it matters**: The gear's failures are silent by construction: it fails closed, so an outage looks like
policy working, and on content loss it effectively fails open, which looks like nothing at all. Two conditions
are defined and unwired — the ungoverned-permit count, the sole signal that content has been lost or an
assignment has broken, and the fail-closed-by-cause counter, the sole signal separating an outage from a policy
change. Metrics without a required alert on those two means the platform learns about both from the consuming
gear's users.

**Proposal**: Add a p1 requirement naming the conditions that must be alertable: fail-closed refusals by cause
above a threshold, a sharp move in the ungoverned share, the decision-record durability window exceeded (which
stops all permits), hierarchy provider failure rate, activation-propagation delay exceeding its window, and
emergency-access use. State the incident-response expectation for a decision-surface outage — or add the whole
area to §6.2 with the component that owns it, the way the operator-documentation bullet already does.

---

### 31. No performance expectation for the management and violations query surface, which the availability requirement nevertheless measures

**Axis**: Domain sweep · **Checklist**: `PERF-PRD-001`
**Severity**: HIGH
**Location**: §6.1 line 741 · §5.9 line 651 · §7.1 line 879

**Issue**: Every time figure in §6 belongs to the decision path or to propagation. `fr-violations` defines a
filtered query over the record store with no response-time or result-size expectation; `interface-rest-api`
names pagination conventions but no target. Meanwhile `nfr-availability` measures the surface — "management
availability is measured at the REST surface" — and grants it a 4-hour monthly maintenance window. The surface
is a first-class SLO target for uptime and unspecified for speed.

**Why it matters**: The violations projection is a filtered scan over a store growing at the platform's full
gated-operation rate, it is one of the five §1.3 outcomes, and its §9 criterion ("An administrator can retrieve
every refusal for their tenant within the retention window") is untestable because nothing says what "retrieve"
must cost. A goal stated as retrievability with no time bound is met by a query that takes ten minutes — the
difference between the tool administrators use instead of reading logs and the tool §12 already worries will be
"delivered, unused". The absence also leaves the read path unconstrained against the write path, which is the
most plausible way a management operation damages the 25 ms decision budget.

**Proposal**: Add a p1 threshold for the management surface: bounds on violations queries and content
list/read at the record volume implied by the reference load and retention window, a maximum page size, and a
statement that management-surface work must not consume the decision path's budget. Tie the §9 criterion to that
figure.

---

### 32. The regulatory exclusion asserts no regime attaches, while the gear processes personal data and implements an erasure right

**Axis**: Domain sweep · **Checklist**: `COMPL-PRD-001`
**Severity**: HIGH
**Location**: §6.2 line 852 · §5.9 line 671

**Issue**: "The gear holds no payment, health, or financial-reporting data, so **no scheme-specific regime
attaches to it**." `grep -niE 'GDPR|HIPAA|PCI|SOX|CCPA'` → **zero hits**. But the gear stores subject
identifiers per evaluation at request rate, retains them, exports them, and provides pseudonymisation-based
erasure — controls that only make sense under a data-protection regime the document never names. The delegation
is also unverifiable: "discharged where subject identity is owned" names no component, while §4.2 and §11 name
`authn-resolver` for identity elsewhere. `quota-enforcement` names each regime and dismisses it individually,
then names the component that owns PII.

**Why it matters**: An exclusion that overstates its reach converts a considered decision into a wrong one: the
reader concludes no data-protection analysis is needed, while the gear is one of the platform's larger
per-request personal-data producers and the only holder of the correlation between a named subject and every
operation the platform refused them. Naming no regime also leaves `fr-subject-data-handling` without a stated
driver — which is how a data-protection control ends up deferred past first release on priority grounds (it is p2).

**Proposal**: Name the regimes individually as quota-enforcement does: PCI DSS, HIPAA and SOX do not attach and
why; data-protection law does attach to the decision record; cite the requirements discharging minimisation,
storage limitation and erasure; name the component owning consent, access and portability. Reconsider
`fr-subject-data-handling`'s p2 tier against §1.3's definition of "p1 complete".

---

### 33. "Neither generalises" is inaccurate for `event-broker`, whose filter-engine contract is the closest seed for the facility this gear makes a hard prerequisite

**Axis**: PRD-4 Gear boundary
**Severity**: HIGH
**Location**: §1.2 line 59 · §5.7 line 541 · §3.1 line 175 · §10 line 1055

**Issue**: §1.2 dismisses both neighbours with "Both evaluate policy over state they own themselves, and both
keep their engine registry **local to that domain**", and §5.7 says "the platform already has two gears that
each **built** one". Both halves are wrong on the part that matters. `event-broker` PRD line 360: additional
engines "**MUST** be pluggable via the same GTS-typed registry pattern used for storage backends"; its ADR-0005
calls this the "**Symmetric plugin pattern**: filter engines plug in via the same GTS-typed `types_registry` +
`ClientHub` resolution as storage backends and OAGW auth/guard/transform plugins". The registry is the
platform-wide idiom, not a local one; only the `bool` return and the event-only filter context are domain-shaped.
And neither evaluator is built: `gears/system/quota-enforcement/` contains only `docs/`, and no evaluator crate
appears in `Cargo.lock`. Separately, §10's absolute "Does not exist yet, **in any form, anywhere in the
repository**" and §3.1's "no such library, gear, or specification is present" are falsified by §5.7's own
sentence and by both neighbours' specifications.

**Why it matters**: This is the schedule-critical dependency of the whole gear, and of the sibling. If
generalising `FilterEngine` is viable, the prerequisite becomes a contract widening with an existing owner
rather than a new unowned platform component; if it is not, the reason needs to be on the record before two
gears commit to waiting. The current justification cannot bear the weight placed on it, and §12 records the
non-arrival as unmitigable partly on its strength.

**Proposal**: Separate contract shape from registry mechanism: `quota-enforcement`'s registry is gear-local,
`event-broker`'s is not. State why `event-broker`'s `compile()/eval()` contract cannot be widened to a
value-returning result over a caller-supplied context — or record that widening it is the candidate path, with
an owner. Correct "each built one" to "each specified one", and restate §10 and §3.1 as "no **shared** policy
evaluation facility exists", naming the two domain-local engines.

---
## MEDIUM findings

Compressed. Each carries location, the defect, and the change.

**34. The determinism denylist is duplicated across both same-commit siblings with no owner** — §5.7 line 551.
Both gears carry the identical obligation to reject non-deterministic builtins; the only place sharing is
contemplated is the sibling's risk-table mitigation ("The policy engine carries the same exposure, so it is one
audit serving two gears"), never a requirement. Two independently maintained denylists over the same backend
build means the platform's determinism guarantee is only as strong as the staler list — and §12 calls that drift
"the hardest defect in this gear to attribute". *Widen §13's question to cover the denylist as well as the
declaration, and name the evaluation facility as owner of both, with each gear consuming a published per-build list.*

**35. PAP/PDP cohesion is asserted, never argued, and the platform's nearest precedent splits them** — §1.4 line
96, §7.1 line 874. The document asserts the PAP+PDP label, argues only for separate *interfaces*, and then in
`nfr-availability` supplies the evidence that they are two things: the surfaces "fail independently", carry
per-surface targets, and differ eightfold in maintenance allowance. `PERMISSION_GTS_TYPE.md` defers a separate
"AuthZ Management Gear", i.e. the authorization subsystem splits administration from its PDP. The strongest
cohesion argument is present but unconnected — `nfr-decision-latency`'s "any content read that reaches storage on
the decision path breaks the target". *Add a cohesion paragraph making that argument explicitly, and note why the
AuthZ Management Gear precedent does not apply.*

**36. No parameterised policy: every comparable separates rule logic from administrator-set values** — §5.1.
`grep -ni "template\|parameter"` finds no authoring sense of either. The only authored artefact is source text in
a backend language, while the Tenant Policy Administrator is defined as a governance role and §4.2 excludes an
authoring UI. Gatekeeper splits ConstraintTemplate from Constraint; Cedar has policy templates with `?principal`
placeholders whose documented purpose is letting a user instantiate policy "without writing policy logic
themselves"; Azure Policy definitions take parameters. Without it, "no more than 10 instances in eu-west" and
"no more than 20" are two hand-written documents that drift, and the practical authoring population collapses to
platform engineers — making §12's "specified surface with no users" likelier. *Add a parameterised-document
requirement (p2/p3), or add templates to §4.2 with the reason and a §13 question naming who authors content in
first release and in what language.*

**37. Validation is syntax-and-identifier only; comparables type-check content against the resource schema** —
§5.1 line 251. Validation covers syntax, backend resolution, resource-type resolution, vocabularies, the denylist
and limits — nothing about whether the attribute names the content reads exist on anything. The PRD names the
resulting failure twice ("a typo… the failure mode an author is least able to detect") and mitigates it only
after the fact in §12. Cedar validates policies against a schema at authoring time; Kubernetes type-checks CEL
against the resource schema at policy creation. The platform already holds the schemas: `types-registry` is "one
shared authority for type identity, schema validation" and this gear already resolves against it on the authoring
path. *Require attribute names in a target's filters to be checked against a resolvable GTS Type Schema. State
separately whether the operation context is schema'd at all.*

**38. `fr-content-validation` assigns `types-registry` a check it cannot perform** — §5.1 line 253. Validation
must cover "resolution of the declared backend identifier through the types registry **to an instance the
deployment carries**". The registry provides no such attestation — its lifecycle is ACTIVE/DELETED and its
availability model says nothing about runtime presence — and the gear's own glossary defines a backend as an
implementation "behind the evaluation facility's contract" whose exposure is "decided by which compile-time
features the facility enables". A compile-time feature is not a registered GTS instance. *Split into two checks:
identifier resolution through the registry, and a presence check against the facility's enumeration of loaded
backends. Add the facility's obligation to expose that enumeration to §10.*

**39. §1.2's inventory omits the closest instance: `serverless-runtime`'s Tenant Policy Manager** — §1.2 line 59.
`gears/serverless-runtime/docs/DESIGN.md:812` (p2, host-owned) owns "enablement / disablement, quotas …, runtime
allowlist (which plugin GTS types a tenant may invoke), and default limits", justified by "a cross-cutting
concern that must apply uniformly across every plugin" and consulted "at the plugin-dispatch boundary before any
call". That is admission-shaped tenant policy reached by this gear's own argument, one scope level down, and
neither this PRD nor the sibling contains the string "serverless". *Add it to §1.2 and §14, and say whether its
allowlist and default limits are content this gear would own or are permanently host-local.*

**40. `license-resolver` is never engaged, though a §13 question the PRD marks blocking is what that gear
answers** — §11 line 1076, §13 line 1109. §4.2 partitions quota, authorization, authentication and hierarchy by
naming their gears, and leaves licence undrawn; "licence" appears once, inside a restatement of IRM's mapping.
Meanwhile §13 asks "Which component owns the authoritative record of a tenant's subscription plan and entitlement
state" as blocking before first release, and `license-resolver`'s PRD answers it: "the authoritative point-in-time
license check used by other modules to gate access". *Name it in §4.2 on the same terms as the quota exclusion,
and as the candidate owner in the §13 question.*

**41. The credstore comparable is a counter-example on the axis that matters** — §14 line 1121. Cited "for the
local-implementation-plus-management pattern this gear follows", credstore in fact "owns all secret metadata …
and enforces policy; **pluggable backends store only the secret values**" — a gear-local plugin registry, the
shape §5.7 explicitly refuses and §14's very next bullet concedes this gear "departs from". Its delegated part
also carries inverted risk: credstore keeps authorization and hierarchy in the gear and delegates a pure value
store; this gear delegates the semantically hardest piece. *Narrow the citation to what credstore supports — owns
metadata, lifecycle and authorization locally, delegates a narrow substrate — and say the risk profiles differ;
or drop it and state that linking a shared facility rather than hosting a plugin registry is new for this platform.*

**42. "Refusal" is defined as a prohibiting decision and then used throughout for infrastructure failures** — §1.4
line 90 vs §6.1 line 756, §9 lines 1036-1042, §1.3 line 71. The glossary equates refusal with prohibition; §5.5
forbids infrastructure failures from being prohibitions; §6.1 and §9 nonetheless call them refusals. Under the
glossary reading `fr-violations` correctly projects prohibitions only — and the goal and criterion promising
retrieval of "every refusal" become unsatisfiable, which `fr-violations`' own exclusion paragraph already admits.
*Redefine "Refusal" as any outcome in which the operation does not proceed, introduce "Prohibition" for the narrow
sense, and restate the goal and criterion as "every prohibiting decision".*

**43. A decision-query surface is promised by two p1 interfaces, one goal and one criterion, and required by no
requirement** — §4.1 line 192, §7.1 lines 874 and 883. Both interfaces attribute a decision-query capability to
`fr-violations`, which scopes itself entirely to "the prohibiting decision records". Nothing in §5 requires a
query over decision records, yet the p1 goal "Any decision in the retention window can be traced to the subject,
the operation, and the policy version" and the criterion "An auditor can reconstruct any past decision from its
record" both depend on one. *Add `fr-decision-query` on `fr-violations`' terms, or drop "decisions" from all three
places and retier the goal.*

**44. `observe` is a p1 document outcome whose only specified effect is delivered by p2 and p3 requirements** —
§5.5 line 431. At p1 an observe outcome is authorable, contributes nothing by construction, and is recorded by
nothing: the p1 record field set has no per-document outcome list, and the requirement mandating that
non-influencing outcomes be recorded is p3. `fr-responsibility-boundary` (p1) nonetheless requires the gear to
accept it from a backend. That is where an implementer invents a behaviour — most likely dropping it silently,
the one thing the requirement rules out. *Either add observe to the p1 record field set, or move it out of the p1
outcome set and introduce it with `fr-evaluation-phases` at p3, matching `interface-decision-client`'s
"two-valued in practice".*

**45. Both human actors' stated needs are contradicted by the requirements serving them** — §2.1 lines 110 and
124. The tenant administrator needs "a list of current violations they can act on"; `fr-violations` and the
glossary both go out of their way to say the gear provides a refusal history and "not what currently violates
policy", and §12 carries the mismatch as a risk. The policy author needs "the ability to see what a policy will
refuse before it takes effect"; the only two mechanisms are p2 and p3 (finding 18). *Restate both needs to match
what the requirements deliver, and record the standing-breach view in §13 as §12 already says it would be a scope
change.*

**46. The batch latency figure cannot be reconciled with the per-evaluation bound, and the sibling assumes the
opposite arithmetic** — §6.1 line 746. A flat 100 ms for a batch of arbitrary size is incompatible with a 20 ms
per-member bound unless members evaluate concurrently, which no requirement states — and the batch bound is an
open question, so a p1 threshold is stated against an unknown N. The sibling models engine time as *summing*
across members and budgets 10 ms on top. *Restate as a function of batch size matching the sibling's model, and
make the bound a number. DESIGN.md line 813 already assumes the shared-fixed-cost model.*

**47. `fr-evaluation-input`'s rationale argues against the rule it states, and disagrees with the sibling about
who mints the correlation identifier** — §5.6 line 511. The rationale's closing sentence — "a value the caller
can repeat, omit, or choose fails both tests" — disqualifies exactly the design the requirement mandates, since
"when the caller supplies one" permits omission. The sibling resolves it correctly at its own boundary: the
gateway **mints** and forwards, and **MUST NOT** derive it from a value the calling gear controls. Separately,
`fr-decision-records` requires a batch identifier that `fr-evaluation-input` does not accept and
`fr-batch-evaluation` does not source. *Restate: both identifiers are minted by the gateway and supplied on every
request; the gear rejects a request omitting the correlation identifier. Add the batch identifier to the input.*

**48. The shared audit stream both gears assert as a property is p1 for the gateway and p2 here** — §2.2 line 163
vs §10 line 1061. The differing durability models are not a conflict — the sibling names the difference
explicitly and both are sound. The tiering is: at p1 complete for both gears the gateway publishes to the shared
topic and this gear does not, so the one-stream property both documents state as present fact is false for the
entire first release, and §3.1's independent-entitlement claim with it. *Promote the `event-broker` dependency
and `contract-decision-record` to p1, keeping the not-on-the-durability-path distinction; or qualify §2.2 and
§3.1 with the tier.*

**49. §1.1 and the glossary describe the gateway as thin and delegating the check, which the sibling's p1
requirements contradict** — §1.1 line 51, §1.4 line 83. At p1 the gateway evaluates its own policy content
through the same facility and refuses unilaterally without consulting any engine. This PRD's own §5.9 already
knows ("a gateway's own built-in policy"), and §4.2 acknowledges it — only the two places where a reader forms
their model say otherwise. *Restate both to match: a gateway that applies the platform's own built-in policies
itself and delegates every tenant-authored question. Remove "thin".*

**50. `fr-cross-tenant` mandates a refusal cause the gateway's closed cause set cannot carry** — §5.8 line 598.
A cross-tenant refusal is neither a prohibition nor a could-not-run failure, and the gateway's engine-result rule
handles only permission and prohibition while its cause set has no boundary member. So the one refusal this
document calls "the highest-consequence failure in a multi-tenant platform" reaches the enforcing gear
indistinguishable from an ordinary policy refusal. *Raise it against the sibling — its cause list says "at
minimum" — and state here which channel the cause travels on.*

**51. §4.2 excludes staged rollout on the strength of a requirement that does not support it** — §4.2 line 207.
The exclusion cites `fr-deterministic-ordering`, which governs ordering *within* one evaluation and expressly
says the result does not depend on it; it rules out nothing about sampling across requests. "The reproducibility
of decision records" is not a requirement in this document. And of the three substitute mechanisms offered, only
assignment scope is p1. *Justify the exclusion on its own terms — a decision varying by request fraction is not
attributable to a policy version — and mark the substitutes' tiers, or promote `fr-non-enforcing-assignment`.*

**52. Two p1 requirements disagree about whether lifecycle state belongs to a bundle or a bundle version** — §5.2
line 264 vs line 311, §6.1 line 809, §8 line 1020. `fr-lifecycle-states` opens by making the distinction
normative and load-bearing; `fr-deprecation`, the propagation threshold and the use case immediately use the
other model. Deprecating "a bundle" is undefined under `fr-lifecycle-states`. *Say "bundle version" throughout.*

**53. §3.1's "everything an evaluation needs arrives on the request" is falsified by three requirements** — §3.1
line 172. True of *resource state*, which is what the surrounding sentences are about; unqualified as written,
and contradicted by the hierarchy read that `nfr-decision-latency` allocates 20 of its 25 ms to. *Qualify to
resource facts and name the hierarchy read as a Policy Information Point call.*

**54. The p1 goal that every policy change carries a content digest is not met** — §1.3 line 70. Digests exist
only for activated versions; the administration event field set — which the rationale names as the requirement
delivering the goal — has no digest field, and a "policy change" includes every draft modification and
assignment change. *Narrow the goal to activated versions, or add the digest to the audit field set.*

**55. The gateway is the declared prerequisite for every evaluation and appears in no row of §10** — §7.2 line
917. A component without which no evaluation reaches the gear, whose contract shape is unfixed and which §11 and
§12 record as unbuilt, is a p1 dependency; the sibling lists this gear reciprocally. *Add the row.*

**56. Nothing requires content to be loaded before the gear serves decisions** — §6.1 line 746. An instance that
has started but not loaded content, or whose load failed, has an empty applicable set for every evaluation —
which `fr-outcome-combination` makes an ungoverned permission. So the instance permits every gated operation
across every tenant and records each as ordinary policy silence, and the ungoverned counter cannot distinguish
"this tenant authored no policy" from "this instance has none of it loaded". DESIGN.md line 1165 supplies the
missing rule. *Require decision readiness to be gated on the initial content load, and add "content not yet
loaded" to the injected set.*

**57. The propagation window, cache hit rate and latency percentiles do not state the population they are
measured over** — §6.1 lines 799, 809, 819. `grep -in 'instance|replica|node|fleet'` finds no gear-instance sense
of any of them, yet the gear caches decision-contributing state and promises a bounded propagation window. Since
first release composes in-process, the number of gear instances equals the number of consumer hosts, each with
its own cache — so the multi-instance case is the normal one. The 60 s window is satisfiable by one instance while
another serves withdrawn policy indefinitely, and the test still passes. *State the window across every instance
serving the decision surface, state the hit rate per-instance, and name the deployment shape in §3.1.*

**58. Assignment lifecycle holes** — §5.3 line 344, §5.2 line 311, §8 line 1028. An assignment survives its
bundle's last deprecation and resolves to nothing, with no requirement saying whether that is legal or
observable; whether a bundle with no activated version may be assigned is unstated; and §8's withdrawal flow
asserts pre-change impact reporting that no requirement provides. The first is the same silent-disarmament
failure as finding 9. *Require an assignment to name a bundle with at least one activated version, report
affected assignments before a deprecation that would orphan them, and expose the orphan count as a metric.*

**59. `fr-bootstrap` names no mechanism and its acceptance criterion cannot fail** — §5.8 line 616. The
requirement states a path must exist without stating any property of it; the criterion "without an undocumented
path" tests the document rather than the system, and is satisfied by a static credential with unconditional
management authority that is never revoked. The rationale identifies the risk it fails to close. DESIGN.md
invents the scope, the observability and the gauge. *State the path's properties — configured not built in,
limited to management capabilities, observable while in effect, every operation recorded as a marked
administration event, ceasing once activated content exists — and replace the criterion with one that can fail.*

**60. Nothing requires a single evaluation to see one consistent set of active versions** — §6.1 line 799. An
evaluation consuming its ordered applicable set across an activation boundary can read some bundles at the old
version and some at the new; no requirement forbids it, and the resulting record names a version mix that was
never simultaneously in force. DESIGN.md line 379 supplies the guarantee. *Require a single evaluation, and every
member of a batch, to resolve against one consistent set of active versions.*

## LOW findings

| # | Location | Defect | Change |
|---|---|---|---|
| 61 | §14 lines 1116-1117 | Links `./ADR/` and `./features/`; `docs/` contains only `DESIGN.md` and `PRD.md`. The prose concedes they may not exist while the links assert they do. Fails `make lychee`. | Remove or replace with "none yet". Same defect in the sibling. |
| 62 | §5.7 line 551 → nowhere | Four decisions the PRD argues at length — the determinism-enforcement mechanism, the authorization-boundary residuals, the audited-backend consequences, the latency allocation — have no ADR to move to, and DESIGN §1.2's four planned ADRs include none of them. | Create `docs/ADR/` and seed it with the four, plus DESIGN's own list. |
| 63 | §2.2 line 157 vs §7.2 line 893 vs §10 line 1058 | Three counts of what `types-registry` resolves: four kinds, three kinds, one kind. The fourth item ("plugin instances behind them") appears in no requirement or contract. | Align §2.2 and §10 to `contract-gts`. |
| 64 | §5.2 line 322 | The document's only `MAY`, which the template forbids. Descriptive rather than a deferred obligation, so the template's remedy does not apply — but it is trivially restatable. | "…and reporting a change that turns out not to widen in practice is conformant." |
| 65 | §2.2 line 139 | "Every evaluation this gear performs arrives through the gateway" is contradicted by dry-run, content validation, the §1.3 harness and the p1 REST surface that "makes the gear operable without a consuming gear". | Restate as "every **admission** evaluation". |
| 66 | §12 line 1086 | The latency mitigation says "keep hierarchy caching **on** the critical path"; `nfr-hierarchy-latency` says caching keeps hierarchy resolution **off** it. | "keep the hierarchy cache on the decision path so that the remote resolution is not". |
| 67 | §5.5 line 485, §6.1 line 756 | The emergency path has no failure semantics for its own entitlement source, and the threshold's promised separate measurement ("measured separately") appears nowhere. | Require a distinguishable infrastructure refusal when the source is unreachable, and supply the separate threshold. |
| 68 | §7.1 lines 862, 872, 883 | Interface entries carry ClientHub registration scope, error-envelope format, precondition headers, OData/cursor conventions, OpenAPI generation and rate-limiting arrangement — all already stated in DESIGN §3.3. | Reduce Type to "Rust trait, asynchronous"; move the transport conventions to DESIGN. |
| 69 | §6.1 line 769 | `nfr-availability`'s Threshold argues a rejected alternative ("excluding four hours … would concede eleven times the budget") and carries an operational MUST on scheduling, inside a field a verification harness reads. | Move the argument to the Rationale directly below; promote the scheduling MUST to a requirement or a stated operator expectation. |
| 70 | §5.6 line 504, §11 lines 1072-1073, §6.1 line 746 | Decision rationale and a named technology audit sit in requirement rationales and assumptions: two accepted residuals with no §12 row, the audited Rego candidate's parse API and cooperative-timeout mechanism, an allocator-conflict detail, and budget decomposition the "Architecture Allocation" field already delegates. DESIGN.md carries each of them already, so these are duplications that will drift rather than pure misplacements. | Keep the requirement-level why; move the mechanism to DESIGN where it already lives; move the two residuals to §12; extract the decisions as ADRs per finding 62. |

## Industry comparison

Evidence base behind findings 16, 17, 18, 19, 20, 36 and 37. The core model is coherent and mostly matches
practice — the fixed deny-overrides algorithm, the implicit-permit default (correct for admission, unlike
Cedar's implicit deny for authorization), the bundle lifecycle with immutability after activation, and the 60 s
propagation window all have direct precedent. The gaps are on the operational and authoring side.

| System | How it solves the problem | Relevance |
|---|---|---|
| [XACML 3.0](https://docs.oasis-open.org/xacml/3.0/xacml-3.0-core-spec-os-en.html) | Declarable combining algorithm per PolicySet from eight-plus options; four decisions (Permit/Deny/NotApplicable/Indeterminate); Obligations §2.12 must be discharged or the PEP denies, Advice §2.13 "may be safely ignored" | This PRD's fixed rule is XACML's `permit-unless-deny` with NotApplicable folded into a permission cause — coherent but unnamed. The obligation/advice split is the vocabulary IRM's p1 "obligations or warnings" reaches for (finding 16) |
| [AWS Cedar / Verified Permissions](https://docs.cedarpolicy.com/auth/authorization.html) | Combining algorithm fixed and not configurable, order-independent. Schema validation catches "incorrectly typed attribute names" before storage. Policy templates with `?principal`/`?resource` placeholders, staying linked | Validates the hard-wired algorithm — worth saying so. Contradicts on two authoring-time controls this PRD lacks: schema-aware validation (37) and parameterised policy (36) |
| [OPA — bundles](https://www.openpolicyagent.org/docs/management-bundles) | Polling with `min_delay_seconds` 60 default. Signed bundles: `.signatures.json` JWTs binding file list and per-file hashes, verified against an out-of-band key; activation only if verification succeeds | The 60 s propagation window is squarely aligned — cite it as precedent. Bundle signing is the model the self-recorded digest does not match (20) |
| [OPA — decision logs](https://www.openpolicyagent.org/docs/management-decision-logs) / [Styra DAS](https://docs.styra.com/das/observability-and-audit/decision-logs/decision-masking) | Record by default, redact by declaration: mask policy returns JSON Pointers naming fields to erase, and "the erased paths are recorded on the event itself" | The wholesale value suppression here has no opt-in at any tier, and the PRD concedes the cost in three places (19) |
| [OPA Gatekeeper](https://open-policy-agent.github.io/gatekeeper/website/docs/violations/) | `enforcementAction: deny \| dryrun \| warn` per constraint, recommended for testing new constraints. ConstraintTemplate (logic) separated from Constraint (parameters + match) | Counter-evidence to deferring shadow mode to p2/p3 (18); precedent for the warn channel (16) and template/parameter separation (36) |
| [K8s ValidatingAdmissionPolicy](https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/) | `validationActions: [Deny \| Warn \| Audit]` on the **binding** — the direct analogue of this gear's assignment — with `[Warn, Audit]` the documented path before `[Deny]`. CEL type-checked against the resource schema at creation | Closest structural analogue, and it puts the enforcement mode exactly where this PRD puts it, but ships all three in v1 (18) and type-checks (37) |
| [Azure Policy](https://learn.microsoft.com/en-us/azure/governance/policy/concepts/exemption-structure) | Assignment down a hierarchy with `enforcementMode: DoNotEnforce`. First-class exemptions: category Waiver/Mitigated, `expiresOn`, `resourceSelectors`, approval metadata, dedicated `exempt/Action`, and a compliance substate showing the state without the exemption | The nearest match to this PRD's whole shape. Two things it has at v1 and this PRD has at no tier: DoNotEnforce (18) and exemptions (17) |
| [Kyverno](https://kyverno.io/docs/guides/exceptions/) | `PolicyException` as a separate resource, disabled by default, itself governable by policy — motivated by "a team responsible for policy authoring … may not be the same team responsible for submission of resources" | That motivation is a one-for-one description of this platform's ancestor-authors / descendant-submits relationship, which is why the exemption gap is HIGH (17) |
| [OpenID AuthZEN 1.0](https://openid.github.io/authzen/) | Access Evaluation and batch endpoints over subject/action/resource/context, with three declared batch semantics | Scoped to authorization, which this gear deliberately is not, so non-conformance is defensible — but neither the REST surface nor `fr-batch-evaluation` names the standard or states the divergence |

## Checked and clean

- **Requirement IDs**: all 81 `cpt-cf-policy-engine-*` identifiers referenced are defined in the document. The one cross-gear reference, `cpt-cf-admission-control-fr-deferral-relay`, resolves and is described accurately.
- **Template conformance**: all fourteen sections present and in order; the §6.2 exclusion list is more thorough than any peer's and correctly turns operator documentation into a gear-specific MUST rather than an omission.
- **Requirement language**: MUST/MUST NOT throughout, with the single MAY at finding 64.
- **Actors**: every actor is referenced by at least one requirement, and every actor named in a requirement is defined. (The two *needs* mismatches are finding 45, not an actor-coverage defect.)
- **`fr-authorization-boundary`'s mechanism**: withholding entitlements from the evaluation input genuinely makes an authorization rule unwritable rather than merely discouraged. Only the criterion justifying it is wrong (finding 21); the structural boundary itself is the strongest thing in the document.
- **Contract ownership with the gateway**: `contract-admission-engine` correctly cedes the engine contract to `admission-control`, matching the sibling. The residual is which of this gear's two surfaces the gateway calls (finding 3), not who owns what.
- **Decision semantics**: `fr-denial-precedence`, `fr-outcome-combination` and the order-independence claim are mutually consistent and match Cedar's fixed-algorithm position and Azure's most-restrictive combination.
- **Declared Open Questions**: all nine in §13 are genuine and were excluded from the gap findings, except where a question conflicts with settled normative text or where a p1 threshold is stated against an unanswered value (findings 14 and 46).
- **Durability-model divergence from the sibling**: this gear making `event-broker` p2 and non-durability-path while the gateway makes it p1 and the only durability path is **not** a defect — each gear reasons it correctly from whether it owns a database. Only the tiering consequence is a finding (48).

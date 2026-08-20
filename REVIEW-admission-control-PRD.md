# PRD Review — Admission Control

**Document**: `gears/system/admission-control/docs/PRD.md` (671 lines)
**Reviewed against**: `docs/spec-templates/gears-sdlc/PRD/template.md`, `docs/checklists/PRD.md`, and the sibling artifacts written in the same commit (`gears/system/policy-engine/docs/PRD.md`, `gears/infrastructure-resource-manager/docs/PRD.md`)
**Date**: 2026-08-20
**Reviewer**: gear-spec-review

## Verdict by axis

| Axis | Verdict | Findings |
|------|---------|----------|
| PRD-1 Industry alignment | CONCERNS | 4 (1 HIGH structural: no observe mode) |
| PRD-2 Contradictions | FAIL | 12 (1 CRITICAL, 3 HIGH) |
| PRD-3 Logical gaps | FAIL | 11 (2 CRITICAL, 5 HIGH) |
| PRD-4 Gear boundary | CONCERNS | 5 (2 HIGH) |
| PRD-5 Layer discipline | CONCERNS | 6 (1 HIGH) |
| Domain sweep (SEC/REL/DATA/TEST/DOC) | FAIL | 6 (1 CRITICAL, 4 HIGH) |

This is a strong document — denser, better argued and more honest about its own gaps than most PRDs in this
repository, and its boundary against `policy-engine` is drawn from both sides. It is not ready to build from,
for three reasons that are structural rather than editorial. The gear's central property, fail-closed, is
normatively specified only for the engine call, so the built-in evaluation path — the one that runs first, runs
locally, and executes deployment-supplied content — has no refusal obligation and no fault-injection case.
Nothing binds the subject and tenant a caller names to the propagated `SecurityContext`, on a gear whose entire
output is a policy decision about that subject and an audit record attributing it. Record durability is stated
as a 5-second observable window with no behaviour at the boundary, in a gear that owns no database and that
says in two places its records exist nowhere else. All three are closable with text; none is a design dead end.

**Correction, post-review:** finding 4 of the first issue of this report claimed the gate↔engine contract was
claimed by both PRDs with incompatible ownership. That was wrong and has been withdrawn — see finding 4 below.
The two documents agree.

## Fix first

Forty-four live findings are not a work queue. Five of them are roots; most of the rest close with them.

1. **Finding 1** — restate `fr-fail-closed` as a general "cannot complete an admission" rule and make
   `nfr-fail-closed`'s threshold cite that list rather than restate a shorter one. This closes the normative
   half of findings 3, 11 and 14 and the injection-set half of 12, and it is the single largest correctness
   defect in the document.
2. **Finding 2** — bind subject and tenant to the propagated `SecurityContext`. Standalone, one requirement,
   and the gear's whole output is untrustworthy without it.
3. **Finding 3** — decide whether the 5-second durability window is a serving condition or an accepted loss
   bound. Findings 15, 16 and 32 all become tractable once the record's status is settled; today they are
   arguments about data nobody has committed to keeping.
4. **Finding 7** — give built-in policies a declared match scope. Findings 8, 10 and 13 are all consequences of
   gate overhead being O(all policies) with an unbounded input; a matcher plus bounded inputs makes
   `nfr-overhead` falsifiable, which it currently is not.
5. **Finding 20** — create `docs/ADR/`. Findings 34, 35 and 36 are all "move this reasoning somewhere", and
   there is nowhere to move it to. Fixing the two dead §14 links also unblocks `make lychee`.

Findings 6, 18, 19, 21 and 38 are the ones worth a conversation rather than an edit: they ask whether the gear
is scoped right, not whether it is written right.

## Findings

### 1. `fr-fail-closed`'s normative scope is the engine only, so the built-in evaluation failure path is neither required to refuse nor tested

**Axis**: PRD-2 Contradictions · PRD-3 Logical gaps · **Checklist**: `TEST-PRD-001`
**Severity**: CRITICAL
**Location**: §5.4 line 321 · §5.2 line 277 · §6.1 line 433 · §9 line 604

**Issue**: `fr-builtin-evaluation-bounds` delegates an exceeded built-in bound to `fr-fail-closed`, but
`fr-fail-closed` enumerates three conditions and all three are properties of the engine call. The delegation
lands on nothing normative. `nfr-fail-closed`'s threshold then calls its enumeration "the complete set" while
omitting built-in evaluation failure, built-in bound exceedance, and evaluation-facility unavailability — and
§9 line 604 verifies "every enumerated failure condition", so the omission propagates into acceptance unchallenged.

**Evidence**:
> "Where the selected engine is unreachable, exceeds its bound, or returns an error, the system **MUST** refuse the operation with the could-not-run cause." (§5.4, line 321)

> "**MUST** treat an exceeded bound as a failure that refuses per `cpt-cf-admission-control-fr-fail-closed`" (§5.2, line 277)

> "Zero admissions across the complete set of injected failure conditions — engine unreachable, engine timeout, engine error, engine returning an unmappable result, no engine selected, types registry unavailable during rule matching, and internal error." (§6.1, line 433)

The unmappable-result rule that `fr-deferral-relay` is predicated on appears only in a rationale (line 208) and
in this threshold — never as normative text in §5. The sibling's equivalent threshold does enumerate the
evaluation paths: `policy-engine` PRD line 756 lists "evaluation failure, evaluation cost bound exceeded, the
declared backend unavailable, … decision records not reaching durable storage within the window … and internal error".

**Why it matters**: An implementer reading `fr-fail-closed` literally has no obligation to refuse when a
built-in policy times out or errors. A fall-through to the engine would let the engine permit an operation a
non-overridable platform guardrail was supposed to refuse, and no test would catch it, because the condition is
not in the injected set. This is the one property the gear declares testable rather than incidental.

**Proposal**: Restate `fr-fail-closed`'s trigger in general form — the gate refuses with the could-not-run cause
wherever it cannot complete an admission: engine unreachable, exceeding its bound, erroring, or returning a
result the gate cannot map to a permission, a prohibition or a deferral; a built-in policy failing to evaluate
or exceeding either bound of `fr-builtin-evaluation-bounds`; the evaluation facility being unavailable; or an
internal error. Add explicitly that a built-in policy that fails to evaluate **MUST NOT** be treated as yielding
nothing and **MUST NOT** fall through to the engine. Have `nfr-fail-closed`'s threshold cite that list rather
than restate a differing one.

---

### 2. Nothing binds the subject and tenant of an admission to the propagated `SecurityContext`

**Axis**: Domain sweep · **Checklist**: `SEC-PRD-002`
**Severity**: CRITICAL
**Location**: §7.1 lines 485-487 · §11 line 626 · §5.5 line 352

**Why applicable**: `docs/ARCHITECTURE_MANIFEST.md` line 336 assigns authentication to the API Gateway but
states "Gear domain services own authorization: they call PolicyEnforcer" — so this is not inherited. Both
sibling system gears specify it for themselves: `quota-enforcement` PRD §5.13 has a PDP-gated authorization
section with a per-actor permitted/prohibited matrix (line 259 prohibits "submitting operations attributed to a
tenant or user outside the SecurityContext-derived scope"), and `types-registry` PRD lines 484-489 separate
platform-plane from tenant-plane authorization for its own surfaces.

**Issue**: The admission client is a ClientHub trait registered **without scope**, and subject and tenant are
ordinary request parameters on it. No requirement says the gate derives them from the propagated
`SecurityContext`, checks the named tenant against it, or restricts which gears may call the trait at all.

**Evidence**: `grep -niE 'authent|authoriz|authn|authz|securitycontext|rbac|permission' PRD.md` returns only:
§4.2 line 162 (authorization is the question the gate does not answer for others), §5.5 line 386 (credentials
excluded from records), §11 line 626 ("Subjects arrive already authenticated … the gate forwards that context
rather than establishing it"), and §10 line 621 (a `toolkit-security` dependency row). No requirement.

The author applied exactly this discipline one field over and stopped:
> "a correlation identifier the system **MUST** mint per gated operation … and **MUST NOT** derive either from a value the calling gear or its client controls" (§5.5, line 352)

**Why it matters**: The subject and tenant are the whole input on which tenant policy is evaluated and the whole
attribution of the audit record. If a caller names them, a caller can obtain a permit intended for another
subject and write a misattributed audit record — defeating the Security Auditor actor whose stated need (§2.1
line 102) is a trustworthy record of who was gated. The join key is protected from caller control; the identity
the decision is *about* is not.

**Proposal**: Add a p1 requirement in §5, on the model of `quota-enforcement` §5.13: the subject and tenant on
which an admission is evaluated **MUST** be derived from the propagated `SecurityContext`; a request naming a
subject or tenant outside the `SecurityContext`-derived scope **MUST** be refused rather than evaluated; the
admission record **MUST** carry the derived values. Add the corresponding §9 criterion.

---

### 3. Record-sink unavailability has no defined behaviour, while the sibling makes the identical window a serving condition

**Axis**: PRD-3 Logical gaps · PRD-1 Industry alignment · **Checklist**: `REL-PRD-002`
**Severity**: CRITICAL
**Location**: §6.1 line 464 · §2.2 line 128 · §10 line 622

**Issue**: Three p1 statements collide and none resolves. `nfr-record-completeness` demands one record per
decision with no sampling and at most 5 s awaiting durability; §2.2 and §10 both say the topic is the *only*
place a built-in refusal or a could-not-run refusal becomes durable; and no requirement says what the gate does
when the sink is unavailable — refuse, buffer past the window, or discard. The condition is absent from the
`nfr-fail-closed` injection set, so the conformance suite cannot detect which was chosen.

**Evidence**:
> "One record per decision, on both verdicts, with no sampling … at most 5 seconds of records may be awaiting durability at any moment, and that window **MUST** be observable." (§6.1, line 464)

> "Because this gear owns no database, the topic is where its records become durable … a refusal by built-in policy and a could-not-run refusal exist nowhere else." (§2.2, line 128)

`grep -niE 'unavailab|buffer|overflow|backpressure|drop|shutdown|drain'` over the PRD → no match. The gear's own
DESIGN introduces a "bounded in-memory buffer" (DESIGN.md lines 418, 759) that no requirement authorises, and
notes only as a design risk that "buffer pressure appears as refusals".

The sibling took the opposite and explicit position for the same question:
> "Where the system cannot bring a new decision inside that window — because records are not reaching durable storage — it **MUST** stop returning permitting decisions and refuse with an infrastructure cause until it can … The bound is therefore a serving condition and not only a loss allowance: an unrecordable decision is not made." (`policy-engine` PRD, line 789)

Kubernetes exposes the same choice as an operator setting — `--audit-webhook-mode` of `batch`, `blocking`, or
`blocking-strict`, the last failing the request when audit logging fails
(https://kubernetes.io/docs/tasks/debug/debug-cluster/audit/).

**Why it matters**: Goal four of §1.3 is that every admission decision is recorded with its cause. An
event-broker outage longer than five seconds voids it silently and unrecoverably, and the two record classes
lost are the ones the document argues are most valuable. An engine outage and a broker outage are likely
correlated, so the gate starts producing could-not-run refusals at volume exactly when it is least able to
record them. Conversely, if the answer is "refuse", a broker outage becomes a platform-wide stoppage — a
materially different availability contract from the one `nfr-availability` states. An implementer will pick one
by accident.

**Proposal**: Choose and state it. Either follow the sibling and make the window a serving condition — where
records cannot be brought inside it the gate refuses with the could-not-run cause, signalling back-off rather
than immediate retry — or accept bounded loss explicitly, replace "no sampling" with a measured loss bound, and
add it to §12 as a named risk. Either way add "record sink unavailable" and "pending-record buffer full" to the
§6.1 injection set, and add a drain obligation for planned shutdown so that a rolling upgrade does not lose the
window at every host.

---

### 4. WITHDRAWN — the gate↔engine contract is not claimed by both PRDs

**Axis**: PRD-2 Contradictions
**Severity**: ~~CRITICAL~~ — withdrawn on re-verification
**Location**: §7.1 lines 502-507 vs `gears/system/policy-engine/docs/PRD.md` §7.2 lines 914-918

This finding claimed that this PRD and `policy-engine`'s disagreed on who owns the gate↔engine contract, on its
stability, and on its ClientHub registration scope. It was based on comparing this gear's
`cpt-cf-admission-control-interface-engine-plugin` against `cpt-cf-policy-engine-interface-decision-client`.
Those are two different artifacts, and the comparison was wrong.

`policy-engine` §7.2 carries a separate entry, `cpt-cf-policy-engine-contract-admission-engine`, which the
original verification missed because it read only the §7.1 lines the finding cited:

> "The engine-facing trait the gateway defines, implemented by this gear so the gateway can attach it as one of
> its engines. **The gateway owns the contract; this gear conforms to it** without changing its decision
> semantics. … the decision surface it wraps is specified here and is not conditioned on it."

That agrees with this PRD exactly. `interface-decision-client` is `policy-engine`'s own public decision surface,
which the gateway's engine trait wraps; being unscoped and stable is a property of that surface, not of the
plugin contract. Nothing here needs to change.

One residual observation, which belongs to `policy-engine`'s review rather than this one: an *unscoped*
ClientHub registration for the decision client structurally allows any gear to resolve it directly, which both
documents say cannot happen ("management gears reach policy through it, not around it", `policy-engine` §1.1).
That is a defect in the other document, if it is one at all, and is being assessed there.

**Process note**: this false positive survived verification because the check read the cited lines rather than
searching the cited document for the concept. Verification of a cross-document claim has to search the whole
source document, not confirm the quoted excerpt.

---

### 5. The operational REST API has no reader boundary, and the DESIGN reasons the surface out of existence

**Axis**: PRD-3 Logical gaps · Domain sweep · **Checklist**: `SEC-PRD-002`
**Severity**: HIGH
**Location**: §7.1 lines 491-498 · §5.5 lines 371-375

**Issue**: §7.1 declares a p1 "REST API, versioned, served beneath the platform API prefix" reporting the
selected engine, every loaded built-in policy with its identity, per-policy match counts, per-policy refusal
counts, and the could-not-run failure-condition breakdown. No requirement says the endpoint is authenticated,
what permission it needs, which authorization plane it sits on, or that it is not reachable from the tenant
surface.

**Evidence**: The searches in finding 2 return no authorization statement for this surface. §6.2 line 472 says
only "its operational surface is consumed by operators and tooling", and says it as an *accessibility*
exclusion. And the downstream document that would normally close this actively denies the surface exists:

> "Network segmentation, transport security, and CORS are not applicable: the gate opens no listener and exposes no external surface." (`DESIGN.md`, line 729)

which contradicts DESIGN.md's own line 108 (`REST[Operational REST surface]` in the component diagram) and line
145 ("A read-only operational surface … present in both deployment shapes").

**Why it matters**: This is a complete inventory of the platform's non-overridable guardrails plus a live signal
of which are firing and when the gate is blind. An unauthenticated or tenant-reachable version tells an attacker
which operations the platform refuses, provides a probe for enumerating guardrails by watching match counts, and
signals the window during which everything fails closed and operators are under pressure. Because DESIGN
reasons the surface away, no downstream document will supply the control.

**Proposal**: State in `fr-operational-surface` that every operation on the surface requires an authenticated
subject holding a platform-plane permission, is unreachable unauthenticated, and is refused rather than
tenant-filtered for a tenant-scoped subject. Name the permission identifier in §7.1 and reconcile DESIGN.md line 729.

---

### 6. No observe-only mode for built-in policies, while every comparable has one and the sibling argues for it in the same commit

**Axis**: PRD-1 Industry alignment
**Severity**: HIGH
**Location**: §5.2 lines 244-259 — absent from §5 entirely

**Evidence**: `grep -niE 'dry.?run|shadow|audit.only|non.enforcing|observe|warn'` over §4–§9 → no relevant match.
The document instead fixes the opposite: "A built-in policy **MUST** yield either a prohibition or nothing"
(line 248) and "A refusal by a built-in policy **MUST NOT** be overridable by policy content, by a tenant
administrator, or by any request parameter" (line 259), with no management API (line 250).

Every comparable ships a non-blocking mode and documents rolling out through it:

| System | Mechanism | Source |
|---|---|---|
| Kubernetes ValidatingAdmissionPolicy | `validationActions: [Deny\|Warn\|Audit]`; KEP-3488 prescribes enabling Audit/Warn first, Deny last | https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/ |
| Kubernetes webhooks | Deploy with `failurePolicy: Ignore`, monitor, then switch to `Fail` | https://kubernetes.io/docs/concepts/cluster-administration/admission-webhooks-good-practices/ |
| OPA Gatekeeper | `enforcementAction: dryrun` — "only records in audit -- never blocks"; also `warn` | https://open-policy-agent.github.io/gatekeeper/website/docs/violations/ |
| Kyverno | `failureAction: Audit` plus per-namespace `failureActionOverrides` | https://kyverno.io/docs/policy-types/cluster-policy/validate/ |
| GCP Organization Policy | dry-run policies: "violations … are audit logged, but the violating actions aren't denied" | https://docs.cloud.google.com/resource-manager/docs/organization-policy/dry-run-policy |
| Azure Policy | `audit` effect, evaluated after `deny` | https://learn.microsoft.com/en-us/azure/governance/policy/concepts/effect-basics |

And the sibling written in the same commit argues the principle explicitly for tenant content:
> "Governance frequently needs to observe before it enforces. Without a phase whose outcome cannot refuse, every new rule is a production risk." (`policy-engine` PRD, line 402)

**Why it matters**: §13 records that the first built-in policy has not been named. Whatever it is will land on a
fleet where it applies to every tenant, cannot be exempted, cannot be overridden by any request parameter, and
changes only by editing deployment config and restarting. The blast radius of a mis-scoped first built-in is
total and undiscoverable in advance. The gear also cannot answer the question an operator asks before enabling a
guardrail — how many operations in flight would this refuse — and §5.5 line 379 concedes the match count does
not answer it. The §5.4 argument against a bypass ("a documented way to turn enforcement off is a way that gets
left on") addresses the *failure* axis; an observe-mode built-in never converts a failure into an admission.

**Proposal**: Add a p1 `cpt-cf-admission-control-fr-builtin-policy-observe-mode`: each built-in policy declares
`enforce` or `observe` in deployment configuration; an observing policy is evaluated identically but its
prohibition **MUST NOT** refuse and **MUST NOT** stop the engine consultation of `fr-decision-order`; a
would-have-refused outcome is a distinct field on the admission record and a separate count on the operational
surface and in the metrics. Bound it to built-in policies only. If the answer is deliberately no, say so in
§5.2's rationale and name the alternative rollout method — the silence currently reads as an omission.

---

### 7. Built-in policies have no declared match scope, yet four statements presuppose a matcher no requirement defines

**Axis**: PRD-1 Industry alignment · PRD-2 Contradictions
**Severity**: HIGH
**Location**: §5.2 lines 244-252; presupposed at §5.5 line 373, §6.1 lines 423 and 433, §8 line 541

**Evidence**: `fr-builtin-policy-form` specifies where content comes from, what evaluates it, and what it may
yield — nothing about declaring which operations it applies to. Yet the operational surface must report "the
match count for each" (line 373); `nfr-overhead` says "Built-in policy **matching** is inside this figure" (line
423); the injection set names "types registry unavailable during **rule matching**" (line 433); and the use case
says "The gate **matches** the operation against built-in policies" (line 541). Line 281 marks the model shift
explicitly — "Once the gate **evaluates rather than matches**" — which identifies the four as residue.

Every comparable separates a cheap declarative matcher from expensive evaluation and runs it first: Kubernetes
webhooks have `rules` + `namespaceSelector` + `objectSelector` + CEL `matchConditions` evaluated before the
webhook is called; Gatekeeper constraints have a `match` field; Kyverno rules have `match`/`exclude`.

**Why it matters**: Absent a declared matcher, applicability can only live inside the policy content, so content
must be compiled and run to discover it does not apply. Gate overhead becomes O(all built-in policies) rather
than O(those that apply), which is why `nfr-overhead` needs its escape clause "the figure holds only while the
built-in set stays small" — a requirement should be doing that work. It also means adding one built-in for one
resource type tightens the effective per-policy budget for every other operation on the platform. Separately,
`nfr-overhead`'s threshold states the same inclusion twice under two names for the same operation.

**Proposal**: Extend `fr-builtin-policy-form`: each built-in policy **MUST** declare a match scope in deployment
configuration — at minimum the action and the resource types it applies to, resolved through the types registry
— and the system **MUST NOT** evaluate content for an operation the match scope excludes. Have
`fr-configuration-validation` validate it (line 406 already reads as though this mechanism was intended).
Restate `nfr-overhead` in terms of *matched* policies, define "match count" against the new requirement, and
delete the duplicate sentence "Built-in policy matching is inside this figure".

---

### 8. The built-in evaluation bounds have no default, no ceiling, and no aggregate startup check, so a conformant configuration can violate the p1 overhead NFR

**Axis**: PRD-2 Contradictions · PRD-3 Logical gaps
**Severity**: HIGH
**Location**: §5.2 line 277 · §5.6 line 406 · §6.1 line 423

**Issue**: `nfr-overhead` fixes p95 at 5 ms and names built-in evaluation "the dominant term in it", then hedges
that "the figure holds only while the built-in set stays small and its per-policy bound stays well inside the
total". `fr-builtin-evaluation-bounds` requires both bounds be operator-configurable and requires neither a
documented default nor a ceiling — unlike `fr-engine-bound` (line 330), which does require "a documented
default". Startup validation checks only that bounds exist (DESIGN.md line 461: "checks that bounds are present
and positive"), and nothing bounds the *number* of built-in policies, which §11 line 630 states as an assumption
("Built-in policies are few") rather than a requirement.

**Why it matters**: Two conformant configurations break the p1 NFR. A 50 ms total built-in bound satisfies
`fr-builtin-evaluation-bounds` and violates `nfr-overhead`. And 200 policies each comfortably inside the
per-policy bound can collectively exceed the total bound, so every gated operation on the platform is refused
with could-not-run — configuration that validated, discovered at first traffic. The Platform Operator's stated
need (§2.1 line 95) is "configuration validated at startup rather than discovered at first traffic"; the single
mistake that stops the platform is the one startup validation does not catch.

**Proposal**: Require a documented default for both bounds. Have `fr-configuration-validation` reject a
configuration whose per-policy bound multiplied by the loaded policy count exceeds the total bound, and whose
total bound falls outside `nfr-overhead`'s budget. Add a maximum built-in policy count. Add the defaults to
§13's open-question list alongside the engine call bound.

---

### 9. A batch size bound is assumed by three statements and required by none

**Axis**: PRD-3 Logical gaps · PRD-2 Contradictions
**Severity**: HIGH
**Location**: §5.1 lines 213-215 · §8 line 574 · §6.1 line 423

**Evidence**: `fr-batch-admission` requires accepting a batch and combining verdicts, with no cardinality limit.
Three places nonetheless refer to "the configured bound": §8's alternative flow ("Batch exceeds the configured
bound: The gate refuses the request against the limit"), `nfr-overhead` ("A batch of up to the configured batch
bound"), and §13 line 656, which asks what the bound *should be* — a question about a value, not about whether
the mechanism is required. `fr-refusal-cause`'s four enumerated causes have no place for a size-limit refusal.
`grep -n 'MUST bound' PRD.md` → no match.

**Why it matters**: The batch path multiplies both built-in evaluation and engine calls per request on a
component with a 5 ms overhead budget and a fail-closed posture. It is the gate's only self-inflicted
denial-of-service path, the only request shape whose cost scales with caller input, and the acceptance criteria
never test the limit because no requirement creates it. A use-case alternative flow is not normative.

**Proposal**: Add to `fr-batch-admission`: the system **MUST** enforce an operator-configurable bound on batch
size with a documented default, **MUST** refuse a batch exceeding it whole rather than evaluating, truncating or
admitting a subset, and **MUST** carry a distinguishable cause for that refusal. Add that cause to
`fr-refusal-cause` and the bound to `fr-configuration-validation`. Narrow §13's question to the default value.

---

### 10. The batch overhead budget is arithmetically impossible for any batch bound above two

**Axis**: PRD-2 Contradictions
**Severity**: HIGH
**Location**: §6.1 line 423 (both clauses) · §8 line 565

**Evidence**: The same threshold states that built-in evaluation is inside the 5 ms single-admission p95 and is
"the dominant term in it", and that "A batch of up to the configured batch bound adds no more than 10 ms at p95
beyond the sum of its members' engine time". §8 line 565 confirms evaluation is per member: "The gate applies
built-in policies to every member." N members of a dominant-term ~5 ms evaluation cannot sum to ≤10 ms.

Note also that the two clauses use different exclusion bases: the single-admission figure *excludes* engine
time, the batch figure is measured *beyond the sum of* member engine time.

**Why it matters**: The budget will either force the batch bound to two — destroying the purpose of
`fr-batch-admission`, which exists to give a multi-resource plan one answer — or be abandoned at implementation,
taking the credibility of the single-admission figure with it. `nfr-overhead` is p1 with an Architecture
Allocation pointing at DESIGN, so DESIGN will inherit an unsatisfiable target.

**Proposal**: Express the batch clause per member: "A batch adds no more than the single-admission figure per
member at p95, beyond the sum of its members' engine time", or state a fixed per-batch figure derived from the
batch bound once finding 9 is closed. Use one exclusion basis for both clauses.

---

### 11. `fr-engine-backoff` requires refusing operations that `fr-engine-result` requires admitting

**Axis**: PRD-2 Contradictions
**Severity**: HIGH
**Location**: §5.4 line 339 vs §5.3 line 310

**Evidence**:
> "the system **MUST** reduce the rate at which it calls that engine for the stated interval, and **MUST** continue to refuse operations during it rather than admitting them." (§5.4)

> "The system **MUST** admit where the engine returns a permission and **MUST** refuse where it returns a prohibition" (§5.3)

**Issue**: "Reduce the rate" means some calls still reach the engine. For those, the engine may return a
permission — at which point `fr-engine-result` requires admission and `fr-engine-backoff` forbids it. Both are
p1 and unconditional. Read the other way, refusing everything makes the rate reduction pointless, since no
engine answer can change any verdict. Compounding it, `fr-refusal-cause`'s four causes have no back-off entry,
and the injection set has no back-off condition.

**Why it matters**: Back-off is the one path where the gate refuses operations that policy would have permitted.
Getting the scope wrong either amplifies load on already-failing engine storage — the exact harm the requirement
exists to prevent — or converts a transient engine condition into a blanket outage with no cause a caller can
classify.

**Proposal**: Restrict the refusal clause to shed calls: "…**MUST** refuse with the could-not-run cause every
operation whose evaluation is shed by that reduction, and **MUST NOT** admit an operation on the ground that the
engine is backing off." Add the back-off condition to `fr-refusal-cause` and to the §6.1 injection set.

---

### 12. Engine-requested back-off has no ceiling, no floor rate, and no exit condition

**Axis**: PRD-3 Logical gaps
**Severity**: HIGH
**Location**: §5.4 line 339

**Evidence**: The requirement is one sentence. `grep -niE 'ceiling|cap |maximum interval|re-probe|probe|override'`
over the PRD → no match; the same search over DESIGN.md finds back-off only in telemetry.

**Issue**: Two holes. The interval is whatever the engine states, with no ceiling and no operator override — a
defective or hostile selected engine returning a one-hour back-off on every response puts the deployment into
could-not-run refusal of every gated operation for an hour, with no lever to shorten it and no requirement to
re-probe early. And "reduce the rate" names no target and no floor, so no test can fail it: halving the rate and
issuing one call per hour both conform.

**Why it matters**: The gate fails closed, so the back-off window is an outage of every gated operation across
every enforcing gear, and its duration is controlled by the component the gate is supposed to be resilient
against. §12 line 643 commits to "Hold the engine bound tight enough that an outage is detected quickly"; an
unbounded engine-dictated back-off defeats that mitigation entirely.

**Proposal**: Require an operator-configurable floor rate, an operator-configurable ceiling on the honoured
interval with a documented default, re-probing at the floor rate so recovery is detected without waiting out the
interval, and exposure of the remaining interval on the operational surface.

---

### 13. Caller-supplied operation properties are unbounded in count and size, making the overhead threshold unfalsifiable

**Axis**: PRD-3 Logical gaps
**Severity**: HIGH
**Location**: §7.1 line 487 · §6.1 line 423 · §5.5 line 386

**Evidence**: §7.1 accepts "operation properties" with no cardinality or size statement;
`grep -niE 'payload|size|unbounded|limit'` finds no bound. `nfr-overhead` states 5 ms p95 "for a single
admission" with no input-size qualifier. §6.2 line 475 removes scalability as a requirement on the grounds of no
shared mutable state. DESIGN.md line 763 asserts "the gate performs no work proportional to request size", which
is false for built-in evaluation, since §5.2 line 277 states the backend receives the request context.

**Why it matters**: An operation carrying 50,000 properties scales the gate's dominant cost term and, via
`fr-record-confidentiality`'s requirement to record the *name* of every property supplied, scales the audit
record too. The 5 ms figure is stated without a bounded input, so no test can be constructed that the
requirement could fail — any breach is answered with "the request was large". This is also the one input the
enforcing gear relays from further upstream (§11 line 628: the gate "has no schema for it").

**Proposal**: Add to `fr-admission-interface`: the system **MUST** bound the number and total serialized size of
caller-supplied operation properties, **MUST** make both operator-configurable with documented defaults, and
**MUST** refuse a request exceeding either with a distinguishable cause rather than truncating. Qualify the
`nfr-overhead` threshold: "measured with an operation context at the configured property bound".

---

### 14. The containment boundary — capability restriction and determinism denylist — has no threshold, verification method, or acceptance criterion

**Axis**: Domain sweep · **Checklist**: `TEST-PRD-001`
**Severity**: HIGH
**Location**: §5.2 lines 277-279 · §9 lines 599-611

**Issue**: `fr-builtin-evaluation-bounds` bundles four separately-testable obligations — a capability
restriction, a per-policy bound, a total bound, and a startup denylist — into one paragraph with no threshold
figures, no **Verification Method** field, and no representation in §9. `nfr-fail-closed` and
`nfr-non-modification` both got explicit thresholds and the former a named verification method; this did not.
None of §9's thirteen criteria mention a capability, a builtin, a denylist, or an evaluation bound.

**Why it matters**: This is the boundary between deployment configuration and code execution — the PRD's own
rationale says without it "configuration would be a remote-execution surface". It depends on a facility §10
says does not exist, that §11 says "declares no sandbox posture of its own", and whose builtins "can be neither
removed nor shadowed". §12 line 640 accepts the residual risk with the mitigation "enumerate the backend's
registered builtins in a test rather than trusting a hand-maintained list" — a test the requirement never
mandates. The §5 preamble's blanket 90% coverage does not substitute: a capability restriction is verified by
proving an absence, which coverage cannot express.

**Proposal**: Split the requirement into its containment and cost obligations and give each a Verification
Method. For containment, state that verification enumerates the backend build's registered builtins and asserts
every capability-bearing and non-deterministic one is absent or denylisted, and that content referencing one is
rejected at startup. Add §9 criteria for both, and one for `fr-engine-backoff`, which also has none.

---

### 15. No retention, deletion, or classification requirement for the personal data in admission records; the deferral target does not exist

**Axis**: Domain sweep · **Checklist**: `SEC-PRD-003`, `COMPL-PRD-003`
**Severity**: HIGH
**Location**: §6.2 line 473 · §2.2 line 128

**Evidence**:
> "personal data is confined to the subject identifiers in its records and follows the platform's retention model." (§6.2, line 473)

`grep -rn "retention model" docs/ guidelines/ gears/` returns exactly two hits — this sentence, and an unrelated
soft-delete setting in a plugin PRD. There is no platform retention model. The sibling from the same commit
carries `cpt-cf-policy-engine-fr-record-retention` (p1, line 680: "MUST apply a configurable retention period to
decision records, MUST remove records beyond it, and MUST keep the retention period and the current record
volume observable") and disposes of data-subject rights by naming it.

**Why it matters**: Every admission record carries the subject and tenant, on both verdicts, with no sampling,
at the rate of every gated operation on the platform. Both failure directions are unconstrained. Too short: a
compaction setting on a shared topic silently destroys the only copy of built-in and could-not-run refusals. Too
long: unbounded accumulation of subject identifiers at request rate. Because the deferral target does not exist,
nothing downstream inherits the obligation.

**Proposal**: Either add a p1 requirement mirroring `cpt-cf-policy-engine-fr-record-retention`, stating a
configurable and observable retention period with a default and a minimum consistent with the Security Auditor's
need and how erasure interacts with the shared topic — or, if retention genuinely belongs to `event-broker`,
replace "the platform's retention model" with a link to the requirement that owns it and state the retention
floor this gear needs as a requirement *on* `event-broker` in §10.

---

### 16. Ownership and the consumer boundary of the cross-tenant admission record stream are undefined

**Axis**: Domain sweep · **Checklist**: `DATA-PRD-001`
**Severity**: HIGH
**Location**: §7.2 lines 521-525 · §2.2 line 128 · §5.5 line 388

**Evidence**: §7.2 says only "**Direction**: provided to downstream consumers" — never naming or bounding them.
§5.5's rationale asserts "Admission records are widely readable by design" without turning it into a bounded
statement. The sibling states ownership explicitly (`policy-engine` PRD line 173: "Decision records belong to
the platform's audit function; the gear produces them and applies retention, and consumers of the record stream
are entitled to them independently of this gear"). This PRD has no counterpart.

**Why it matters**: One stream aggregates every tenant's subjects, actions and resource identifiers onto a
platform-scoped topic. Its exposure is decided by whoever configures subscriptions rather than by any
requirement, and a tenant-scoped consumer that subscribes obtains a cross-tenant view of the estate.
`fr-record-confidentiality` correctly keeps credentials and property values out — which shows the author
reasoned about record *contents* — but the record still carries identities, so stream confidentiality depends on
a reader boundary nothing establishes.

**Proposal**: Name the owner (the platform audit function), state that this gear is producer and not custodian,
and add to `contract-admission-record` that the topic is platform-scoped and **MUST NOT** be exposed to
tenant-scoped consumers. Add the §9 criterion.

---

### 17. Goal 1 and the first acceptance criterion assert an outcome §11, §12 and §13 all record as unagreed

**Axis**: PRD-2 Contradictions · PRD-4 Gear boundary
**Severity**: HIGH
**Location**: §1.3 line 63 and §9 line 599 vs §11 line 632, §12 line 641, §13 line 654

**Evidence**: §9's first criterion — "An enforcing gear reaches the policy question through this interface on
every operation it declares as gated" — can only be satisfied by another gear's behaviour, which §11 records as
"a change to that gear's integration which nobody has yet agreed", §13 lists as an open question due before
implementation, and §12 carries as a live risk. Verified: IRM's PRD still routes admission through a "Policy
Decision Service" mapped to `authz-resolver`, and its own open-questions table carries the mirror question with
an empty Answer column and a 2026-10-31 target.

**Why it matters**: An acceptance criterion is a release gate. Making p1 completion contingent on a decision
another team has not taken means the gate either ships "incomplete" by its own definition or the criterion is
quietly reinterpreted at release.

**Proposal**: Restate the criterion as a property of the interface rather than of adoption — "The interface
admits every operation shape an enforcing gear declares as gated, verified against Infrastructure Resource
Manager's admission requirements" — and move the adoption claim into §1.3 as a target conditioned on the §13
question rather than on p1 completion.

---

### 18. The gear's self-nominated load-bearing justification rests on a capability with zero instances and no implementation path

**Axis**: PRD-4 Gear boundary
**Severity**: HIGH
**Location**: §5.2 line 261 · §13 line 653 · §10 line 618 · §12 line 639

**Evidence**:
> "This is also the requirement that makes the gear's existence load-bearing rather than a convenience: without it, everything here could live in the engine." (§5.2, line 261)

Against which: no built-in policy has been named (§13 line 653, which also concedes the independence requirement
is thereby "unfalsifiable in practice"); the evaluation facility that would run them "Does not exist yet, in any
form, anywhere in the repository" (§10 line 618) with the mitigation "Nothing this gear can mitigate" (§12).
The other leg — the single interception point — has zero committed callers (finding 17).

**Why it matters**: Applying the PRD's own test, a reviewer cannot conclude the gear is load-bearing today. That
matters less as a philosophical point than as a sequencing one: the p1 requirements attached to built-in
policies (bounds, denylist, determinism auditing, content validation) are the expensive half of the gear, and
they are committed ahead of anything that proves they are needed. §12 line 641 names the outcome itself.

**Proposal**: Either name at least one concrete built-in policy in §5.2 so the independence requirement has an
observable instance, or restate the claim. Engine selection, the plugin contract, fail-closed semantics and the
admission record all cannot live inside the replaceable component being selected, so the interception point is
load-bearing on its own and line 261 understates the case. If no built-in can be named before implementation,
re-tier the four built-in requirements to p2 behind the evaluation facility.

---

### 19. IRM's p1 warnings clause is served by neither gear in the pair

**Axis**: PRD-4 Gear boundary
**Severity**: HIGH
**Location**: §4.2 line 163 · §5.1 line 177

**Evidence**: IRM `cpt-cf-infrastructure-resource-manager-fr-policy-gating` (p1, line 1134):
> "A decision **MAY** be advisory: an allow verdict **MAY** carry obligations **or warnings** from the decision service, and the system **MUST** deliver them to the caller unaltered alongside the operation result."

This PRD relays obligations only (§5.1 line 177) and reassigns the warning half to `quota-enforcement` in an
out-of-scope bullet (§4.2 line 163: "threshold-crossing warnings are events it emits to sinks registered with
it"). `grep -ci warning gears/system/policy-engine/docs/PRD.md` → **0**; its decision client returns permit or
prohibit with cause, reason and obligations, and nothing else.

**Why it matters**: IRM attaches warnings to the *policy* decision service's allow verdict, in the same
requirement as obligations and separately from its own quota gating. The §4.2 sentence is not wrong about quota,
but it answers a different requirement than the one IRM wrote, and the result is that a policy-originated
warning on a permission is carried by neither gear. Silently narrowing an upstream p1 requirement inside an
out-of-scope bullet is the failure a boundary section exists to prevent; it surfaces at integration.

**Proposal**: In §4.2, quote IRM's wording and split it explicitly. Either require the verdict to relay
engine-supplied advisory warnings alongside obligations — a small addition to both interfaces, cheaper now than
after either stabilises, and the same argument already made for the deferral variant — or record that IRM's
warning clause is deliberately unserved and add it to §13 with the IRM owner.

---

### 20. `fr-deferral-relay`'s rationale is a decision record, and there is no ADR directory to move it to

**Axis**: PRD-5 Layer discipline · **Checklist**: `ARCH-PRD-NO-002`
**Severity**: HIGH
**Location**: §5.1 line 208 · §14 lines 664-665

**Evidence**: The first two sentences of the rationale are legitimate (the engine is three-valued, the gate is
two-valued, and the naive composition turns "awaiting approval" into "retry the outage"). From "Carrying the
deferral in the plugin contract from the first version…" onward it justifies reserving an unpopulated variant in
v1 over adding it later, weighs that against the obligation-collection precedent, and defends the chosen interim
mapping ("the honest interim") against the two rejected mappings the requirement text enumerates.

The house standard is different, and the reference gear demonstrates it — `types-registry` PRD line 292 states
the requirement rationale and then defers: "ADR-0004 records the alternatives, the concurrency argument behind
contiguity, and the deployment configuration this requirement deliberately does not have." That gear carries 15
ADRs; `quota-enforcement` carries six. This gear carries none, and §14 links `./ADR/` and `./features/`, neither
of which exists — which will also fail the `make lychee` CI check.

**Why it matters**: Reasoning that lives only in a requirement's rationale cannot be superseded. When someone
later proposes adding the deferral variant in v2 or remapping the interim cause, there is no record to revisit
and no decision status to change — they must reopen a MUST statement's prose. The same pattern recurs at §11
line 631 (an audit of a candidate evaluation backend and the mechanism chosen from it), at §5.5 lines 364 and
379 and §5.1 line 228 (the statelessness decision argued three times against named alternatives), and at §9 line
608 (three declined mechanisms — reservation, lease, decision token — recorded inside an acceptance checkbox).
DESIGN.md line 90 lists five planned ADRs and none of them is any of these four.

**Proposal**: Create `gears/system/admission-control/docs/ADR/` and seed it with the five decisions DESIGN.md
names plus these four: the deferral contract shape and interim mapping; the evaluation-backend determinism
posture and denylist; no verdict pinning between preview and apply; and the gate holding no persistent state.
Reduce the corresponding rationale blocks to the requirement-level "why" plus a citation, following
`types-registry`. Fix or remove the two dead links in §14 either way.

---

### 21. The engine's emergency-access path is invisible to the gate that fronts it

**Axis**: PRD-1 Industry alignment
**Severity**: HIGH
**Location**: §7.1 line 487 · §5.5 lines 350-360 · §5.5 line 386

**Evidence**: `policy-engine` `cpt-cf-policy-engine-fr-emergency-access` (**p3**, line 485) requires: "the
request explicitly asserts emergency access … Every decision reached this way **MUST** be marked as such in its
decision record and **MUST** increment a distinct metric." This PRD never mentions the path. Its interface
description names "subject, action, resource type and optional identifier, tenant context, and operation
properties"; §11 line 628 treats all caller-supplied properties as opaque and untrusted;
`fr-record-confidentiality` strips property values, so the assertion would appear in the admission record by
name only; and neither `fr-admission-records` nor `fr-metrics` has an emergency field or counter.

Separately, no exception mechanism exists for the gate's *own* guardrails. Every comparable offers one: AWS SCPs
exempt the management account outright; Azure Policy exemptions are first-class objects with a category, an
`expiresOn`, requester/approver metadata and a dedicated `exempt/Action` permission; Gatekeeper has
`--exempt-namespace`, designed so namespace-edit permission is not bypass permission; Kubernetes recommends
excluding `kube-system` and the webhook's own namespace to avoid deadlock.

**Why it matters**: The gate is declared the single interface through which enforcing gears reach policy, so the
emergency assertion must travel through it — and the gate in front cannot distinguish an emergency admission
from an ordinary permit, while the engine behind increments a distinct metric for it. Separately, §12 line 643
identifies "every engine outage a platform-wide stoppage" and mitigates only detection speed, not recovery:
during an engine outage every gated operation is refused including those needed to end the outage, and the
platform's one designed escape hatch lives behind the component that is down. This is not an argument to weaken
fail-closed — that default is industry-standard (Envoy's `failure_mode_allow` defaults to false; Kubernetes
`failurePolicy` defaults to `Fail`). It is that the document decided the *failure* axis and never asked the
*scope* axis.

**Proposal**: State in both interfaces how the engine's emergency assertion reaches the engine through the gate;
add an emergency marker to `fr-admission-records` and a distinct counter to `fr-metrics`, mirroring what the
engine requires of itself; and state explicitly whether a built-in prohibition is overridable by an emergency
assertion (§5.2 line 259 currently says no by implication via "any request parameter") so the asymmetry is a
decision rather than an accident. Separately add either an §11 assumption naming the recovery path — that no
operation required to restore the engine or correct built-in configuration traverses a gated gear — or a §13
question owned by platform architecture.

---

### 22. `fr-record-confidentiality` forbids recording exactly the fields `fr-admission-records` mandates

**Axis**: PRD-2 Contradictions
**Severity**: MEDIUM
**Location**: §5.5 line 386 vs §5.5 lines 354-355 and §7.1 line 487

Per §7.1, subject, action, resource type, resource identifier and tenant context are all caller-supplied.
`fr-record-confidentiality` bans "the values of caller-supplied operation context"; `fr-admission-records`
mandates them. Both p1. The narrower intent is visible in the same requirement's own rationale ("the gate has no
schema for caller-supplied context", true only of the free-form properties bag) and in §9 line 611's phrase "a
caller-supplied property value". Since `fr-admission-records` is declared the normative owner of the field set,
a conflicting prohibition in a sibling requirement makes the audit record ambiguous at the point where
auditability is the gear's fourth goal. **Proposal**: replace "the values of caller-supplied operation context"
with "the values of the caller-supplied operation properties", and add that the named fields of
`fr-admission-records` are recorded by value while operation properties are recorded by name only.

---

### 23. Four statements still describe the verdict and the engine result as exactly two-valued

**Axis**: PRD-2 Contradictions
**Severity**: MEDIUM
**Location**: §2.2 lines 110 and 116 · §4.1 line 147 · §6.1 lines 462 and 464

§1.4, §5.1 and §7.1 carry the three-valued story correctly. Four other places were not updated. The most
consequential is §2.2's Policy Engine actor — "returns a permission or a prohibition" — which directly
contradicts the p1 requirement that the plugin contract carry three values from its first version, and which is
what an implementer reads before §7.1. `nfr-record-completeness`'s "on both verdicts", stated twice, becomes
wrong the moment `fr-deferral-verdict` ships, reopening a p1 NFR. **Proposal**: §2.2 Policy Engine → "returns a
permission with its cause, a prohibition with a reason, or a deferral"; §2.2 Enforcing Gear → "receives a verdict
per §1.4"; §4.1 → "one operation or batch in, one verdict out"; replace "on both verdicts" with "on every
verdict, whatever its value" in both occurrences and in §9 line 611.

---

### 24. `nfr-non-modification`'s threshold measures only the admitted path

**Axis**: PRD-2 Contradictions
**Severity**: MEDIUM
**Location**: §6.1 line 444 vs §9 line 600 and §5.1 line 177

The threshold reads "For every **admitted** operation across the conformance suite…", while §9 states the
property "on both verdicts" and `fr-admission-interface` states it unconditionally. A gate that mutated a
request on the refusal path would pass the NFR and fail the acceptance criterion — and the refusal path is where
an implementation is most tempted to annotate the request with a reason. The NFR exists specifically to make
non-modification "a measurable property rather than an omission"; measuring half the paths defeats that.
**Proposal**: replace "For every admitted operation" with "For every operation, on every verdict,".

---

### 25. Goal 5 is milestoned at "p2 complete", and no p2 requirement exists

**Axis**: PRD-2 Contradictions
**Severity**: MEDIUM
**Location**: §1.3 line 67 vs line 59

Counted across §5 and §6: **25 `p1`, 4 `p3`, zero `p2`**. §1.3 line 59 defines the milestone as "every `p1`
requirement in Sections 5 and 6", so "p2 complete" is either vacuously true or undefined. The only p2 item in
the document is `usecase-substitute-engine` in §8, which line 59 explicitly excludes. The requirements that
actually deliver Goal 5 — `fr-builtin-policy-independence`, `fr-engine-selection`,
`interface-admission-client` — are all p1. This is the goal that justifies separation from the policy engine,
and its delivery milestone points at an empty tier. **Proposal**: set Goal 5's target to "p1 complete", or, if
substitution is genuinely deferred, add an explicit p2 substitution-conformance requirement.

---

### 26. §13's governed/ungoverned question presupposes an ordering two p1 requirements and the glossary forbid

**Axis**: PRD-2 Contradictions
**Severity**: MEDIUM
**Location**: §13 line 658 vs §5.1 line 186, §5.3 line 310, §1.4 line 80

A built-in policy cannot see the permission cause, because `fr-decision-order` requires built-ins to run before
the engine is consulted at all. Answering the question "yes" would reverse that order — whose rationale argues
the current one is "the only ordering that saves work" — overturn `fr-engine-result`'s explicit **MUST NOT**
("MUST NOT treat a permission whose cause is ungoverned differently"), and contradict the glossary ("The gate
records it and does not branch on it"). The question is posed as open while three normative statements have
closed it. **Proposal**: reframe as "should the gate gain an ungoverned-refusal mode as a distinct requirement,
applied after the engine returns", making the ordering consequence explicit — or record it as already decided
against and delete it.

---

### 27. §4.1 contradicts itself on batch and omits three capabilities §5 and §7 require

**Axis**: PRD-2 Contradictions
**Severity**: MEDIUM
**Location**: §4.1 lines 147-154

Bullet 1 says "one operation in, admitted or refused out"; bullet 5 puts batch admission in scope. Separately,
three things §5 and §7 require have no in-scope bullet: the read-only operational surface and its p1 REST API;
the deferral relay (§4.2 excludes only the approval *workflow*); and the remote decision surface, a whole second
deployment shape that carries a p3 requirement and conditions half of `nfr-availability`'s threshold while
appearing nowhere in §4. §4 is what a reviewer uses to decide whether a requirement belongs to this gear.
**Proposal**: reword bullet 1 to "one operation or one batch in, one verdict out" and add the three missing
bullets.

---

### 28. §6.2 excludes scalability on a premise two requirements falsify

**Axis**: PRD-2 Contradictions
**Severity**: MEDIUM
**Location**: §6.2 line 475 vs §5.4 line 339 and §5.5 lines 373-375

The exclusion reads "the gate holds **no shared mutable state on the decision path**". A rate limiter shared
across all callers of one engine is shared mutable state read and written on that path, by construction; so are
the per-policy match counts and the windowed refusal counts, both updated on every gated operation. The stated
premise is false, so the conclusion drawn from it is unsupported — and these are exactly the structures that
contend under concurrency, in front of every management operation on the estate. The sibling states a
concurrency requirement for this reason (1,000 concurrent in-flight evaluations). **Proposal**: narrow the
premise to name the two exceptions as contention-bounded by construction, or drop the exclusion and add a
concurrency threshold to `nfr-overhead`.

---

### 29. A per-request types-registry lookup is assumed in three places, required by none, and unbudgeted

**Axis**: PRD-2 Contradictions · PRD-3 Logical gaps
**Severity**: MEDIUM
**Location**: §2.2 line 122 · §7.2 line 516 · §6.1 line 433 vs §5.6 line 406

§2.2 and §7.2 both say the registry resolves the resource types "built-in policies **and requests**" name, and
the injection set names "types registry unavailable **during rule matching**". No requirement establishes a
runtime registry call: `fr-configuration-validation` resolves types at startup, `fr-engine-selection` resolves
the engine at selection, and DESIGN.md line 190 states "After startup, the built-in policy set and the engine
handle are immutable for the life of the process". So either the lookup exists and is unrequired, unbudgeted
against the 5 ms figure, and unhandled — or it does not and four statements are wrong. Meanwhile the real
condition, the registry being unreachable *during startup validation*, is undefined: line 406 reads as
fail-to-start, which in the in-process shape evicts the gears the gate shares a host with — the outcome
`fr-absent-engine` argues against for the engine. **Proposal**: decide and state it once; if resolution is
startup-only, drop "and requests" from §2.2 and §7.2, replace the injection condition with "types registry
unavailable during startup validation", and require a bounded retry before failing to start.

---

### 30. `fr-absent-engine` forbids unreadiness unconditionally while §3.1 says unreadiness is correct in the remote shape

**Axis**: PRD-2 Contradictions
**Severity**: MEDIUM
**Location**: §5.3 line 299 vs §3.1 line 138 and §5.1 line 235

Three-way conflict: `fr-absent-engine`'s **MUST NOT** is unqualified; §3.1 says "unreadiness becomes a safe
signal" once the gate runs remotely; and `fr-remote-decision-surface` requires "the same failure behaviour" as
the in-process surface, which would force the remote shape back to the prohibition. §3.1 line 137's blanket
disclaimer does not resolve it, because the p3 requirement explicitly re-imports the in-process behaviour.
Readiness is what an orchestrator acts on: wrong in-process it evicts unrelated serving surfaces; wrong remotely
it leaves a gate that refuses everything sitting in rotation. **Proposal**: scope the prohibition to the
in-process shape and narrow `fr-remote-decision-surface` to "the same verdicts and the same causes".

---

### 31. Whether a running deployment's rule set is immutable is never stated

**Axis**: PRD-3 Logical gaps
**Severity**: MEDIUM
**Location**: §3.1 line 140 · §5.2 line 250 · §5.6 line 406

`grep -niE 'reload|SIGHUP|hot|immutab'` → no match; "restart" appears only inside a use-case step. §5.2 forbids
only an *API*; a file edit picked up by a platform-wide reload is not an API. Nothing says whether the gate
re-reads configuration, whether a reloaded set is re-validated against the startup rules, or what happens to
in-flight admissions. DESIGN.md line 190 does state immutability — as a design principle with no requirement
behind it. The whole precedence argument rests on built-in policies being deployment-versioned and
non-administrable at runtime; if configuration were reloadable without the startup validation path, a mistyped
resource type would produce a guardrail that silently never fires. **Proposal**: state in
`fr-builtin-policy-form` that the effective configuration is fixed for the lifetime of a process, that a
platform reload signal is ignored and reported as such on the operational surface, and that an in-flight
admission is decided against the configuration in force when it was submitted.

---

### 32. Admission records name the engine but not the built-in rule set that decided

**Axis**: PRD-3 Logical gaps
**Severity**: MEDIUM
**Location**: §5.5 lines 350-362

The field set carries "the identity of the selected engine", justified at line 362 because "a deployment can
substitute it, and a record that does not say which engine decided cannot be compared across a substitution".
Exactly that reasoning applies to built-in policies, which are also substitutable — by a configuration change —
and there is no field for the version or digest of the loaded set, nor for the deciding instance. During a
rolling configuration rollout the same operation is refused on some hosts and admitted on others, and the
records are indistinguishable. **Proposal**: add a stable identity of the effective built-in policy set (a
version or content digest) and the deciding gate instance, and expose the same identity on the operational
surface so an operator can confirm a rollout is uniform.

---

### 33. Nothing states whether one tenant's traffic can affect another tenant's decisions, latency, or records

**Axis**: PRD-3 Logical gaps
**Severity**: MEDIUM
**Location**: §11 line 626 · §6.2 line 475 · §5.4 line 339

`grep -niE 'per.tenant|isolat|fair|noisy|concurren'` finds only §3.1's unrelated use of "isolating". Tenant A's
burst trips the engine's back-off signal, and under `fr-engine-backoff` the gate then refuses every operation
for the interval, including tenant B's. The same shared-resource question applies to the record buffer and to
concurrent in-flight engine calls. `docs/arch/authorization/TENANT_MODEL.md` states "Isolation by default" as a
platform principle; a system gear that exempts itself should say so deliberately rather than by omission, since
the enforcing gear cannot supply a bulkhead — the gate owns the engine handle and the buffer. **Proposal**: add
an assumption or NFR stating plainly that the gate's behaviour is not tenant-partitioned and that deployments
needing isolation must partition, or require per-tenant bounds on in-flight admissions and buffer share.

---

### 34. Two acceptance criteria carry design commentary and record declined mechanisms

**Axis**: PRD-5 Layer discipline · **Checklist**: `BIZ-PRD-NO-002`
**Severity**: MEDIUM
**Location**: §9 lines 608-609

Line 608's testable content ends at "given unchanged policy between the two"; what follows explains why no
stronger guarantee exists and names three declined mechanisms (reservation, lease, decision token) — a
rejected-alternatives record inside a checkbox that cannot be ticked. Line 609's testable content ends at "is
refused"; the final sentence is caller guidance. Both siblings write §9 as testable single-sentence criteria.
**Proposal**: split both; move the caller guidance to §7.1 as a conformance expectation; record the declined
preview/apply mechanisms as an ADR (see finding 20).

---

### 35. `fr-builtin-evaluation-bounds` specifies the backend call's argument set and cooperative-cancellation accounting

**Axis**: PRD-5 Layer discipline · **Checklist**: `ARCH-PRD-NO-001`
**Severity**: MEDIUM
**Location**: §5.2 lines 277-279

Two crossings inside an otherwise sound requirement: the exact argument list handed to the backend is an
internal call signature (the requirement is that no capability is passed and that the gate supplies the
timestamp), and the cooperative-versus-preemptive cancellation clause describes how a particular backend
implements timeouts and then adjusts an NFR's measurement basis accordingly. A requirement that names the
backend's cancellation model is invalidated by a backend change even though the intent is unchanged, and an
implementer must know which backend is in use before they can tell whether the clause applies. **Proposal**:
move both to DESIGN §3.2 and § NFR Allocation; retain in the PRD the capability restriction, the gate-supplied
timestamp, the two configurable bounds, the refusal on exceedance, and the determinism constraint. The same
applies at a lower level to §5.6's "Both checks run against the parsed form of the content rather than its
source text" — if the intent is evasion-resistance, state that instead.

---

### 36. `nfr-overhead`'s threshold carries budget decomposition the requirement already delegates to DESIGN

**Axis**: PRD-5 Layer discipline · **Checklist**: `BIZ-PRD-NO-002`
**Severity**: MEDIUM
**Location**: §6.1 line 423

The measurable part belongs here: 5 ms p95, 10 ms p99, exclusions, batch figure. The sentence beginning
"Evaluation of built-in policies is inside this figure and is the dominant term in it…" is an allocation of the
budget across internal terms and a statement of the conditions under which it holds — and the requirement
already carries "**Architecture Allocation**: See DESIGN.md § NFR Allocation", which is where that belongs.
Decomposition changes whenever the internal breakdown changes; the externally promised figure does not.
**Proposal**: move the decomposition to DESIGN § NFR Allocation.

---

### 37. Wall-clock bounding of built-in evaluation reintroduces the non-determinism the same requirement forbids

**Axis**: PRD-1 Industry alignment
**Severity**: MEDIUM
**Location**: §5.2 lines 277-279

Line 277 bounds evaluation by wall-clock time; line 279 forbids non-deterministic builtins because "a built-in
policy whose verdict varies between two identical requests is a platform guardrail nobody can reproduce, review,
or reason about". A wall-clock bound makes the verdict a function of host load: the same policy against the same
request admits on an idle node and refuses with could-not-run on a loaded one. Kubernetes faced this exact
choice and chose a deterministic cost budget — KEP-3488: "unlike webhooks, runtime cost is deterministic (it is
purely a function of the input data and the CEL expression and is independent of underlying hardware or system
load)" (https://github.com/kubernetes/enhancements/blob/master/keps/sig-api-machinery/3488-cel-admission-control/README.md).
§11 line 631 already asserts the audited backend accepts "an externally imposed per-policy cost bound", so the
option appears available and was not taken without comment. **Proposal**: make the primary bound a deterministic
cost bound with the wall-clock bound retained as a backstop; add a §9 criterion that an identical request under
load and at rest produces the same built-in verdict.

---

### 38. The boundary argument engages a weaker counter-argument than `policy-engine` actually offers

**Axis**: PRD-4 Gear boundary
**Severity**: MEDIUM
**Location**: §5.2 line 270

The claim is that built-in independence "could not be obtained by assigning a bundle at the root tenant of the
policy engine". But `policy-engine`'s PRD already specifies non-withdrawable ancestor guardrails, and says so in
as many words:
> "a prohibition anywhere in the chain refuses regardless of where it sits. That makes ancestor guardrails a consequence of the combination rule rather than a separate mechanism … a descendant could remove a constraint its ancestor is accountable for." (`policy-engine` PRD, line 355)

This PRD names neither `cpt-cf-policy-engine-fr-nearest-tenant` nor `fr-inheritance-barriers`. Separately, the
no-engine half of the payoff is vacuous: `fr-absent-engine` already refuses everything when no engine is
selected, so a built-in there changes the refusal *cause*, not the outcome. That leaves survival across
substitution as the sole real payoff — of an engine with no alternative implementation anywhere in the
repository. The strongest counters (make the engine non-replaceable; carry built-in content as engine-agnostic
content re-published into whichever engine is selected) are never stated. **Proposal**: name the two
`policy-engine` requirements, concede the tenant-non-withdrawable guardrail already exists there, and narrow the
claim to what is genuinely unobtainable — survival across substitution, plus independence from the engine's
availability and content lifecycle. Requalify or drop "or that runs before any engine is configured".

---

### 39. Built-in policy evaluation is a second responsibility that duplicates the engine's evaluator surface

**Axis**: PRD-4 Gear boundary
**Severity**: MEDIUM
**Location**: §5.2 lines 246, 277-279 · §12 lines 638 and 640

The interface, engine selection and failure semantics are one responsibility. Built-in evaluation is a second:
it makes the gate compile and execute policy content in the same language as the engine, with its own cost
bounds, its own builtin denylist bound to a specific backend build, its own determinism audit and its own
content validation. The only stated separator is the absence of a write API. §12 names the risk ("The gate
becomes a second policy engine by increments") and its mitigation — "The boundary is management, not
expressiveness" — concedes full expressiveness and therefore the whole evaluator surface; §12 line 640 further
concedes the exposure is shared ("one audit serving two gears"). The neighbour describes this gear as "a thin
gateway that admits or rejects, delegating the check to the one policy engine selected" (`policy-engine` PRD,
line 83), which §5.2 is not. Two components evaluating the same language with independently configured bounds
and independently maintained denylists will drift on a backend upgrade. **Proposal**: either state in §4.1 that
the gate carries a deliberately minimal second evaluation responsibility and require its facility posture
(denylist, bounds, capability set) to be one shared configuration with `policy-engine` so drift is structurally
impossible — or constrain built-in policies to a non-Turing matching form, which would remove the second
responsibility, the denylist, the cost bound and the non-existent p1 dependency at once. Record the choice as an ADR.

---

### 40. §6.2's exclusion list omits areas the gear demonstrably touches, including operator documentation

**Axis**: Domain sweep · **Checklist**: `DOC-PRD-001`
**Severity**: MEDIUM
**Location**: §6.2 lines 468-475

§6.2 disposes of six areas well, which makes the areas absent from it read as considered-and-included when they
are unaddressed: operator documentation, alerting and incident response, recovery/backup/DR, and offline
operation. `grep -niE 'documentation|runbook|alert|on-call|offline|air-gap'` → no match. The sibling disposes of
ten areas and makes operator documentation a gear-specific MUST for this exact failure profile (`policy-engine`
PRD line 851: "Because a misconfigured limit … refuses every gated operation across every consuming gear, the
configuration surface, the operational limits, and the bootstrap path … MUST be documented for operators before
first release"). The argument is stronger here, and §5.6 line 408 makes it in the gear's own words: "a guardrail
that silently never fires is the failure an operator is least able to detect". The Platform Operator actor is
charged with responding to incidents and the PRD requires no alert and no runbook. **Proposal**: extend §6.2 to
dispose of each remaining area explicitly, and add operator documentation as a gear-specific requirement on the
sibling's rationale.

---

### 41. `p1` requirements depend on a `p1` dependency the document says does not exist, making the "p1 complete" milestone unreachable

**Axis**: PRD-3 Logical gaps
**Severity**: MEDIUM
**Location**: §10 line 618 · §12 line 639 · §1.3 line 59

Four p1 requirements are written against the evaluation facility; §10 records it as "Does not exist yet, in any
form, anywhere in the repository" and §12's mitigation is "Nothing this gear can mitigate". Four of §1.3's five
milestones are gated on "p1 complete". The document is honest about the dependency, which is why this is not
higher — but the goals table promises a milestone that no amount of work on this gear can reach. **Proposal**:
condition the affected milestones on the dependency landing, or re-tier the four requirements (see finding 18).

---

### 42. The deferral major-version transition says nothing about a caller still on the previous major

**Axis**: PRD-3 Logical gaps
**Severity**: LOW
**Location**: §5.1 line 226 · §7.1 line 487

`fr-deferral-verdict` requires a major version bump and never says what happens across it. In the in-process
shape compilation answers the question; `fr-remote-decision-surface` projects the same contract over a network
where a v1 caller and a v2 gate genuinely coexist, and a deferral would reach a caller that cannot represent it.
Both requirements are p3, so this is small now and a compatibility incident later. **Proposal**: state that
where both majors are served, a deferral is presented to a previous-major caller as a refusal carrying the
awaiting-approval cause, never as an admission or an unrepresentable value.

---

### 43. "No approval service exists" is asserted twice; the gear exists with structured upstream requirements

**Axis**: PRD-4 Gear boundary · PRD-2 Contradictions
**Severity**: LOW
**Location**: §4.2 line 165 · §13 line 659

`gears/approval-service/` exists with a 133-line `UPSTREAM_REQS.md` carrying p1 requirements
(`cpt-cf-approval-service-upreq-register-resource`, `-query-status`, `-status-changed-event`) sourced from
`model-registry`, plus p2 entries. Only its PRD and DESIGN are TODO stubs. What does not exist is a
specification, not the gear — and that changes where §13's deferral-termination question should be routed: there
is an owning gear to address it to, and `UPSTREAM_REQS.md` is exactly the artifact for two gears carrying the
same unresolved question. **Proposal**: correct both assertions to "an approval service that is registered as a
gear but not yet specified", and contribute an upstream requirement to that gear sourced from
`fr-deferral-verdict` and `cpt-cf-policy-engine-fr-deferral-outcome`.

---

### 44. "Does not exist … in any form, anywhere in the repository" overstates the evaluation-facility gap

**Axis**: PRD-2 Contradictions
**Severity**: LOW
**Location**: §10 line 618

Correct for a shared *policy* evaluation facility. Overstated as written: `event-broker` PRD line 116 specifies
a `FilterEngine` — "Plugin trait (`compile()` + `eval()`) for evaluating filter expressions over events.
GTS-typed … v1 built-in CEL engine. Resolved at JOIN via `ClientHub`." That is an expression-evaluation facility
in some form and the nearest precedent. It matters because §12's mitigation is "nothing this gear can mitigate";
if a comparable trait exists one gear over, the mitigation is "generalise the existing one", which is materially
cheaper. **Proposal**: restate as "No shared policy evaluation facility exists. The nearest precedent is the
gear-local `FilterEngine` compile/eval plugin trait in `event-broker`, which is not shared and carries no policy
semantics." The same wording appears verbatim in `policy-engine`'s PRD line 1055; fix both together.

---

### 45. §3.1 grants the records an exception to the no-persistent-state property that §2.2 and §10 deny

**Axis**: PRD-2 Contradictions
**Severity**: LOW
**Location**: §3.1 line 140 vs §2.2 line 128 and §10 line 622

"The gate holds no persistent state of its own **beyond its records**" carves out an exception both other
sections deny: the records are not state the gate holds, they are events it emits to a topic it does not own.
The no-store property is the gear's defining constraint — DESIGN.md line 138 calls it exactly that — and this is
the sentence a later change will cite when adding a table. **Proposal**: "The gate holds no persistent state of
its own. Its records are emitted to the audit topic of `event-broker` and are not retained here."

---

## Industry comparison

Evidence base behind findings 6, 7, 21 and 37. The problem class is standard — admission control over management
operations with a pluggable decision point — and the document's core shape matches it: single interception
point, bounded call, fail-closed default, deny-first ordering, versioned contract, prohibition-only guardrails,
and a deliberate refusal to mutate. The gaps are all on the operational side of the class rather than the
structural side.

| System | How it solves the problem | Relevance |
|---|---|---|
| [K8s admission webhooks](https://kubernetes.io/docs/reference/access-authn-authz/extensible-admission-controllers/) | Declarative `rules`/selectors/`matchConditions` decide whether to call at all; bounded `timeoutSeconds`; per-webhook `failurePolicy` (`Fail` default); staged rollout `Ignore`→`Fail`; exclude `kube-system` to avoid deadlock | Canonical form. Aligned on interception, bounding, fail-closed default, deny-first. Diverges on rollout mode and platform carve-out |
| [K8s ValidatingAdmissionPolicy](https://kubernetes.io/docs/reference/access-authn-authz/validating-admission-policy/) | In-process CEL — the same choice this PRD makes for built-ins. `validationActions: [Deny\|Warn\|Audit]`; deterministic runtime **cost** budget rather than a wall clock | Closest comparable to §5.2. Supplies both missing pieces: non-blocking action, load-independent bound |
| [OPA Gatekeeper](https://open-policy-agent.github.io/gatekeeper/website/docs/violations/) | Explicit `match` field before evaluation; `enforcementAction: deny\|dryrun\|warn`; namespace exemption requiring both a label and an operator flag; `http.send` discouraged | Validates the "no capability into evaluation" stance. Supplies match scope, dry-run, and a non-escalating exemption design |
| [Kyverno](https://kyverno.io/docs/policy-types/cluster-policy/validate/) | validate / mutate / generate split — the split §4.2 adopts by exclusion; `failureAction: Enforce\|Audit` with per-namespace overrides; `PolicyException` as a governable object | Confirms the mutation exclusion is a recognised boundary, not an omission |
| [Envoy `ext_authz`](https://www.envoyproxy.io/docs/envoy/latest/api-v3/extensions/filters/http/ext_authz/v3/ext_authz.proto) | Synchronous check, 200 ms default timeout, `failure_mode_allow` defaults to false; when enabled, stamps `x-envoy-auth-failure-mode-allowed` so the bypass is visible in the request | Confirms fail-closed-by-default. Also shows the pattern §5.4's "the bypass would be silent" argument does not engage: make the bypass visible rather than absent |
| [AWS Organizations SCPs](https://docs.aws.amazon.com/organizations/latest/userguide/orgs_manage_policies_scps.html) | Never grant, only cap — structurally identical to "a built-in may only prohibit"; unwithdrawable by member admins; management account carved out; staged OU rollout strongly recommended | Independently validates the prohibition-only design. Shows carve-out and staged rollout are treated as necessary companions |
| [Azure Policy](https://learn.microsoft.com/en-us/azure/governance/policy/concepts/effect-basics) | Documented effect ordering with `deny` before `audit`; cumulative most-restrictive combination; exemptions as first-class objects with expiry, approver metadata and a dedicated permission | Ordering and combination match §5.1 — aligned. Exemption object is the reference design the gear has no analogue of |
| [GCP Organization Policy](https://docs.cloud.google.com/resource-manager/docs/organization-policy/dry-run-policy) | Hierarchy-wide constraints with a first-class dry-run mode; documented workflow is dry-run → review audit logs → enforce | Second independent confirmation that dry-run precedes enforcement for exactly this class of guardrail |
| [K8s audit backends](https://kubernetes.io/docs/tasks/debug/debug-cluster/audit/) | `batch` / `blocking` / `blocking-strict`; the trade-off between losing audit events and failing the request is explicit and configurable | Basis for finding 3. This PRD requires batch semantics and never states the boundary behaviour; the sibling takes the blocking-strict position |

## Checked and clean

- Requirement IDs: all 41 `cpt-cf-admission-control-*` identifiers referenced in the document are defined in it. The one cross-gear reference, `cpt-cf-policy-engine-fr-deferral-outcome`, resolves (`policy-engine` PRD line 474) and is described accurately.
- Template conformance: all fourteen sections of `docs/spec-templates/gears-sdlc/PRD/template.md` present and in order.
- Requirement language: "MUST"/"MUST NOT" throughout; no "SHOULD"/"MAY" smuggled in as priority.
- Actors: every actor in §2 is referenced by at least one requirement, and every actor named in a requirement is defined in §2.
- Scope discipline on mutation: the exclusion of generating and mutating policy is explicit, argued, and consistent across §4.2, §5.1 and `nfr-non-modification` — and matches the industry's validate/mutate split.
- Decision ordering: `fr-decision-order`, `fr-engine-result` and the batch combination rule are mutually consistent and match the deny-first, most-restrictive convention of Azure Policy and AWS SCPs.
- Declared Open Questions: all seven in §13 are genuine open questions, correctly excluded from the gap findings above except where a question conflicts with settled normative text (finding 26).

# Industry benchmarking method

Shared by PRD Pass A (are these the right requirements?) and DESIGN Pass F (is this the right
architecture?). The point is not to prove the document wrong by pointing at a famous product. It is
to find the specific thing a team that has already run this system in production learned the hard
way, and check whether this document knows it.

## Why this pass exists

Most platform problems — admission control, policy evaluation, quota enforcement, type registries,
event distribution — have been solved several times in public. The failure modes are documented, the
architectures have converged, and the interesting differences are narrow. A spec that quietly departs
from that convergence is either innovating or has not looked; the review's job is to make the author
say which.

## Method

**1. Name the problem in the industry's vocabulary.** The document will use the team's internal
terms. Translate first, because the search only works with the standard name. If the document
describes intercepting a request and accepting or rejecting it against policy, that is *admission
control*; if it describes evaluating a subject–action–resource triple, that is a *policy decision
point*. Getting this wrong makes the whole pass useless — a search for the internal term returns the
team's own documents or nothing.

**2. Pick three to five genuine comparables.** They must actually solve the same problem at
comparable scale. Good sources of comparables for this platform's problem space:

| Problem area | Comparables worth checking |
|---|---|
| Admission / request interception | Kubernetes admission webhooks and admission controllers, Envoy `ext_authz`, Istio authorization policy, API gateway policy plugins |
| Policy decision and evaluation | Open Policy Agent / Gatekeeper, AWS Cedar and Verified Permissions, Google Zanzibar and SpiceDB, OpenFGA, XACML, AuthZEN |
| Type / schema registries | Confluent Schema Registry, Buf Schema Registry, JSON Schema registries, OpenAPI registries |
| Quota, rate limiting, metering | Envoy RLS, Stripe's rate limiters, cloud-provider service quota APIs |
| Multi-tenant identity and tenancy | AWS Organizations, GCP resource hierarchy, Azure management groups, Auth0 / Okta org models |
| Eventing and distribution | Kafka, CloudEvents, outbox pattern literature, xDS |

This table is a starting point, not a limit. If the document's problem is not here, find the
comparables by searching for the problem name plus "architecture", "design", "at scale", or "post
mortem".

**3. Prefer primary sources.** Official documentation, specifications, RFCs, published engineering
write-ups and conference talks from the team that built it. A blog post summarizing someone else's
architecture is a weak citation and often wrong on the details that matter here. Use `WebFetch` to
read the actual page rather than relying on a search snippet — the snippet routinely omits the
qualification that makes the comparison valid.

**4. Compare on the decisions, not the feature list.** Feature-list comparison produces noise. The
questions that produce findings are the ones every implementation of this problem class has had to
answer:

- What happens when the decision point is unavailable — fail open or fail closed, and is it
  configurable per policy?
- Is evaluation synchronous on the request path, or is the decision precomputed or cached? What is
  the cache invalidation story, and what staleness is accepted?
- How does configuration or policy reach the enforcement point, and how is a stale version detected?
- What is the latency budget, and what is in it?
- How are partial failures and retries made safe — idempotency keys, versions, compare-and-swap?
- What is versioned, and what is the compatibility rule when it changes?
- What is the multi-tenancy boundary, and can one tenant affect another's decisions or latency?
- What is auditable, and can an operator answer "why was this denied" after the fact?
- Is there a dry-run, shadow or observe-only mode? For anything that can deny production traffic,
  this is close to universal, and its absence is a real finding.

**5. Turn each comparison into one of four outcomes.**

- **Aligned** — the document matches industry practice. Say so; it belongs in the verdict, not in the
  findings.
- **Deliberate divergence** — the document differs and explains why. Not a finding unless the
  justification does not hold up, in which case say specifically why.
- **Undeclared divergence** — the document differs and does not acknowledge it. A finding. Severity
  follows the consequence, not the fact of divergence.
- **Unaddressed** — every comparable answers a question this document does not ask. Usually the most
  valuable finding this pass produces.

## Evidence rules

- Every claim about another system carries a URL. No URL, no claim.
- Prefer "Kubernetes admission webhooks have a per-webhook `failurePolicy` of `Ignore` or `Fail`
  (link)" to "most systems let you configure failure behavior".
- Say when you are unsure. "OPA's bundle API supports delta bundles; I did not confirm whether
  Gatekeeper uses them" is honest and still useful. Inventing the detail is not.
- If a search produces nothing usable, report the axis as inconclusive and say what you searched.
  A fabricated industry consensus is worse than an admitted gap — it will be quoted back in a design
  review.

## Scope discipline

Do not turn this into a product-comparison essay. The output is a short evidence table plus findings
that name a concrete change to this document. Anything the author cannot act on does not belong in
the report.

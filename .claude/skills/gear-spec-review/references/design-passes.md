# DESIGN review passes

Six passes plus a fresh-reader pass and a domain sweep. Spawn them all in one message. Each returns
a JSON array per `report-format.md`.

Shared preamble for every pass:

```
You are one pass of a DESIGN review. Read /tmp/.../context.json first: it names the gear, the DESIGN
and PRD paths, the extracted PRD requirement IDs, the sibling ADRs, the database.sql if one exists,
and the foundational documents this gear inherits from.

Read the whole DESIGN before judging any part of it, and read the PRD too unless your pass says
otherwise. A design document contradicts itself across 800 lines, not within a paragraph.

Return a JSON array of findings and nothing else. An empty array is a good answer. Every absence
claim must record the literal search you ran. Before claiming something is missing, check the gear's
ADR/ directory and the foundational documents — content that legitimately lives there is not missing
from the DESIGN.

Schema: {axis, checklist_id?, severity, title, location, evidence, issue, why_it_matters, proposal, sources?}

Two standing obligations for every pass:

**Check the document's claims about the repository.** Design documents assert facts — that a component does
not exist, that a schema lives at a path, that a sibling contract says something. Those are cheap to test and
they age badly. When the document states an external fact in your area, run the check.

**When you cite another document, cite it precisely.** Path, line number, and — for a requirement — its
priority tier. The orchestrator re-verifies every cross-document quotation, so an imprecise one costs the finding.

**Severity discipline.** Use `docs/checklists/README.md`'s dictionary literally. If you have more than five
HIGH-or-above, you are inflating — rank and demote the tail.

## Who owns what

Passes run blind to each other, so one defect surfaces three times unless each stays in its lane. Report a
defect only if it belongs to your axis; if you notice one outside it, ignore it — another pass has it.

| Defect | Owner |
|---|---|
| A PRD requirement with no design response | A (coverage) |
| Two statements that cannot both hold | B (contradictions) |
| A statement contradicted by the repository | B (contradictions) |
| Option comparison or rejected-alternative prose | C (layer) |
| Unhandled failure path, undefined boundary, dead-end sequence | D (gaps) |
| Missing or underspecified contract, schema, authorization, GTS | E (concrete artifacts) |
| Architecture that departs from industry practice | F (industry) |
| Anything in REL / OPS / PERF / TEST / COMPL / MAINT / SEM not covered above | H (sweep) |


The reference-quality example in this repo is `gears/system/types-registry/docs/DESIGN.md` with its
`database.sql` and 15 ADRs. When you are unsure how deep a section should go, compare against it —
it is what "done" looks like here.

---

## Pass A — DES-1 Requirement coverage

**Tools**: Read, Grep, Bash.

Template §1.2 requires a Functional Drivers table and an NFR Allocation table mapping every PRD
requirement to a design response. Verify against the extracted ID list, not against your reading:

```bash
comm -23 "$WORK/prd_ids.txt" <(grep -oE 'cpt-[a-z0-9-]+-(fr|nfr|usecase)-[a-z0-9-]+' <design> | sort -u)
```

That gives the requirements the DESIGN never mentions. Then do the harder half — for every
requirement the DESIGN *does* mention, decide whether it is actually realized:

- **Named but not realized.** The driver table has a row, the "Design Response" cell restates the
  requirement in other words, and no component, sequence, contract or table anywhere implements it.
  This is the most common coverage defect and a table-presence check will not catch it.
- **Partially realized.** The design covers the requirement's main clause and drops a qualifier —
  the requirement says "per tenant" and the design is global, or the requirement says "atomically"
  and the design describes two writes.
- **NFR with no mechanism.** An NFR allocation whose Design Response names no mechanism and no
  verification approach. A latency budget allocated to "efficient queries" is not allocated.
- **Design without requirement.** A component, endpoint or table serving no PRD requirement. Either
  the PRD is missing a requirement or the design is over-built; say which you think it is.

Also check use cases: PRD §8 use cases should map to DESIGN §3.6 sequences, and every sequence should
cite the use case and actor IDs it serves.

Emit the full requirement coverage table as an extra key in your result:
`{"findings": [...], "coverage_table": [{"id": "...", "covered_in": "§3.2" | null, "assessment": "..."}]}`.

Finally, check template conformance: the sections of
`docs/spec-templates/gears-sdlc/DESIGN/template.md` that are marked required and are absent, and the
`cpt-*` IDs referenced in the DESIGN that exist nowhere (dangling), including ADR IDs pointing at
ADRs that do not exist in `<gear>/docs/ADR/`.

## Pass B — DES-2 Contradictions

**Tools**: Read, Grep.

Two directions, both required.

**Against the PRD:**
- The design does something the PRD puts out of scope, or the design's scope is narrower than the
  PRD's without saying so.
- The design uses a term in a different sense than the PRD glossary defines it.
- The design's behavior differs from the requirement it claims to satisfy — different defaults,
  different ordering, different failure semantics.
- The design's stated performance or availability properties conflict with a PRD NFR.
- The design assumes an actor, dependency or precondition the PRD does not establish.

**Inside the DESIGN itself** — this is where long documents rot:
- A component's stated responsibility boundary in §3.2 versus what a sequence in §3.6 has it doing.
- A sequence diagram against the prose beneath it.
- A table in §3.7 against the entity description in §3.1 — a column that does not exist, a
  relationship the schema cannot express, a nullable field the prose treats as mandatory.
- An API contract in §3.3 against the component that serves it, or against the errors §3.x defines.
- A principle in §2.1 that some component in §3 violates without a stated exception.
- A dependency in §3.4 that the component model never uses, or a component call to a gear §3.4 does
  not list.
- The same mechanism described twice, in two places, differently. Say which description you believe.

Quote both sides of every contradiction and, where the document supports it, say which is intended.

## Pass C — DES-3 Layer discipline

**Tools**: Read, Grep.

The DESIGN says what the architecture is. An ADR says why it beat the alternatives. Mixing them makes
the design unreadable and the decision unfindable — and this repo already keeps 15 ADRs for the
reference gear, so the discipline is established, not aspirational.

Grep for the tells: `we chose`, `we decided`, `alternative`, `option`, `instead of`, `rather than`,
`trade-off`, `tradeoff`, `pros`, `cons`, `considered`, `rejected`, `we could have`, `the downside`,
`this is better than`, `why not`.

Then judge each hit, because two of these are legitimate:

- **Not a finding**: a one-line statement of a decision with a pointer to its ADR ("Admission is
  asynchronous (ADR-0012)"). That is exactly the intended pattern — the DESIGN is primary, the ADR
  carries rationale.
- **Not a finding**: a constraint explaining why the architecture *cannot* do something ("a composite
  foreign key would not cover the nullable global scope"). That is design reasoning about a
  mechanism, not a decision debate about options.
- **A finding**: an options comparison, a pros/cons list, an evaluation of rejected alternatives, or a
  paragraph justifying a choice against competitors. Move it to an ADR and leave a one-line decision
  plus reference.

Also apply the `MUST NOT HAVE` items of `docs/checklists/DESIGN.md` — `ARCH-DESIGN-NO-001/002`,
`BIZ-DESIGN-NO-003/004`, `DATA-DESIGN-NO-001`, `INT-DESIGN-NO-001`, `OPS-DESIGN-NO-001`,
`TEST-DESIGN-NO-001`, `MAINT-DESIGN-NO-001`, `SEC-DESIGN-NO-001` — and cite the ID. Note the
distinction those items draw: a schema table in §3.7 is required, Rust struct definitions are not; an
endpoint overview table in §3.3 is required, a full OpenAPI body is not.

Name the destination for every finding: which ADR, or which other artifact.

## Pass D — DES-4 Logical gaps

**Tools**: Read, Grep.

Architectural gaps, not requirement gaps:

- **Failure paths.** For every external call in §3.4/§3.5 and every sequence in §3.6: what happens on
  timeout, on error, on partial failure. Whether the gear fails open or closed, and whether that
  matches the PRD. Multi-step writes with no statement of what happens if step 2 fails after step 1
  committed.
- **Component boundaries.** A component whose "Responsibility boundaries" subsection is missing or
  restates the responsibility scope. Two components that could both own the same state.
- **Sequences that dead-end.** A sequence that ends without a response, or that calls an operation no
  contract defines.
- **Concurrency.** Concurrent writers to the same entity, and what serializes them: a lock, a unique
  constraint, a compare-and-swap token, an ordering rule. "The database handles it" is a gap.
- **State and lifecycle.** Entities created with no defined deletion, cascade or retention. Caches
  with no invalidation rule. Versioned things with no compatibility rule.
- **Bootstrapping and ordering.** A gear that depends on something that depends on it, or that must
  be available before its own dependency is.
- **Scale.** Something described as a table scan, an unbounded fan-out, an unbounded list, or a
  per-item external call, where the PRD states a latency or throughput budget.
- **Observability.** For a platform gear on a request path, no statement of what is logged, metered
  or traced, when the PRD or checklist expects it.

Each finding names the scenario. "Error handling is underspecified" is not a finding; "§3.6
Registration ends at the outbox enqueue — if the dispatcher never picks the message up, no section
says how the operation leaves `accepted`" is.

## Pass E — DES-6/DES-7 Contracts, data, authorization, GTS

**Tools**: Read, Grep, Glob.

Four checks in one pass because they are the concrete-artifact checks and they overlap.

**Contracts (§3.3).** Is the public API surface actually specified — operations, paths, methods,
stability, and the errors each can return? Does it match the PRD's §7 Public Library Interfaces? For
inter-gear calls, does the design say the interaction goes through a versioned contract, SDK client
or plugin interface rather than internal types (template §3.4 dependency rules)? Is `SecurityContext`
propagation stated? Is versioning and evolution addressed for anything a consumer will depend on?

**Database (§3.7).** Either tables documented inline with columns, types, primary key, constraints
and indexes, or a normative `database.sql` with an inventory table explaining what each table holds —
the reference gear does the latter. Check that the schema actually supports the operations §3.2 and
§3.6 describe: the uniqueness constraint the concurrency argument depends on, the index the latency
NFR needs, the column a compare-and-swap requires. A schema that cannot serve a described flow is a
CRITICAL. Also check multi-backend implications, since this platform targets SQLite, PostgreSQL and
MySQL.

**Authorization.** Read `docs/arch/authorization/DESIGN.md` and check conformance, not merely
presence. Its model is specific: a PDP/PEP split, AuthZEN-based evaluation with an extended response,
`SecurityContext` versus `PlatformSecurityContext` for the tenant and platform planes, constraint
compilation to SQL through advertised `supported_properties`, and decision caching. Related documents:
`TENANT_MODEL.md`, `RESOURCE_GROUP_MODEL.md`, `PERMISSION_GTS_TYPE.md`.

Findings to raise:
- No authorization section at all for a gear that serves tenant-visible data (CRITICAL).
- Authorization asserted but the actions, resource type and subject are not named (HIGH).
- A home-grown check where the platform model expects a PDP call, with no stated exemption (HIGH).
- Tenant-plane and platform-plane paths not distinguished (HIGH).
- Constraint handling unspecified: whether the gear advertises `supported_properties`, and what it
  does with a constraint it cannot compile — silently ignoring one is a security defect (HIGH).
- Visibility conflated with authority, i.e. "the tenant can see it" used as the access decision
  (HIGH). The reference DESIGN argues this distinction explicitly; it is a known trap.
- Fail-open on PDP unavailability, or the behavior unstated (CRITICAL when unstated on a write path).
- Deviation from the platform authorization model that is not called out and justified (HIGH).

**GTS.** Read `guidelines/GTS.md` §13 (Reviewing GTS in PRD) and §14 (Reviewing GTS in DESIGN) and
apply them; §1 says when a GTS type is warranted at all, §2–3 fix identifier format, §4 separates
type from instance, §11 covers abstract and final types, §12 the Gears conventions. Check:

- Whether this gear should define GTS types at all — cross-gear contracts, plugin identity, extension
  points and permission resource types are the signals. If it should and does not, that is a finding;
  if it should not, do not manufacture one.
- Identifier format and correctness of every `gts.` identifier that appears in the document.
- Type versus instance used correctly, and version semantics stated.
- Whether declared types are documented as abstract or final where that matters.
- Where the schemas live and how they are registered, if the DESIGN claims them.
- For a gear participating in authorization: the permission resource type, per `PERMISSION_GTS_TYPE.md`.

If the gear genuinely has no GTS surface, return that as an explicit no-finding note rather than
silence, so the orchestrator can mark the axis N/A instead of unchecked.

## Pass F — DES-5 Industry alignment

**Tools**: WebSearch, WebFetch, Read. **Skipped under `--quick`** (it needs network and is the slowest pass).
**Follow `industry-research.md` for method.**

The PRD industry pass asks whether the requirements are right. This one asks whether the architecture
is: given this problem, is this the structure large platforms converge on, and where it differs, is
the difference deliberate?

Cover:
- **Overall shape.** Sidecar versus library versus central service; synchronous versus asynchronous
  admission; push versus pull distribution; embedded versus remote evaluation. Name the pattern the
  design implements and the systems that use it.
- **Known-hard problems** in this class and how the comparables handle them: policy distribution and
  staleness, decision caching and invalidation, fail-open versus fail-closed, evaluation latency and
  its budget, partial failure, ordering and idempotency, multi-tenancy, upgrade and versioning.
  For each, whether this design addresses it and how its answer compares.
- **Anti-patterns.** Structures the industry has moved away from — a synchronous remote call on every
  request in a hot path, unbounded policy recursion, a single global lock, an unversioned contract
  between independently deployed components.

Findings must be actionable and specific. "This differs from OPA" is not a finding; "policy changes
propagate by polling every 30 s with no generation token, so a revoked policy stays live for up to
30 s — OPA bundles and Envoy xDS both carry a version and support push, and §6 of the PRD requires
revocation to be effective immediately" is. Cite a URL for every claim about another system.

## Pass G — Fresh reader

**Tools**: Read only, and only the one file.

```
You are reading a technical design document you have never seen, from a codebase you do not know.
Read only the file at <path>. Do not read any other file, do not search the repository, do not search
the web. If you find yourself reasoning from general knowledge about what such a system "probably"
does, stop and record it as an assumption you had to make.

Answer from the document alone:
1. What does this component do, and what does it deliberately not do?
2. What are its components and which one owns each piece of state?
3. Walk the main write path end to end. Where does it fail, and what happens then?
4. Who is authorized to do what, and how is that decided?
5. What is stored, and where?
6. If you had to implement this, what is the first thing you would have to ask the author?

Then report: (a) questions you could not answer, (b) terms used before definition, (c) statements
that appeared to conflict, (d) background knowledge the document assumes, (e) any place you had to
guess to keep reading.

Return JSON: {"answers": {...}, "unanswerable": [...], "undefined_terms": [...],
"apparent_conflicts": [...], "assumed_knowledge": [...], "guesses": [...]}.
```

The `guesses` list is the highest-signal output — a place where a competent reader had to invent
something is a place where an implementer will invent something different.

## Pass H — Domain checklist sweep

**Tools**: Read, Grep. **Skipped under `--quick`.**

Read `docs/checklists/DESIGN.md` in full and apply it under its Applicability Context and Evidence
Requirements rules. Report **CRITICAL and HIGH only**, cite the checklist ID, and stay off the axes
the other passes own — the value here is the domains they do not cover: REL, OPS, PERF, TEST, COMPL,
MAINT, and the SEM (semantic alignment) group.

# PRD review passes

Five passes plus a fresh-reader pass and a domain sweep. Spawn them all in one message. Each returns
a JSON array per `report-format.md`.

Every pass prompt starts with this preamble — the passes are independent, so each needs it:

```
You are one pass of a PRD review. Read /tmp/.../context.json first; it tells you the gear, the
document path, the gear inventory, the foundational documents this gear inherits from, and the open
questions the document itself declares.

Read the whole PRD before judging any part of it. Requirements contradict each other across
sections, not within a paragraph.

Return a JSON array of findings and nothing else. An empty array is a good answer when the document
is sound on your axis. Every absence claim must record the literal search you ran. Before claiming
something is missing, also check <gear>/docs/ADR/ and the foundational documents listed in
context.json — content that legitimately lives there is not missing from the PRD.

Schema: {axis, checklist_id?, severity, title, location, evidence, issue, why_it_matters, proposal, sources?}

Two standing obligations for every pass:

**Check the document's claims about the repository.** These documents assert facts — that a gear does not
exist, that a dependency is nowhere in the tree, that a sibling requirement says something. Those assertions
age badly and are cheap to test. When the document states an external fact in your area, run the check.

**When you cite another document, cite it precisely.** Path, line number, and — for a requirement — its
priority tier. "The engine requires X" reads very differently once you notice the requirement is `p3`. The
orchestrator re-verifies every cross-document quotation, so an imprecise one costs the finding.

**Severity discipline.** Use `docs/checklists/README.md`'s dictionary literally. CRITICAL blocks downstream
work; HIGH must be fixed before approval. If you find yourself with more than five HIGH-or-above, you are
inflating — rank them and demote the tail.

## Who owns what

Passes run blind to each other, so the same defect surfaces three times unless each pass stays in its lane.
Report a defect only if it belongs to your axis; if you notice one outside it, ignore it — another pass has it.

| Defect | Owner |
|---|---|
| Missing or unenumerated failure behaviour | C (gaps) |
| Two requirements that cannot both hold | B (contradictions) |
| A statement contradicted by the repository | B (contradictions) |
| Missing capability the industry considers standard | A (industry) |
| Overlap with an existing gear, or a weak boundary argument | D (boundary) |
| Rationale that is really a decision record | E (layer) |
| Anything in SEC / REL / DATA / COMPL / OPS / TEST not covered above | G (sweep) |


---

## Pass A — PRD-1 Industry alignment

**Tools**: WebSearch, WebFetch, Read. **Skipped under `--quick`** (it needs network and is the slowest pass).
**Follow `industry-research.md` for method.**

The question is not "is this document well written" but "is this problem real, is this the shape the
industry has converged on, and are these requirements the ones that matter". A PRD that invents a
novel requirement set for a solved problem is expensive in a way no amount of internal consistency
fixes.

Produce three things:

1. **Problem prevalence.** Is this a recognized problem class? Name the term the industry uses for
   it — the PRD may be describing admission control, policy decision points, quota enforcement or
   rate limiting without using the standard name, which makes it hard to reuse known solutions.
2. **Comparable systems.** Three to five real systems that solve this problem, with what each one
   does structurally. Prefer primary sources: official docs, specs, published architecture write-ups.
3. **Requirement-by-requirement comparison.** For the PRD's functional requirements, which are
   standard, which are unusual (and whether the PRD justifies being unusual), and — the highest-value
   output — which requirements the comparables have that this PRD lacks entirely.

Findings to raise:
- A requirement that departs from universal industry practice with no rationale (HIGH).
- A capability every comparable has that this PRD does not mention and does not exclude (HIGH when
  it is load-bearing, e.g. fail-open/fail-closed semantics, decision caching, policy versioning,
  dry-run, audit trail; MEDIUM otherwise).
- The PRD solving a problem the platform already gets from an established component (HIGH; overlaps
  with PRD-4).
- Terminology that collides with an established industry term while meaning something else (MEDIUM) —
  this reliably causes misimplementation.

Do not raise a finding just because a comparable has a feature; ask whether its absence would hurt
*this* platform. Cite a URL for every claim about another system.

## Pass B — PRD-2 Contradictions

**Tools**: Read, Grep.

Contradictions in a PRD are found by cross-referencing, not by reading top to bottom. Work these
pairs explicitly:

- **Scope vs requirements** — an FR that requires something §4.2 lists as out of scope, or an in-scope
  bullet with no requirement behind it.
- **FR vs FR** — two requirements that cannot both hold; two requirements that specify the same
  behavior differently; the same concept named two ways.
- **FR vs NFR** — a requirement whose obvious realization violates a stated threshold (a per-item
  external call under a 10 ms p95 budget).
- **Requirement vs glossary** — a term used in a sense the glossary does not license, or drifting
  between sections.
- **Priorities** — a `p1` requirement depending on a `p2` requirement or on an out-of-scope capability.
- **Actors** — an actor with no requirement referencing it, or a requirement naming an actor §2 does
  not define.
- **Acceptance criteria vs requirements** — §9 asserting an outcome no requirement produces.
- **Dependencies and assumptions** — an assumption that some other section contradicts.

For each finding, quote **both** sides and say which one you believe is intended, if the document
gives you grounds to. A contradiction report that does not say which way to resolve it makes the
author do the work twice.

## Pass C — PRD-3 Logical gaps

**Tools**: Read, Grep.

A gap is something the document needs to be complete and does not have. Probe:

- **Unfinished flows** — a requirement for a happy path with no statement of what happens on failure,
  timeout, or unavailability of a dependency. Fail-open versus fail-closed is the classic omission
  and is almost always load-bearing.
- **Undefined limits** — anything unbounded: list sizes, nesting depth, retention, retry counts,
  payload size, cardinality. Unbounded is a decision; leaving it unstated is a gap.
- **Lifecycle holes** — creation without deletion, deletion without cascade semantics, no statement of
  what happens to existing data when a definition changes.
- **Presupposed capability** — a requirement that only makes sense if some other capability exists,
  and that capability is neither required here nor sourced from a named dependency.
- **Multi-tenancy** — for a platform gear, whether tenant isolation and cross-tenant visibility are
  stated. `docs/arch/authorization/TENANT_MODEL.md` is the reference; deviating from it silently is
  the finding.
- **Concurrency and ordering** — two actors doing the same thing at once, and whether the document
  says who wins.
- **Migration and compatibility** — for a change to an existing gear: what happens to data and clients
  that predate it.
- **Unverifiable requirements** — a requirement no test could fail. "The system MUST be performant"
  is a gap, not a requirement.

Each finding must name the scenario that exposes the gap. "Concurrency not addressed" is weak; "two
tenants registering the same identifier concurrently — §5.1 does not say which one wins" is a finding.

## Pass D — PRD-4 Gear boundary

**Tools**: Read, Grep, Glob, Bash.

This is the pass most reviews skip and most reorganizations later wish had happened. Two cases:

**New gear.** The PRD must argue why this is a gear rather than a feature of an existing one. Do the
work yourself before judging: `context.json` has the gear inventory; for each plausible neighbour,
read its `docs/PRD.md` overview and scope sections and ask whether the new requirements are a natural
extension of what it already owns. Overlap-prone neighbours in this repo include `policy-engine`,
`admission-control`, `authz-resolver`, `authn-resolver`, `quota-enforcement`, `types-registry`,
`resource-group`, `account-management`, `usage-collector` and `api-gateway`.

Raise a finding when:
- The PRD gives no justification at all for being a separate gear (HIGH).
- The justification exists but does not engage with the closest neighbour by name (HIGH).
- Requirements overlap materially with an existing gear's scope and the boundary is not drawn
  (CRITICAL when both would own the same data or the same decision).
- The gear's stated responsibility is not cohesive — it is two gears (HIGH).
- The gear is defined by a technology rather than a responsibility (MEDIUM).

**Change to an existing gear.** The question becomes: why do these requirements belong in *this*
gear? Raise a finding when new requirements pull in a responsibility that another gear already owns,
when they would force this gear to depend on something it currently does not, or when they break the
gear's stated scope boundary in §4 without amending it.

In both cases, if the boundary argument exists and is sound, say so in the verdict rather than
inventing a concern.

## Pass E — PRD-5 Layer discipline

**Tools**: Read, Grep.

A PRD states what and why. Implementation detail belongs in DESIGN; the reasoning that picked one
option over another belongs in an ADR. The `MUST NOT HAVE` section of `docs/checklists/PRD.md`
(`ARCH-PRD-NO-001/002`, `BIZ-PRD-NO-001/002`, `DATA-PRD-NO-001`, `INT-PRD-NO-001`, `TEST-PRD-NO-001`,
`OPS-PRD-NO-001`, `SEC-PRD-NO-001`, `MAINT-PRD-NO-001`) enumerates the categories; read that section
and apply it. Cite the ID.

Signals worth grepping: `because we chose`, `instead of`, `alternative`, `trade-off`, `we evaluated`,
`SQL`, `CREATE TABLE`, `endpoint`, `HTTP`, `crate`, `struct`, `enum`, `Arc<`, `async`, table names,
class names, library names, and any code fence.

Two judgement calls this pass keeps getting wrong, so make them deliberately:

- **A constraint is not an implementation detail.** "MUST persist decisions durably" is a
  requirement. "MUST use PostgreSQL with a `decisions` table" is not. Naming an external system the
  gear must interoperate with is a requirement; naming the library used to talk to it is not.
- **Interface requirements are allowed.** Template §7 asks for the public API surface and stability
  guarantees. A named interface with a stability contract belongs there; a full endpoint list with
  request and response schemas does not.

For each finding, name the destination: "move to DESIGN §3.3" or "extract as an ADR — the PRD should
retain only the requirement, not the reasoning".

## Pass F — Fresh reader

**Tools**: Read only, and only the one file.

Prompt this pass differently — it must have no repo context:

```
You are reading a specification document you have never seen, from a codebase you do not know. Read
only the file at <path>. Do not read any other file, do not search the repository, do not search the
web. If you are tempted to reason from general knowledge of what a system like this "probably" does,
stop and record that as an assumption you had to make.

Answer these questions from the document alone:
1. What problem does this gear solve, and for whom?
2. What are the three most important things it must do?
3. What happens when its main dependency is unavailable?
4. Who is allowed to do what, and how is that decided?
5. What is explicitly not being built?
6. If you had to implement this, what is the first question you would need answered?

Then report: (a) every question you could not answer from the document, (b) every term used before
it was defined, (c) every place two statements seemed to conflict, (d) every piece of background
knowledge the document assumes you already have.

Return JSON: {"answers": {...}, "unanswerable": [...], "undefined_terms": [...],
"apparent_conflicts": [...], "assumed_knowledge": [...]}.
```

Rank every list by consequence and cap each at the eight items that would most change an implementation.
This pass generates far more raw material than any other, and an unranked dump of forty terminological
observations buries the three places where a competent reader had to invent something. Drop anything a careful
reader could resolve from the document with a second pass over it — the target is what is genuinely absent, not
what is merely inconvenient to find.

The orchestrator converts this into findings in Step 3 — a confusion is only a finding if the
document neither answers it nor links to where it is answered.

## Pass G — Domain checklist sweep

**Tools**: Read, Grep. **Skipped under `--quick`.**

Read `docs/checklists/PRD.md` in full and apply it, honouring its Applicability Context rules: an
item that does not apply to a platform infrastructure gear is not a finding, but an item that applies
and is neither addressed nor explicitly excluded is.

Report **CRITICAL and HIGH only**. The other passes already cover the axes the user cares most about;
this pass exists to catch the domains they do not — SEC, REL, DATA, OPS, TEST, COMPL. Cite the
checklist ID on every finding. Do not duplicate anything the other passes are chartered to find.

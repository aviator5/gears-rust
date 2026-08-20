---
name: gear-spec-review
description: "Review a gear's PRD.md or DESIGN.md as a spec reviewer: industry benchmarking against how large platforms solve the same problem, contradictions, logical gaps, PRD->DESIGN requirement coverage, layer discipline (requirements vs design vs ADR), justification for a new gear versus extending an existing one, plus authorization, database schema, contracts and GTS type coverage. Use this whenever the user asks to review, critique, sanity-check, audit or give feedback on a PRD, DESIGN, spec, requirements document or design document under gears/ - including when they just paste document paths, say 'review these docs', 'посмотри эти документы', 'что не так с этим дизайном', or ask whether a design matches industry practice. Produces a written review report and does not modify the reviewed documents."
user-invocable: true
allowed-tools: Bash, Read, Glob, Grep, Write, WebSearch, WebFetch, Agent
---

# Gear Spec Review

Review a gear's `PRD.md` or `DESIGN.md` and produce a findings report. This skill never edits the
reviewed document — the author does that, informed by the report.

**Usage**: `/gear-spec-review <path-or-gear-dir> [more paths...] [--prd] [--design] [--quick]`

Examples:
- `/gear-spec-review gears/system/policy-engine/docs/DESIGN.md`
- `/gear-spec-review gears/system/admission-control` — reviews both PRD and DESIGN
- `/gear-spec-review gears/system/policy-engine gears/system/admission-control --quick`

---

## Table of Contents

- [What this reviews](#what-this-reviews)
- [Review axes](#review-axes)
- [Inputs and target resolution](#inputs-and-target-resolution)
- [Step 0 — Orientation](#step-0--orientation)
- [Step 1 — Build the context pack](#step-1--build-the-context-pack)
- [Step 2 — Spawn the parallel passes](#step-2--spawn-the-parallel-passes)
- [Step 3 — Verify before reporting](#step-3--verify-before-reporting)
- [Step 4 — Write the report](#step-4--write-the-report)
- [Step 5 — Terminal summary](#step-5--terminal-summary)
- [Finding quality bar](#finding-quality-bar)
- [What not to do](#what-not-to-do)

---

## What this reviews

A PRD states **what** the system must do and **why**. A DESIGN states **how** it is built. An ADR
records **why one option was chosen over others**. Most defects in these documents are not typos —
they are content that landed in the wrong document, requirements that contradict each other three
sections apart, requirements that the design silently dropped, and architectures that reinvent
something the industry settled a decade ago. This skill hunts those.

Three bodies of existing material do part of the job already; use them instead of re-deriving them:

| Source | Use it for |
|---|---|
| `docs/checklists/PRD.md`, `docs/checklists/DESIGN.md` | Per-domain expert sweep (BIZ/ARCH/SEC/REL/DATA/INT/OPS/TEST), the severity dictionary, the report format, and the `MUST NOT HAVE` lists |
| `docs/spec-templates/gears-sdlc/{PRD,DESIGN}/template.md` | Required section structure, ID conventions, requirement language |
| `gears/system/types-registry/docs/` | Reference-quality PRD, DESIGN, `database.sql` and 15 ADRs — the bar for density, traceability and cross-linking |

The checklists are large (~1000 lines each). Only the domain-sweep pass reads them in full; the other
passes work from this skill's reference files.

## Review axes

These are the axes the report is organized by. They exist because a generic "is this a good doc"
review reliably misses the expensive problems.

**PRD** (details: `references/prd-passes.md`)

| Axis | Question |
|---|---|
| PRD-1 Industry alignment | How common is this problem, how do large players solve it, and do these functional requirements appear in their solutions? |
| PRD-2 Contradictions | Does any statement conflict with another statement in the same document? |
| PRD-3 Logical gaps | What does the document assume without stating, or start without finishing? |
| PRD-4 Gear boundary | Why a new gear rather than extending an existing one — and for an edit to an existing PRD, why these requirements belong in *this* gear? |
| PRD-5 Layer discipline | Implementation detail that belongs in DESIGN, or decision rationale that belongs in an ADR |

**DESIGN** (details: `references/design-passes.md`)

| Axis | Question |
|---|---|
| DES-1 Requirement coverage | Is every PRD FR and NFR realized by something concrete in the design? |
| DES-2 Contradictions | Conflicts with the PRD, and conflicts between two parts of the DESIGN itself |
| DES-3 Layer discipline | Option comparisons, trade-off debates and "we chose X because" — those belong in ADRs |
| DES-4 Logical gaps | Flows that dead-end, components with undefined boundaries, dangling references |
| DES-5 Industry alignment | Do the architectural choices match how large players build this? |
| DES-6 Contracts, data, authorization | API contracts, database schema, and authorization conforming to `docs/arch/authorization/DESIGN.md` |
| DES-7 GTS types | GTS types, instances and identifiers per `guidelines/GTS.md` §13–14 |

## Inputs and target resolution

Resolve each argument:

1. A path ending in `PRD.md` or `DESIGN.md` — review that document.
2. A directory — look for `<dir>/docs/PRD.md` and `<dir>/docs/DESIGN.md`, or `<dir>/PRD.md` and
   `<dir>/DESIGN.md`. Review whichever exist, subject to `--prd` / `--design`.
3. Nothing — ask which document to review rather than guessing.

When both documents of one gear are in scope, **review the PRD first**. The DESIGN passes need the
PRD's requirement IDs, and a DESIGN finding is often downstream of a PRD defect; saying so is more
useful than reporting it twice.

`--quick` runs only the axes that need no web access (PRD-2/3/4/5, DES-1/2/3/4) and skips the domain
sweep. Use it when the user wants a fast structural pass.

## Step 0 — Orientation

Judging a document you have not situated produces confident nonsense: "missing tenant model" when the
tenant model is a repo-wide document this gear correctly links to. Before any pass, spend a few
cheap commands establishing what the gear is and what it may legitimately rely on:

```bash
cd <repo-root>
ls gears/*/ gears/system/*/ -d                       # the gear inventory — needed for PRD-4
ls <gear>/docs/ <gear>/docs/ADR/ 2>/dev/null         # sibling artifacts: ADRs, database.sql, features/
wc -l <target-doc> <sibling-docs>
git log --oneline -5 -- <gear>/docs/                 # is this new, or an edit to something established?
```

Read the target document in full yourself. You are the orchestrator; you cannot verify a sub-agent's
findings against a document you have not read.

Then note, for the passes to consume:

- Whether this is a **new gear** or a **change to an existing one** — PRD-4 asks a different question
  in each case.
- Which **foundational documents** the gear legitimately inherits from rather than restating:
  `docs/ARCHITECTURE_MANIFEST.md`, `guidelines/`, `docs/arch/authorization/DESIGN.md`, and any parent
  gear PRD. Both templates say gear specs document only *deviations* from these. An omission that the
  gear inherits is not a finding; an omission it silently inherits *while deviating* is.
- Any **open questions the document itself declares**. An author who wrote "Open Questions: how do we
  handle X" has not forgotten X. Report that only if the open question blocks something the document
  elsewhere claims is settled.

- The **closest sibling document** — the gear specified alongside this one, in the same commit or against the
  same problem. Name it in the context pack. Reading the two side by side is the highest-yield single technique
  in this review: siblings written by the same author against the same platform diverge in revealing ways, and
  each divergence is either a defect in one of them or a boundary nobody drew. A requirement the sibling has and
  this one lacks, a property the sibling made normative and this one left observable, a contract both claim to
  own — none of these are visible from inside one document.

If something material is genuinely unresolvable from the repo — organizational context, a decision
made in a meeting, which of two conflicting readings the author intended — ask the user. Ask once, at
this step, batched. Do not block the whole review on it: run the passes that do not depend on the
answer while you wait.

## Step 1 — Build the context pack

Sub-agents each need the same facts and should not each re-derive them. Write them once:

```bash
WORK=/tmp/gear-spec-review-$(basename <gear>)-$(date +%s)
mkdir -p "$WORK"
```

Write `$WORK/context.json`:

```json
{
  "repo_root": "<abs path>",
  "gear": "<gear name>",
  "gear_path": "<gears/system/foo>",
  "doc_type": "PRD" | "DESIGN",
  "doc_path": "<abs path to target>",
  "prd_path": "<abs path or null>",
  "design_path": "<abs path or null>",
  "adr_paths": ["<...>"],
  "db_sql_path": "<abs path or null>",
  "is_new_gear": true,
  "gear_inventory": ["account-management", "policy-engine", "..."],
  "foundational_docs": ["docs/ARCHITECTURE_MANIFEST.md", "docs/arch/authorization/DESIGN.md", "..."],
  "prd_requirement_ids": ["cpt-cf-foo-fr-bar", "..."],
  "declared_open_questions": ["..."]
}
```

Extract `prd_requirement_ids` mechanically, so the coverage pass compares against reality rather than
against its own reading:

```bash
grep -oE 'cpt-[a-z0-9-]+-(fr|nfr|usecase|interface|contract)-[a-z0-9-]+' <prd> | sort -u > "$WORK/prd_ids.txt"
grep -oE 'cpt-[a-z0-9-]+-[a-z]+-[a-z0-9-]+' <design> | sort -u > "$WORK/design_ids.txt"
```

Also copy the target document (and the PRD, when reviewing a DESIGN) into `$WORK/` so every agent
quotes the same bytes.

## Step 2 — Spawn the parallel passes

Read `references/prd-passes.md` or `references/design-passes.md` — whichever matches `doc_type` — and
spawn every pass defined there in a **single message** so they run concurrently. A 1200-line design
document does not survive one sequential read with twelve questions in mind; each pass holds one
question and finds things a combined pass misses.

Each pass returns a JSON array of findings using the schema in `references/report-format.md`. Passes
that need the industry comparison follow `references/industry-research.md`.

One pass is unusual and must stay that way: the **fresh-reader pass** receives only the document and
is explicitly forbidden from reading anything else in the repo or the web. Its value is that it
cannot fill gaps from context the author's colleagues happen to share — which is exactly the position
a new engineer, an auditor, or a model reading the doc in isolation is in. Its confusions are raw
input, not findings; Step 3 decides which are real.

## Step 3 — Verify before reporting

The dominant failure mode of automated doc review is the confident false positive: "no authorization
model" when §3.4 defines one, or "FR-7 uncovered" when the DESIGN covers it under a different name.
A report with three fabricated findings gets the whole report ignored. So every finding earns its
place:

1. **Merge and dedupe.** Same defect from two passes → keep the one with better evidence, and note
   both axes. Findings that only differ in wording are one finding.
2. **Re-check every CRITICAL and HIGH yourself.** For "missing X", run the search: `grep -in
   'x\|synonym\|related-term' <doc>`. Record the exact search in the finding's evidence. If it hits,
   demote or drop. For "contradicts Y", read both cited passages in full — many apparent conflicts
   dissolve once you see that one is scoped to the platform plane and the other to the tenant plane.
3. **Re-verify every cross-document quotation.** A pass that quotes another gear's PRD, an ADR, or a
   foundational document is making the claim you are least able to check by reading the document under review —
   and, in practice, those are the findings that matter most and the ones most likely to be paraphrased into
   something the source does not say. Open the cited file at the cited line. Confirm the quote is verbatim and
   the surrounding context does not reverse it, and confirm the priority tier of any requirement cited from
   another gear: a `p3` obligation elsewhere does not carry the same weight as a `p1` one, and severity should
   follow.

   Then **search the whole source document for the concept, not just the cited lines**. A pass that reports
   "document B contradicts document A about X" has usually found the one place B mentions X and stopped; B
   frequently settles X somewhere else entirely — a later section, a contracts table, a glossary entry — and
   reading only the cited excerpt confirms a conflict that does not exist. This is the single most likely way a
   confident false positive reaches the report, because the quote checks out and the claim is still wrong.
   `grep -n` the source document for the concept before accepting any cross-document contradiction.
4. **Apply the inheritance test.** Is the "missing" content actually owned by a foundational document
   the gear links to? Then it is not missing. If the gear *deviates* from that document without
   saying so, that is a finding — a sharper one.
5. **Apply the wrong-document test in both directions.** Something absent from the PRD may be
   correctly present in an ADR. Check `<gear>/docs/ADR/` before reporting a PRD or DESIGN omission.
6. **Resolve fresh-reader confusions.** A fresh-reader failure is a real finding only if the document
   neither states the answer nor links to where it is stated. If the answer is in the repo but
   unlinked, that is a MEDIUM "document is not self-contained", not a CRITICAL gap.
7. **Drop the speculative.** If you cannot cite a location or a concrete failing scenario, the
   finding does not go in the report. "Consider whether…" is not a finding.

Assign severity from `docs/checklists/README.md`: CRITICAL blocks downstream work; HIGH must be fixed
before approval; MEDIUM improves clarity; LOW is polish.

## Step 4 — Write the report

Write to the repository root as `REVIEW-<gear>-<PRD|DESIGN>.md`, matching the existing convention for
review artifacts in this repo. State the path in your reply. Do not commit it and do not edit the
reviewed document — this skill reports only.

Use the exact structure in `references/report-format.md`. It combines the per-issue format from
`docs/checklists/*.md` (Why Applicable / Issue / Evidence / Why It Matters / Proposal) with an
axis-verdict table at the top, so the author can see in ten seconds which of the twelve questions the
document fails.

A dense document can legitimately produce thirty findings, and thirty findings are not actionable as a list.
Lead with the ordered shortlist: the few defects that, fixed first, unblock or dissolve the rest. Findings
frequently share a root cause — one under-scoped requirement can produce a gap, a contradiction, a missing
fault-injection case and a hollow acceptance criterion — so say that rather than presenting four independent
problems.

Report problems only. Do not enumerate what is fine — with one exception: the axis-verdict table
names every axis, including the passing ones, because "we checked industry alignment and it holds up"
is information the author needs.

## Step 5 — Terminal summary

Print a compact table so the user can triage without opening the file:

```text
## PRD Review: policy-engine  →  REVIEW-policy-engine-PRD.md

| Axis | Verdict | Findings |
|------|---------|----------|
| PRD-1 Industry alignment | CONCERNS | 2 |
| PRD-2 Contradictions | FAIL | 1 CRITICAL, 2 HIGH |
| ... | | |

Top 3:
1. CRITICAL §5.1 vs §4.2 — FR-decision-caching requires state the scope section excludes
2. HIGH §5.3 — no bound on policy evaluation depth; OPA and Cedar both cap this
3. HIGH §1.2 — "why a new gear" not argued against authz-resolver, which already owns PDP calls
```

When several documents were reviewed, print one block per document, PRD before DESIGN, and add one
line naming DESIGN findings that are downstream of a PRD defect.

## Finding quality bar

Write findings the way a senior reviewer writes them: the author should be able to act without asking
you a follow-up question.

- **Cite a location.** Section number and heading, plus a line number where useful.
- **Quote or prove absence.** Either the exact text, or the exact search you ran and its empty result.
- **Say what breaks.** Not "this is unclear" but "an implementer reading §3.2 will build per-request
  PDP calls; §1.2's 10 ms p95 budget does not survive that."
- **Propose something concrete.** A sentence to add, a section to move to an ADR, a requirement to
  split. "Clarify this" is not a proposal.
- **Cite sources for industry claims.** A named system and a URL, or the claim does not go in.
- **One defect per finding.** If §3.2 has two problems, that is two findings.

Engineering English. No praise, no hedging, no emoji, no "it might be worth considering".

## What not to do

- Do not edit the reviewed document, the PRD, or any ADR. Report only.
- Do not report an omission without first searching the document, its ADRs, and the foundational
  documents it inherits from.
- Do not re-report the same defect once per axis.
- Do not report style, formatting, or wording preferences. Table of contents generation, heading case
  and markdown polish are not what this review is for.
- Do not invent industry facts. If a search returns nothing usable, say the comparison was
  inconclusive rather than asserting what "most platforms do".
- Do not treat a declared Open Question or an explicitly out-of-scope item as a gap.
- Do not pad the report to look thorough. Twelve real findings beat forty, and a document that is
  genuinely sound should get a short report saying so.

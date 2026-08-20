# Finding schema and report format

## Finding schema (sub-agent output)

Every pass returns a JSON array — nothing else, no prose before or after it. An empty array is a
valid and useful answer.

```json
[
  {
    "axis": "PRD-2",
    "checklist_id": "BIZ-PRD-003",
    "severity": "CRITICAL",
    "title": "Short noun phrase naming the defect",
    "location": "§5.1 Registry Core, line 412 (and §4.2, line 205)",
    "evidence": "\"The system MUST cache decisions for 60s\" (§5.1) vs \"Caching is out of scope\" (§4.2)",
    "issue": "What is wrong, in one or two sentences.",
    "why_it_matters": "The concrete downstream consequence.",
    "proposal": "The specific change: text to add, section to move, requirement to split.",
    "sources": ["https://..."]
  }
]
```

Field rules:

- `axis` — one of `PRD-1`…`PRD-5`, `DES-1`…`DES-7`, or `SWEEP` for the domain-checklist pass.
- `checklist_id` — the `docs/checklists/*.md` item ID when one applies (e.g. `ARCH-PRD-NO-001`),
  otherwise omit the field. Do not invent IDs.
- `severity` — `CRITICAL` | `HIGH` | `MEDIUM` | `LOW`, per the dictionary in
  `docs/checklists/README.md`.
- `location` — section number and heading. Include line numbers when they help; include *both*
  locations for a contradiction.
- `evidence` — quoted document text, or, for an absence claim, the literal search performed and its
  result: `"grep -in 'retention|purge|ttl' PRD.md → no match"`. An absence claim without a recorded
  search is not admissible.
- `sources` — required for any claim about what other systems do; omit otherwise.

## Report file structure

```markdown
# {PRD|DESIGN} Review — {gear name}

**Document**: `{path}` ({N} lines)
**Reviewed against**: `docs/spec-templates/gears-sdlc/{PRD|DESIGN}/template.md`, `docs/checklists/{PRD|DESIGN}.md`{, PRD.md for a DESIGN review}
**Date**: {YYYY-MM-DD}
**Reviewer**: gear-spec-review

## Verdict by axis

| Axis | Verdict | Findings |
|------|---------|----------|
| PRD-1 Industry alignment | PASS \| CONCERNS \| FAIL \| N/A | — |
| ... one row per axis, including passing ones ... |

{One paragraph: the two or three things that actually matter, and whether the document is ready to
build from. Say it plainly — "ready once the two CRITICALs are closed" is more useful than a hedge.}

## Fix first

{The ordered shortlist — usually three to six — that unblocks or dissolves the rest. One line each, naming the
finding number. Where several findings share a root cause, say so here rather than leaving the author to infer
it from thirty separate entries.}

1. **Finding {N}** — {what to change, in one clause}. Findings {M}, {K} follow from it.
2. ...

## Findings

### 1. {Short issue title}

**Axis**: {PRD-2 Contradictions}{ · **Checklist**: `BIZ-PRD-003`}
**Severity**: CRITICAL
**Location**: §5.1, line 412 · §4.2, line 205

**Why applicable**: {Why this requirement applies to this document's context. Skip when obvious —
it exists so the author can tell "you forgot this" from "this does not apply to you".}

**Issue**: {What is wrong.}

**Evidence**:
> {quoted text}

{or: `grep -in 'pattern' PRD.md` → no match}

**Why it matters**: {Concrete consequence.}

**Proposal**: {Concrete fix.}

{**Sources**: {name} — {url}}

---

### 2. ...

## Industry comparison

{Only for reviews that ran the industry pass. A short table of the comparable systems examined and
what each one does about the core problem, then the deltas worth acting on. Findings themselves live
in the Findings section; this section is the evidence base behind them.}

| System | How it solves the problem | Relevance |
|--------|---------------------------|-----------|

## Requirement coverage

{DESIGN reviews only. Every PRD FR/NFR ID and where the design realizes it.}

| Requirement ID | Covered in DESIGN | Assessment |
|----------------|-------------------|------------|
| `cpt-cf-foo-fr-bar` | §3.2 Component Model | Covered |
| `cpt-cf-foo-nfr-baz` | — | **Not covered** — see finding 4 |

## Checked and clean

{One line per axis or major check that produced nothing, so the author knows it was examined rather
than skipped. Keep it to a list of short lines — no elaboration.}
```

## Ordering

Findings are sorted CRITICAL → HIGH → MEDIUM → LOW, and within a severity by document order. Numbering
is sequential across the whole list so findings can be referenced as "finding 7".

## Root causes

Where one defect produces several findings, add `caused_by: <finding number>` to the downstream entries rather
than repeating the analysis. A reader who fixes the root should be able to see immediately which of the
remaining findings close with it.

## Length discipline

There is no target finding count. A sound document gets a short report. If a pass produces twenty
LOW findings, that pass drifted into copy-editing — drop them and say so in one line rather than
burying the CRITICALs.

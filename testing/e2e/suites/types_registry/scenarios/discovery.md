# Discovery scenarios (`GET /entities`)

End-to-end discovery scenarios cover namespace patterns, the `kind`, `depth` and lifecycle filters, projection, cursor continuation and hydration through `:batchGet`. The local launcher uses **SQLite**; these tests do not prove PostgreSQL/MySQL or tenant/PDP behavior.

Every scenario has an e2e test in [test_discovery.py](../test_discovery.py) or [test_discovery_patterns.py](../test_discovery_patterns.py); the [suite README](../README.md) lists each group. Each scenario lists its source fixtures before Given and refers to them by name in Given/When/Then; all fixture links point to `fixtures/discovery/`. Every scenario registers into a fresh target namespace. Namespace-isolation scenarios additionally register neighbouring controls; identifier-matching scenarios use version, name, kind and chain controls within the target namespace.

Unless a scenario says otherwise, discovery lists only active entities, a walk follows every `next_cursor` with the same query until none remains, and it returns each matching entity once in canonical GTS-ID order. Items are projected as an exact read projects them: the mandatory `gts_id`, `gts_uuid`, `kind` and `lifecycle_status` accompany every selection, an absent `$select` returns the document-free default metadata, and a field that does not apply to an item's kind is absent. Pages carry no validators.

## Contents

- [Paged discovery and returned fields](#paged-discovery-and-returned-fields)
  - [TR-DISC-001 — Limit-one cursor visits every row exactly once](#tr-disc-001--limit-one-cursor-visits-every-row-exactly-once)
  - [TR-DISC-002 — Pattern excludes neighbouring namespaces](#tr-disc-002--pattern-excludes-neighbouring-namespaces)
  - [TR-DISC-003 — Content projection survives page continuation](#tr-disc-003--content-projection-survives-page-continuation)
  - [TR-DISC-004 — Inapplicable selected artifacts remain absent on Instances](#tr-disc-004--inapplicable-selected-artifacts-remain-absent-on-instances)
- [Filters](#filters)
  - [TR-DISC-101 — Kind selects Type Schemas or Instances](#tr-disc-101--kind-selects-type-schemas-or-instances)
  - [TR-DISC-102 — Pattern, depth, kind, and selection compose](#tr-disc-102--pattern-depth-kind-and-selection-compose)
  - [TR-DISC-103 — Lifecycle filter separates live rows and tombstones](#tr-disc-103--lifecycle-filter-separates-live-rows-and-tombstones)
  - [TR-DISC-104 — Type scopes, version wildcards and token boundaries](#tr-disc-104--type-scopes-version-wildcards-and-token-boundaries)
  - [TR-DISC-105 — Pinned type minors and major zero](#tr-disc-105--pinned-type-minors-and-major-zero)
  - [TR-DISC-106 — Concrete instance structure and base-minor flexibility](#tr-disc-106--concrete-instance-structure-and-base-minor-flexibility)
  - [TR-DISC-107 — Later wildcards and beginning-of-ID anchoring](#tr-disc-107--later-wildcards-and-beginning-of-id-anchoring)
  - [TR-DISC-108 — Three segments pin independently](#tr-disc-108--three-segments-pin-independently)
  - [TR-DISC-109 — Unregistered UUID selectors remain valid](#tr-disc-109--unregistered-uuid-selectors-remain-valid)
- [Cursor continuation](#cursor-continuation)
  - [TR-DISC-201 — A changed pattern cannot reuse a cursor](#tr-disc-201--a-changed-pattern-cannot-reuse-a-cursor)
  - [TR-DISC-202 — Page size may change without changing cursor identity](#tr-disc-202--page-size-may-change-without-changing-cursor-identity)
- [Read discovered entities with batchGet](#read-discovered-entities-with-batchget)
  - [TR-DISC-301 — Discovered references can hydrate the complete set through batchGet](#tr-disc-301--discovered-references-can-hydrate-the-complete-set-through-batchget)

## Paged discovery and returned fields

### TR-DISC-001 — Limit-one cursor visits every row exactly once

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register independent `person_schema` and `device_schema` Type Schemas in the target namespace. In a neighbouring namespace register `person_schema`/`person_instance` and `device_schema`/`device_instance` as distinct control pairs. Both target schemas are active.

**When:** GET with the target namespace pattern and `limit=1`, then follow every cursor with the same pattern and limit.

**Then:** exactly two pages of one item each: the first has a `next_cursor`, the second none, and the IDs appear once each in canonical order.

### TR-DISC-002 — Pattern excludes neighbouring namespaces

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active. Register a second `person_schema` with a distinct namespace prefix.

**When:** discover using the first namespace's pattern, then the pattern formed by appending `*` to `employee_schema`'s `gts_id`, then a valid pattern for an unregistered name in the first namespace.

**Then:** the namespace walk contains exactly its 12 entities and excludes the neighbouring schema. The employee branch walk contains exactly `employee_schema`, `manager_schema`, `director_schema`, `employee_instance`, `manager_instance`, and `director_instance`; the contractor branch, direct person Instance and device tree stay outside it. The non-matching pattern returns an empty `items` array with no cursor, not a `404`.

### TR-DISC-003 — Content projection survives page continuation

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active.

**When:** Traverse the target namespace pattern with `limit=1&$select=content` on every page.

**Then:** each item contains only the mandatory `gts_id`, `gts_uuid`, `kind`, and `lifecycle_status` and its whole authored `content`; no `origin` or other document appears. All 12 fixture documents survive the cursor walk unchanged, including derived schemas' authored `$ref` values and deeply derived Instance payloads.

### TR-DISC-004 — Inapplicable selected artifacts remain absent on Instances

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active.

**When:** Discover using the target namespace pattern and `$select=resolved_schema`.

**Then:** all six Type Schema items have a nonempty `resolved_schema` object; all six Instance items have `kind=instance` and omit `resolved_schema`, at every depth. Each item carries the mandatory `gts_id`, `gts_uuid`, `kind`, and `lifecycle_status`.

## Filters

### TR-DISC-101 — Kind selects Type Schemas or Instances

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active.

**When:** Traverse the target namespace separately with `kind=type_schema` and `kind=instance`, each at `limit=2`.

**Then:** the first walk returns exactly the six schema fixtures and the second exactly the six Instance fixtures, each in canonical order regardless of depth or branch. Each page contains only its requested kind, with default metadata fields and no authored content.

### TR-DISC-102 — Pattern, depth, kind, and selection compose

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active. In a second namespace register `person_schema`, then `employee_schema` and `employee_instance`, rewriting their IDs and `$ref` to that namespace.

**When:** discover with `pattern` equal to `employee_schema`'s `gts_id` followed by `*`, `depth=3`, `kind=instance`, and `$select=content`.

**Then:** only the target namespace's `employee_instance` appears, with its authored content and mandatory fields. The neighbouring employee Instance and local contractor Instance test pattern exclusion, the local employee and manager schemas test kind exclusion, and the local manager Instance tests depth exclusion independently. A `depth=2` repeat returns an empty result and terminates.

### TR-DISC-103 — Lifecycle filter separates live rows and tombstones

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register `person_schema` and `device_schema` as independent target schemas, then delete `person_schema` and await completion; `device_schema` remains active. In a neighbouring namespace register `person_schema`/`person_instance` and `device_schema`/`device_instance` as distinct control pairs.

**When:** Discover using the target namespace pattern with absent lifecycle, `active`, `deleted`, and `all`.

**Then:** absent and `active` return only the target live schema, `deleted` only the target tombstone, and `all` both target schemas in canonical order. No neighbouring control appears. The tombstone remains exact-readable.

### Identifier matching coverage

These client examples follow GTS specification §10 and `gts-spec/tests/shared_id_pattern_cases.py` (`MATCH_CASES`). Expected result sets are authored independently of the matcher and compared as complete metadata pages at limit 2, including ordering and continuation. Tests generate schemas from `person_schema` in a fresh namespace and verify every admission outcome.

| GTS matching cases | HTTP scenario |
|---|---|
| `type`, `all_majors`, `first_minor_wildcard` | TR-DISC-104 |
| `pinned_minor`, `v0_chain`, `v0_minor_wildcard`, `v0_pinned` | TR-DISC-105 |
| `instance`, `instance_pinned` (admissible subset) | TR-DISC-106 |
| `later_minor_wildcard`, `later_chain_wildcard`, `derived_type`, `anchored_type`, chain prefix | TR-DISC-107 |
| `chain_instance`, pinned base/derived/instance selectors (admissible subset) | TR-DISC-108 |
| UUID selector acceptance / empty selection | TR-DISC-109 |

The managed admission profile limits which specification candidates can be created through HTTP: a schema major cannot mix major-only and minor-bearing spelling; a version family holds only one kind; registered instances require a major-only last segment, reject explicit UUID tails, and cannot instantiate a major-zero type. Therefore positive UUID matching, explicit instance-minor matching and same-family type/instance distinctions remain lower-level test responsibilities. These are coverage limits, not changes to GTS matching rules.

Selector-to-selector coverage (`COVERAGE_CASES`) is not discovery: stored entities have concrete identifiers. Exhaustive invalid syntax and numeric/length bounds also remain lower-level checks; this suite does not claim complete spec coverage.

### TR-DISC-104 — Type scopes, version wildcards and token boundaries

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json); generated versioned roots, branches and instances from this schema via `pattern_entities`.

**Given:** Person roots with contiguous minors 0–2 under major 1, minors 0–1 under major 0, major-only roots 2 and 10, instances and derived branches, and a `person_extra` control. A derived `employee.v1.0~` under the major-only `person.v2~` root places an explicit minor only in the later segment.

**When:** Query `person.v1~`, `person.v1~*`, `person.v1.*`, `person.*`, `person.v*`, `person.v2.*`, `person.v2~*`, and a namespace wildcard after `person.v2~`.

**Then:** Concrete type and chain wildcard select the same branch across base minors. Family/version-marker wildcards span majors but exclude `person_extra`. Major 1 excludes major 10. `v2.*` excludes the major-only branch selected by `v2~*`, even though its derived type carries an explicit minor. The later namespace wildcard includes only the direct instance and derived type under major 2.

### TR-DISC-105 — Pinned type minors and major zero

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json); generated versioned roots, branches and instances from this schema via `pattern_entities`.

**Given:** The same roots and branches; only stable majors have instances.

**When:** Query `v1.0~` with and without `*`, `v0~*`, `v0.*`, and `v0.1~*`.

**Then:** Pinned minor 0 excludes minors 1 and 2 while retaining descendants with independently versioned segments. Major zero follows the same rules.

### TR-DISC-106 — Concrete instance structure and base-minor flexibility

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json); generated versioned roots, branches and instances from this schema via `pattern_entities`.

**Given:** Direct `alice.v1` instances across base minors, `alice.v10` and `alice_extra.v1` controls, and derived employee types with longer chains.

**When:** Query `person.v1~…alice.v1`, pin the base minor, pin the instance minor, then pin both. Also query instance selectors ending in `employee.v1` and `employee.v1.0`, whose corresponding type branches and longer chains are registered.

**Then:** Flexible base minor selects three instances; a pinned base selects one. Other names, majors and type branches are excluded. A pinned instance minor matches nothing because registered instances have major-only final segments. The employee instance selectors return empty pages: a same-name type and its longer descendants cannot satisfy a concrete instance selector.

### TR-DISC-107 — Later wildcards and beginning-of-ID anchoring

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json); generated versioned roots, branches and instances from this schema via `pattern_entities`.

**Given:** Explicit-minor employee branches under two base minors, a major-only employee branch under base minor 2, their descendants, and direct alice instances.

**When:** Query a later `employee.v1.*`, `employee.v1~*`, concrete employee type, pinned employee type, a namespace wildcard after the base, and a root selector whose employee name occurs only in later segments.

**Then:** The minor wildcard excludes the major-only employee branch; the chain wildcard and concrete type include it. A pinned derived minor narrows only that segment. The namespace wildcard includes both direct instances and derived branches, but no other first-segment major. Root matching never starts at a later segment.

### TR-DISC-108 — Three segments pin independently

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json); generated versioned roots, branches and instances from this schema via `pattern_entities`.

**Given:** Two base minors × two explicit derived minors and a branch with an omitted derived minor under base minor 2, each with an `item.v2` instance.

**When:** Query the three-segment instance selector, pin the derived minor, pin the base minor, pin the final instance minor to 0, and pin base and derived minors together.

**Then:** Each pin constrains only its own segment; omitted base/derived minors remain flexible, including the omitted-derived-minor branch. Pinned final minors produce empty pages because stored instances have major-only final segments. Combining base minor 0 and derived minor 0 selects exactly `leaf_0_0`.

### TR-DISC-109 — Unregistered UUID selectors remain valid

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json); generated versioned roots, branches and instances from this schema via `pattern_entities`.

**Given:** Named instances, derived types and other type majors. Managed registration rejects explicit UUID tails.

**When:** Query a UUID selector under flexible major 1, pinned minor 0, and major 2.

**Then:** Each valid selector returns an empty page, rather than a syntax error or named instances. This proves selector acceptance and exclusion only.

## Cursor continuation

### TR-DISC-201 — A changed pattern cannot reuse a cursor

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register independent `person_schema` and `device_schema` Type Schemas in the target namespace. In a neighbouring namespace register `person_schema`/`person_instance` and `device_schema`/`device_instance` as distinct control pairs. Obtain a first page at `limit=1` under the target namespace pattern, with a nonempty cursor.

**When:** Resume the cursor with the neighbouring namespace pattern, keeping `limit=1` and the original selection.

**Then:** `400` Problem JSON rejects the continuation rather than mixing traversals.

### TR-DISC-202 — Page size may change without changing cursor identity

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [device_schema](../fixtures/discovery/device_schema.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Generate independent closed-envelope roots from `person_schema`, rewriting each `gts_id` and authored `$id` consistently. Register four target roots named `item000` through `item003` in one namespace; their canonical order is known before reading. In a neighbouring namespace register `person_schema`/`person_instance` and `device_schema`/`device_instance` as distinct control pairs.

**When:** request the first page with `limit=1`, the namespace pattern and `$select=content`. Resume its cursor with `limit=2`, preserving the pattern and selection, and continue with limit 2 until no cursor remains.

**Then:** the first page reports limit 1 and contains exactly the first expected entity. The next request succeeds with `200`, reports limit 2 and contains exactly the next two expected entities; subsequent pages contain at most two items and report limit 2. Every item retains the requested projection and expected authored content. The completed walk returns all four entities once in canonical order and terminates. Changing page size neither invalidates the cursor nor resets or skips its position.

## Read discovered entities with batchGet

### TR-DISC-301 — Discovered references can hydrate the complete set through batchGet

**Fixtures:** [person_schema](../fixtures/discovery/person_schema.json), [employee_schema](../fixtures/discovery/employee_schema.json), [manager_schema](../fixtures/discovery/manager_schema.json), [director_schema](../fixtures/discovery/director_schema.json), [contractor_schema](../fixtures/discovery/contractor_schema.json), [device_schema](../fixtures/discovery/device_schema.json), [person_instance](../fixtures/discovery/person_instance.json), [employee_instance](../fixtures/discovery/employee_instance.json), [manager_instance](../fixtures/discovery/manager_instance.json), [director_instance](../fixtures/discovery/director_instance.json), [contractor_instance](../fixtures/discovery/contractor_instance.json), [device_instance](../fixtures/discovery/device_instance.json).

**Given:** Register six Type Schemas (`person_schema` → `employee_schema` → `manager_schema` → `director_schema`, sibling `contractor_schema`, independent `device_schema`) and their six Instances (`person_instance`, `employee_instance`, `manager_instance`, `director_instance`, `contractor_instance`, `device_instance`) in one fresh namespace, in dependency order; all are active. Its 12 GTS IDs and authored documents are known from these fixtures, and no mutation occurs during the reads.

**When:** traverse namespace-scoped discovery with `limit=1` and no `$select`. Collect each item's `gts_uuid` in discovery order and submit those returned references directly as batchGet keys, with top-level `"$select": "content"`. Do not derive UUIDs or substitute fixture GTS IDs for the discovered references.

**Then:** discovery returns exactly the 12 expected entities in canonical GTS-ID order, with default metadata and no documents or validators, and the walk terminates. BatchGet returns `200` with 12 `found` results, each carrying an `etag` and echoing its UUID key in canonical lowercase hyphenated form. Match results by key after checking completeness and uniqueness; batch response order is not contractual. Each entity has exactly its mandatory fields and authored `content`; its identity matches the corresponding discovery item and its content matches the independent fixture. Every discovered reference resolves, and no expected entity is missing or duplicated in the hydrated result.

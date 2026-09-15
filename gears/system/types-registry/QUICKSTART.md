# Types Registry - Quickstart

Stores Global Type System (GTS) Type Schemas and Instances with versioning,
dependency checks and schemas materialized at admission.

Features:

- Asynchronous writes: submit, then poll per-entity outcomes
- Optimistic concurrency via `expected_resource_version`
- Single/batch deletion, blocked by live direct registered dependants
- Dry run on every mutation; entity state stays unchanged
- Required `Idempotency-Key`; replay returns the same operation, changed content conflicts
- Reads by GTS identifier or Registry Reference UUID, including tombstones
- Batch reads with one explicit result per key, absence included
- Bounded, content-free discovery with an opaque cursor

Full API documentation: <http://127.0.0.1:8087/cf/docs>

## Surfaces

| Base path | What it is |
|---|---|
| `/types-registry/v1` | Legacy in-memory API; removed after consumer migration |
| `/types-registry/v2` | Database-backed async API below; promoted to `/v1` after migration |

`/v2` manages global platform entities without tenant ownership.
All gear routes are internal (`exposed = false`) but appear in `/cf/docs`.
Exposing mutations requires platform authentication (`X-ToolKit-Internal-Token` /
`PlatformIdentity`) on a separate listener, followed by a PDP decision before dispatch.

Use the gear's internal base URL:

```bash
BASE="$TYPES_REGISTRY_INTERNAL"
```

## Examples

Examples use `cf`, the only platform vendor allowed by default; others need an `allowed_vendors` policy region.

### Register a Type Schema, then poll the outcome

```bash
RECEIPT=$(curl -s -X POST "$BASE/types-registry/v2/entities" \
  -H "Idempotency-Key: register-example-event-1" \
  -H "Content-Type: application/json" \
  -d '{
        "items": [{
          "gts_id": "gts.cf.core.example.event.v1~",
          "content": {
            "$id": "gts://gts.cf.core.example.event.v1~",
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "email": { "type": "string" } }
          }
        }]
      }')
```

Response: **202 Accepted**, `Location: …/types-registry/v2/operations/{operation_id}`
and an advisory `Retry-After: 1`. The receipt body carries the same id:

```bash
OPERATION_ID=$(printf '%s' "$RECEIPT" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["operation_id"])')
```

Follow the `Location`:

```bash
curl -s "$BASE/types-registry/v2/operations/$OPERATION_ID" | python3 -m json.tool
```

```json
{
    "operation_id": "34af3e4e-4927-4a98-a028-d4c2fe9edc95",
    "kind": "registration",
    "dry_run": false,
    "status": "completed",
    "items": [
        {
            "gts_id": "gts.cf.core.example.event.v1~",
            "status": "succeeded",
            "resource_version": 1,
            "error": null
        }
    ]
}
```

`completed` means all items are terminal; inspect their outcomes. Read the entity's artifacts and `gts_uuid` Registry Reference:

```bash
curl -s "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~" \
  | python3 -m json.tool
```

### Rehearse a deletion, then perform it

Dry run checks preconditions, lifecycle and dependants, and persists a pollable prediction
without changing entities or assigning a `resource_version`:

```bash
curl -s -X DELETE \
  "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~?expected_resource_version=1&dry_run=true" \
  -H "Idempotency-Key: rehearse-delete-1"
```

To commit, omit `dry_run` and use a new idempotency key. Reusing the dry-run key returns `409`:

```bash
curl -s -X DELETE \
  "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~?expected_resource_version=1" \
  -H "Idempotency-Key: delete-1"
```

The entity remains readable with `"lifecycle_status": "deleted"` and an advanced version.
Same-key replay returns **200**, `Idempotency-Replayed: true`, without deleting twice.

Both deletion routes require a positive `expected_resource_version`. Missing, non-numeric
or zero returns `400`; a mismatch returns `202` then item `precondition_failed`.
`If-Match` is rejected because the version check is asynchronous.

Batch deletion accepts a GTS identifier or Registry Reference in each `key`.
Outcomes use GTS identifiers in request order:

```bash
curl -s -X POST "$BASE/types-registry/v2/entities:batchDelete" \
  -H "Idempotency-Key: delete-batch-1" \
  -H "Content-Type: application/json" \
  -d '{
        "items": [
          { "key": "gts.cf.core.example.event.v1~", "expected_resource_version": 1 },
          { "key": "d226dd5b-14c8-56da-a718-9cf29becaba1", "expected_resource_version": 2 }
        ]
      }'
```

Unknown Registry References return `404` (no identifier for an item outcome); absent GTS identifiers fail asynchronously.

`dry_run` defaults to `false`: body field here (`"dry_run": true`), query parameter for single deletion.

### Read a set of entities in one round trip

`:batchGet` answers every key it is given. A `POST` because a GTS identifier runs to
1024 characters, which a query string cannot carry safely, and portable `GET` has no body.
Each `key` is a GTS identifier or a Registry Reference UUID:

```bash
curl -s -X POST "$BASE/types-registry/v2/entities:batchGet" \
  -H "Content-Type: application/json" \
  -d '{
        "items": [
          { "key": "gts.cf.core.example.event.v1~" },
          { "key": "d226dd5b-14c8-56da-a718-9cf29becaba1" },
          { "key": "gts.cf.core.example.missing.v1~" }
        ]
      }' | python3 -m json.tool
```

```json
{
    "items": [
        { "key": "gts.cf.core.example.event.v1~", "status": "found", "entity": { "...": "..." } },
        { "key": "d226dd5b-14c8-56da-a718-9cf29becaba1", "status": "found", "entity": { "...": "..." } },
        { "key": "gts.cf.core.example.missing.v1~", "status": "not_found" }
    ]
}
```

Results come back in request order, each echoing the key it was asked by, so a caller that
mixed identifiers and Registry References matches answers to questions without re-deriving
either. A `found` result carries the same full representation the exact read returns —
authored content plus the materialized artifacts. An absent key is `not_found` inside a
`200`, not a `404`: one missing key must not lose the answers for the others.

A key named twice collapses onto its first mention. The two spellings of one entity are two
keys and get two results. At most 100 keys per request, the same ceiling registration and
deletion batches carry; an empty `items` is a `400`. A `found` result carries the authored
document and all three materialized artifacts, so the key count is what bounds the
response size.

`If-None-Match` is refused rather than ignored, because validators are per key: each item
carries its own `if_none_match` slot.

### Discover what exists, then hydrate

`GET /entities` returns **one bounded page** of active entities, ordered by canonical
identifier. Deleted entities are excluded — a tombstone stays readable by key and leaves
discovery:

```bash
curl -s "$BASE/types-registry/v2/entities?limit=2&pattern=gts.cf.core.*" \
  | python3 -m json.tool
```

```json
{
    "items": [
        {
            "gts_id": "gts.cf.core.example.event.v1~",
            "gts_uuid": "d226dd5b-14c8-56da-a718-9cf29becaba1",
            "kind": "type_schema",
            "lifecycle_status": "active",
            "resource_version": 1,
            "owning_gear": "types-registry",
            "created_at": "2026-09-15T09:15:30Z",
            "updated_at": "2026-09-15T09:15:30Z"
        }
    ],
    "page_info": { "next_cursor": "eyJ2IjoxLCJrIjpb...", "limit": 2 }
}
```

A page is **content-free**: identity and metadata only, with no `content`, no
`resolved_schema`, no `effective_traits` and no validator. Discovery answers *what exists*;
an exact read or `:batchGet` answers *what is in it*. So the read pattern is page, then
hydrate the identifiers the page gave you:

```bash
# Page, collecting identifiers until next_cursor is absent.
CURSOR=""
IDS=""
while :; do
  PAGE=$(curl -s "$BASE/types-registry/v2/entities?limit=100&cursor=$CURSOR")
  IDS="$IDS $(echo "$PAGE" | python3 -c 'import json,sys
for item in json.load(sys.stdin)["items"]: print(item["gts_id"])')"
  CURSOR=$(echo "$PAGE" | python3 -c 'import json,sys
print(json.load(sys.stdin)["page_info"].get("next_cursor") or "")')
  [ -n "$CURSOR" ] || break
done

# Hydrate: full documents for what the traversal found.
echo "$IDS" | python3 -c 'import json,sys
print(json.dumps({"items": [{"key": k} for k in sys.stdin.read().split()]}))' \
  | curl -s -X POST "$BASE/types-registry/v2/entities:batchGet" \
      -H "Content-Type: application/json" -d @-
```

The traversal ends when `page_info.next_cursor` is **absent** — not when a page comes back
short. One page is bounded in work as well as in results, so a selective `pattern` over a
large table can legitimately return nothing and still hand back a cursor asking to be
called again.

`cursor` is opaque, versioned and bound to the query it was issued for. Replaying one under
a different `pattern`, or one from an older protocol version, is a `400` rather than a page
spliced out of two traversals.

`limit` defaults to 100 and may not exceed 1000; `0` or `1001` is a `400` naming `limit`.
`$select` is refused with a `400` naming `$select`: each read surface has one fixed field
set, and answering a caller that asked for one field with the whole set would hand back
bytes it did not ask for.

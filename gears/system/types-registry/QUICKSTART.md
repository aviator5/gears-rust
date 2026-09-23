# Types Registry - Quickstart

Stores versioned GTS Type Schemas and Instances, validates dependencies and
materializes schemas during admission.

Features:

- Asynchronous writes: submit, then poll per-entity outcomes
- Optimistic concurrency via `expected_resource_version`
- Single/batch deletion, blocked by live direct registered dependants
- Dry run on every mutation; entity state stays unchanged
- Required `Idempotency-Key`; replay returns the same operation, changed content conflicts
- Reads by GTS identifier or Registry Reference UUID, including tombstones
- Batch reads with one explicit result per key, absence included
- Bounded discovery with an opaque cursor, filtered by `pattern`, `depth` and `kind`
- `$select` on every read: a document-free default, documents only when asked for

Full API documentation: <http://127.0.0.1:8087/cf/docs>

## Surfaces

| Base path | What it is |
|---|---|
| `/types-registry/v1` | Legacy in-memory API; removed after consumer migration |
| `/types-registry/v2` | Database-backed async API below; promoted to `/v1` after migration |

`/v2` manages global platform entities. Routes are internal (`exposed = false`)
but appear in `/cf/docs`. External mutation access requires platform
authentication and PDP authorization before dispatch.

Use the gear's internal base URL:

```bash
BASE="$TYPES_REGISTRY_INTERNAL"
```

## Examples

Examples use `cf`, the only vendor allowed by default.

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

The **202 Accepted** response includes `Location`, `Retry-After: 1` and the operation ID:

```bash
OPERATION_ID=$(printf '%s' "$RECEIPT" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["operation_id"])')
```

Follow the `Location`:

```bash
curl -s "$BASE/types-registry/v2/operations/$OPERATION_ID" | python3 -m json.tool
```

The completed response contains a `succeeded` item at `resource_version: 1`.

`completed` means every item is terminal. Read the entity:

```bash
curl -s "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~" \
  | python3 -m json.tool
```

```json
{
    "gts_id": "gts.cf.core.example.event.v1~",
    "gts_uuid": "d226dd5b-14c8-56da-a718-9cf29becaba1",
    "kind": "type_schema",
    "origin": {
        "type": "managed",
        "resource_version": 1,
        "created_at": "2026-09-15T09:15:30Z",
        "updated_at": "2026-09-15T09:15:30Z"
    },
    "lifecycle_status": "active",
    "content_hash": "3a1f0c9d4b2e8a71"
}
```

### Select fields

Every read returns this document-free default unless `$select` names fields. Documents
are selected individually and returned flat: `content` (either kind), `resolved_schema`,
`effective_traits`, `effective_traits_schema` (Type Schemas only; absent on an Instance),
plus the `provenance` group (`gts_spec_version`, `gts_impl_version`, `owning_gear`,
`compat_forced`):

```bash
curl -s "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~?\$select=content,effective_traits" \
  | python3 -m json.tool
```

Names are case-insensitive and order does not matter. `lifecycle_status` is always
returned. An unselected field is omitted; a selected document that is JSON `null` stays
`null`. `content_hash` is a non-cryptographic prefilter, not proof two documents are
equal. An empty, duplicate, unknown or nested name (`content.title`) is a `400` naming
`$select`, and any other query parameter is refused.

### Rehearse a deletion, then perform it

Dry run persists a pollable prediction without changing entities:

```bash
curl -s -X DELETE \
  "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~?expected_resource_version=1&dry_run=true" \
  -H "Idempotency-Key: rehearse-delete-1"
```

To commit, omit `dry_run` and use a new key; reusing the dry-run key returns `409`:

```bash
curl -s -X DELETE \
  "$BASE/types-registry/v2/entities/gts.cf.core.example.event.v1~?expected_resource_version=1" \
  -H "Idempotency-Key: delete-1"
```

The tombstone remains readable with an incremented version. Same-key replay returns
**200** and `Idempotency-Replayed: true`.

Deletion requires a positive `expected_resource_version`. Invalid values return
`400`; mismatches become asynchronous `precondition_failed` outcomes. `If-Match`
is rejected.

Batch deletion accepts either key form and returns GTS-ID outcomes in request order:

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

Unknown Registry References return `404`; absent GTS IDs fail asynchronously.

`dry_run` defaults to `false`: a body field for batches and query parameter for single deletion.

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
        { "key": "gts.cf.core.example.event.v1~", "status": "found", "entity": { "...": "the default fields" } },
        { "key": "d226dd5b-14c8-56da-a718-9cf29becaba1", "status": "found", "entity": { "...": "..." } },
        { "key": "gts.cf.core.example.missing.v1~", "status": "not_found" }
    ]
}
```

Results come back in request order, each echoing the key it was asked by, so a caller that
mixed identifiers and Registry References matches answers to questions without re-deriving
either. An absent key is `not_found` inside a `200`, not a `404`: one missing key must not
lose the answers for the others.

A top-level `"$select"` applies to every key and follows the exact read's rules, so a
`found` entity is exactly what `GET /entities/{key}` returns for the same selection:

```bash
curl -s -X POST "$BASE/types-registry/v2/entities:batchGet" \
  -H "Content-Type: application/json" \
  -d '{ "$select": "gts_id,content", "items": [{ "key": "gts.cf.core.example.event.v1~" }] }'
```

`$select` in the query string is refused on this route, as are unknown body fields.

A key named twice collapses onto its first mention. The two spellings of one entity are two
keys and get two results. At most 100 keys per request; an empty `items` is a `400`. The
key count does not bound response bytes once documents are selected.

`If-None-Match` is refused rather than ignored, because validators are per key: each item
carries its own `if_none_match` slot.

### Discover what exists

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
            "origin": { "type": "managed", "resource_version": 1, "created_at": "...", "updated_at": "..." },
            "lifecycle_status": "active",
            "content_hash": "3a1f0c9d4b2e8a71"
        }
    ],
    "page_info": { "next_cursor": "eyJ2IjoxLCJrIjpb...", "limit": 2 }
}
```

Page items take `$select` exactly as the exact read does; the default is document-free and
a page never carries a validator. Select documents on the page directly:

```bash
curl -s "$BASE/types-registry/v2/entities?limit=20&pattern=gts.cf.core.*&\$select=gts_id,content" \
  | python3 -m json.tool
```

Or page identifiers first and read documents for the keys you pick through `:batchGet`,
in batches of at most 100 keys. Keep the same `$select` on every
continuation and stop only when `next_cursor` is absent; a short page may still carry one:

```bash
# Page, collecting identifiers until next_cursor is absent.
CURSOR=""
IDS=""
while :; do
  URL="$BASE/types-registry/v2/entities?limit=100&\$select=gts_id"
  [ -z "$CURSOR" ] || URL="$URL&cursor=$CURSOR"
  PAGE=$(curl -s "$URL")
  IDS="$IDS $(echo "$PAGE" | python3 -c 'import json,sys
for item in json.load(sys.stdin)["items"]: print(item["gts_id"])')"
  CURSOR=$(echo "$PAGE" | python3 -c 'import json,sys
print(json.load(sys.stdin)["page_info"].get("next_cursor") or "")')
  [ -n "$CURSOR" ] || break
done

# Hydrate content, at most 100 keys per batchGet.
echo "$IDS" | python3 -c 'import json,sys
keys = sys.stdin.read().split()
for i in range(0, len(keys), 100):
    print(json.dumps({"$select": "gts_id,content",
                      "items": [{"key": k} for k in keys[i:i + 100]]}))' \
  | while read -r BODY; do
      curl -s -X POST "$BASE/types-registry/v2/entities:batchGet" \
        -H "Content-Type: application/json" -d "$BODY"
    done
```

The traversal ends when `page_info.next_cursor` is **absent** — not when a page comes back
short. One page is bounded in work as well as in results, so a selective `pattern` over a
large table can legitimately return nothing and still hand back a cursor asking to be
called again.

### Filter by chain depth and kind

`depth` is an **inclusive maximum number of GTS identifier segments**. A one-segment root
such as `gts.cf.core.example.event.v1~` has depth 1; a Type Schema derived from it, or an
Instance of it (`gts.cf.core.example.event.v1~cf.core.example.first.v1`), has depth 2, and
each further tail adds one. So `depth=2` returns depth 1 and depth 2 alike. `kind` is
`type_schema` or `instance`. Both work with or without `pattern`, and all filters apply
before the page limit:

```bash
# Instances directly of one root, not of the types derived from it.
curl -s "$BASE/types-registry/v2/entities?pattern=gts.cf.core.example.event.v1~*&depth=2&kind=instance" \
  | python3 -m json.tool
```

`depth` must be an integer from 1 to 255; `0`, negative, fractional or larger values, an
unknown `kind`, and the v1 spelling `is_schema` are `400`. A filtered page may come back
empty and still carry a `next_cursor`: follow it until the cursor is absent.

`cursor` (alias `$skiptoken`) is opaque, versioned and bound to the `pattern`, `depth`,
`kind` and normalized `$select` it was issued for. Resuming under any of them changed —
including adding or dropping a filter — or with a token of an unknown version, is a `400`
rather than a page spliced out of two traversals. An absent `$select` and the explicit
default set are the same selection.

`limit` (alias `$top`) defaults to 50 and may not exceed 100; `0` or `101` is a `400`.
A caller selecting documents should page smaller. `$filter`, `$orderby`, `$skip`, v1
filters such as `is_schema` or `vendor`, and any other undeclared parameter are refused.

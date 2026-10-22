# Plane separation scenarios

End-to-end scenarios for the two route sets through the real api-gateway (D17, D20): the platform API under `/types-registry/platform/v1/` admits a validated `X-ToolKit-Internal-Token` only, and the tenant reads under `/types-registry/v2/` admit a validated bearer only. The gateway validates the token with its `internal_auth` shared secret from `config/e2e-local.yaml`. Every refusal is a canonical `401` Problem whose `context.reason` names it.

Every scenario has an e2e test in [test_planes.py](../test_planes.py). The full matrix, forged credentials included, runs in Rust against the `OoP` listener stack (`types-registry/tests/platform_routes_auth_test.rs`).

## Contents

- [Plane separation](#plane-separation)
  - [TR-PLANE-001 — A platform route serves the internal token and refuses a bearer](#tr-plane-001--a-platform-route-serves-the-internal-token-and-refuses-a-bearer)
  - [TR-PLANE-002 — Both credentials on one request are refused on either route set](#tr-plane-002--both-credentials-on-one-request-are-refused-on-either-route-set)
  - [TR-PLANE-003 — A tenant reads with its bearer and cannot mutate](#tr-plane-003--a-tenant-reads-with-its-bearer-and-cannot-mutate)

## Plane separation

### TR-PLANE-001 — A platform route serves the internal token and refuses a bearer

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** nothing is registered under the test's namespace.

**When:** dry-run `person_schema` through `POST /types-registry/platform/v1/entities`, once with no credential, once with the tenant bearer alone, once with a forged internal token and once with the configured internal token.

**Then:** the first three are `401` with reasons `MISSING_INTERNAL_TOKEN`, `MISSING_INTERNAL_TOKEN` (the bearer is dropped unvalidated) and `INTERNAL_AUTH_FAILED`; the last is a `202` receipt.

### TR-PLANE-002 — Both credentials on one request are refused on either route set

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** `person_schema` is registered.

**When:** read it through `GET /types-registry/platform/v1/entities/{gts_id}` and through `GET /types-registry/v2/entities/{gts_id}`, each request carrying both the tenant bearer and the internal token.

**Then:** both are `401` with reason `MIXED_CREDENTIALS`, refused before either credential is validated.

### TR-PLANE-003 — A tenant reads with its bearer and cannot mutate

**Fixtures:** [person_schema](../fixtures/registration/person_schema.json).

**Given:** `person_schema` is registered through the platform API.

**When:** the tenant reads it through `GET /types-registry/v2/entities/{gts_id}` with its bearer, then tries the same read with the internal token alone, then tries to register through `POST /types-registry/v2/entities` with its bearer.

**Then:** the bearer read is `200` with the entity and equals the platform read; the token-only tenant read is `401`; the tenant registration is not served (`404` or `405`), and the entity still has `resource_version` 1.

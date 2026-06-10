---
status: accepted
date: 2026-04-14
decision-makers: Cyber Fabric Architects Committee
---

# Use mTLS with Platform CA for Inter-Module Authentication

**ID**: `cpt-cf-platform-authn-adr-mtls-platform-ca`

## Context and Problem Statement

Cyber Fabric modules can run as separate processes (Out-of-Process / OoP modules), both managed (spawned by the orchestrator) and unmanaged (e.g., Kubernetes pods). These platform modules need to authenticate to system services (types-registry, directory service, etc.) to perform platform-level operations such as registering global GTS entities.

The existing tenant-scoped authentication (AuthN Resolver + Bearer tokens) is unsuitable for this purpose:

1. **Forest tenant topology** — there is no single root tenant; the tenant model is a forest of independent trees ([TENANT_MODEL.md](../../authorization/TENANT_MODEL.md))
2. **`subject_tenant_id` is mandatory** — every `SecurityContext` requires a tenant, but global platform operations are tenant-less
3. **External IdP dependency** — S2S Client Credentials Grant requires an external IdP, which may not be available during platform bootstrap or in air-gapped environments
4. **Different trust model** — platform modules are trusted components, not external callers; they need a platform-level identity, not a tenant-scoped one

**Key question:** How should platform modules authenticate to each other over the network, independent of tenant-scoped identity?

## Decision Drivers

- **Works for both managed and unmanaged OoP modules** — managed modules are spawned by the orchestrator; unmanaged modules run externally (k8s, systemd, etc.)
- **No external infrastructure required for on-prem** — air-gapped / single-binary deployments must work without external PKI or IdP
- **Kubernetes-compatible** — must integrate with k8s cert-manager and standard cloud-native patterns
- **Module identity** — the system must know which module is calling, not just that the caller is trusted
- **Minimal new dependencies** — prefer leveraging existing crates in the workspace
- **Independent of tenant model** — platform authentication must not depend on tenant hierarchy or SecurityContext

## Considered Options

- **Option A**: mTLS with Platform CA — platform operates its own Certificate Authority; module identity encoded in certificate CN/SAN
- **Option B**: Pre-Shared Key (PSK) / Bootstrap Token — orchestrator generates a secret, passes it to spawned modules via environment variable
- **Option C**: Platform Token Service — dedicated internal service that issues short-lived platform tokens, analogous to AWS STS
- **Option D**: SPIFFE/SPIRE — CNCF standard for workload identity with automatic attestation and certificate rotation

## Decision Outcome

Chosen option: **Option A — mTLS with Platform CA**, because it works for both managed and unmanaged modules, requires no external infrastructure for on-prem deployments, provides cryptographic module identity, and is natively supported by the existing crate stack (rcgen + rustls + tonic).

### Consequences

**Good:**

- Standard X.509 PKI — well-understood, auditable, compatible with any TLS-aware tooling
- Module identity from certificate SAN URI ([ADR-0002](0002-spiffe-uri-module-identity.md)) — no additional identity layer needed
- On-prem: embedded CA via rcgen, zero external dependencies
- Kubernetes: cert-manager issues certs in PEM format, consumed by the same Rust code
- All required crates already in the workspace: `rcgen` (0.13+), `rustls` (0.23), `tonic` (0.14)
- Channel encryption as a side effect — all inter-module gRPC traffic is encrypted
- `tonic::Request::peer_certs()` provides client certificate for authorization decisions

**Neutral:**

- Certificate rotation requires rebuilding `rustls::ServerConfig` / `ClientConfig` (standard pattern with `ArcSwap`)

**Bad:**

- On-prem embedded CA requires secure storage for CA private key
- Certificate distribution to managed OoP modules adds complexity to the spawn flow
- More complex than PSK for simple managed-only deployments

### Why Not the Other Options

**Option B (PSK):** Only works for managed OoP modules (orchestrator must inject the secret at spawn time). Does not work for unmanaged modules (k8s pods, external processes). No cryptographic identity — a PSK proves trust but not _which_ module is calling. No channel encryption.

**Option C (Platform Token Service):** Requires building and operating an additional service. Introduces a bootstrap chicken-and-egg problem (how does the token service itself authenticate?). More complex than mTLS for the same result.

**Option D (SPIFFE/SPIRE):** Requires external infrastructure (SPIRE server + agent on every node). The Rust ecosystem support (`spiffe` crate) has low bus factor (single maintainer). Overkill for the current scale. Note: the SPIFFE ID naming convention is adopted for certificate SANs independently of SPIRE infrastructure — see [ADR-0002](0002-spiffe-uri-module-identity.md).

## References

- [Authorization DESIGN.md — S2S Authentication](../../authorization/DESIGN.md#s2s-authentication-service-to-service)
- [TENANT_MODEL.md — Forest Topology](../../authorization/TENANT_MODEL.md)
- [tonic ServerTlsConfig](https://docs.rs/tonic/latest/tonic/transport/server/struct.ServerTlsConfig.html)
- [rcgen — Rust Certificate Generation](https://docs.rs/rcgen/latest/rcgen/)
- [SPIFFE specification](https://spiffe.io/docs/latest/spiffe-about/overview/)

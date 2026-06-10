---
status: accepted
date: 2026-04-14
decision-makers: Cyber Fabric Architects Committee
---

# Use SPIFFE URI in SAN for Module Identity

**ID**: `cpt-cf-platform-authn-adr-spiffe-uri-module-identity`

## Context and Problem Statement

[ADR-0001](0001-mtls-platform-ca.md) established mTLS with a Platform CA as the inter-module authentication mechanism. The next question: how should module identity be encoded in the X.509 certificate?

X.509 certificates support multiple identity fields:

- **Common Name (CN)** — legacy field in the Subject DN; deprecated for identity matching by RFC 9525 (2024, superseding RFC 6125)
- **Subject Alternative Name (SAN)** — the standard field for identity; supports multiple types: DNS, URI, IP, email

RFC 9525 states that clients MUST NOT use CN for identity matching if SAN is present. Major TLS implementations (rustls, Go crypto/tls since 1.15, browsers since ~2015) follow this — CN is effectively ignored for verification. The Go ecosystem broke many internal certificates when it stopped checking CN.

**Key question:** What SAN type and naming format should encode module identity?

## Decision Drivers

- **Standards compliance** — RFC 9525 requires SAN for identity; CN is deprecated
- **Structured identity** — module name and version must be extractable from the identity string
- **Future interoperability** — should work with service mesh (Istio, Linkerd, Consul Connect) without certificate re-issuance
- **Authorization support** — the format should support pattern-based matching (e.g., "all modules", "all versions of calculator")
- **Simplicity** — avoid defining a custom naming scheme when a standard exists
- **Separation of concerns** — identity naming should not depend on the GTS type system

## Considered Options

- **Option A**: SPIFFE URI in SAN — `spiffe://cyberfabric/module/calculator/v1`
- **Option B**: GTS ID as URI in SAN — `gts://gts.cf.core.module.identity.v1~cf.core.calculator.module.v1~`
- **Option C**: DNS-style SAN — `calculator.v1.module.cyberfabric.local`
- **Option D**: CN only (no SAN) — `calculator.v1` in Subject CN

## Decision Outcome

Chosen option: **Option A — SPIFFE URI in SAN**, because it is an industry standard specifically designed for workload identity, provides structured path-based matching, and ensures compatibility with CNCF service mesh tooling without requiring SPIRE infrastructure.

**Identity format:**

```
spiffe://cyberfabric/module/<module_name>/<version>
```

Examples:
- `spiffe://cyberfabric/module/calculator/v1`
- `spiffe://cyberfabric/module/authn-resolver/v1`
- `spiffe://cyberfabric/module/types-registry/v1`

**Certificate fields:**
- **SAN URI**: `spiffe://cyberfabric/module/<module_name>/<version>` — authoritative identity
- **CN**: not set — SAN is the sole identity source

**Trust domain:** `cyberfabric` is the default trust domain, configurable per deployment. In federated scenarios, each Cyber Fabric installation has its own trust domain.

**Path structure:**

```
spiffe://<trust-domain>/module/<module_name>/<version>
         │                │      │              │
         │                │      │              └─ module version (e.g., v1)
         │                │      └─ module name, kebab-case
         │                └─ fixed prefix for platform modules
         └─ deployment trust domain
```

**Pattern matching for authorization:**
- `spiffe://cyberfabric/module/*` — all modules in this deployment
- `spiffe://cyberfabric/module/calculator/*` — all versions of calculator
- `spiffe://cyberfabric/module/calculator/v1` — exact module identity

### Consequences

**Good:**

- SPIFFE is a CNCF standard — well-defined spec, ecosystem tooling, community support
- Zero-cost interop with Istio/Linkerd/Consul Connect — these tools natively understand SPIFFE URIs in certificate SAN
- Path-based structure enables hierarchical authorization patterns
- No SPIRE required — SPIFFE ID is just a URI format; rcgen can set it as a SAN URI in a standard X.509 cert
- Clean separation: identity (SPIFFE) and type system (GTS) are independent concerns
- Trust domain concept maps naturally to Cyber Fabric deployment identity

**Neutral:**

- Parsing SPIFFE URI from SAN requires extracting the URI-type SAN entry (straightforward with rustls/x509-parser)

**Bad:**

- Introduces a naming scheme that is not GTS — but identity and types serve different purposes
- Trust domain string (`cyberfabric`) must be consistent across all certificates in a deployment — misconfiguration causes auth failures

### Why Not the Other Options

**Option B (GTS ID as URI):** A GTS instance ID (e.g., `gts://gts.cf.core.module.identity.v1~cf.core.calculator.module.v1`) could work well structurally — it carries vendor, package, module, and version in a format the platform already understands, and GTS wildcard matching could be reused for authorization. However, `gts://` is not an industry-standard URI scheme for X.509 certificates. External tooling (service meshes, certificate managers, monitoring, security scanners) does not recognize GTS URIs, which would create integration friction with any system that inspects or matches certificate SANs. SPIFFE provides equivalent structural benefits (path-based hierarchy, pattern matching) while being a recognized CNCF standard.

**Option C (DNS-style SAN):** `calculator.v1.module.cyberfabric.local` looks like a DNS name but is not resolvable. DNS SANs are designed for actual hostnames. Using them for workload identity is a misuse that confuses operators and tools. DNS SANs don't carry trust domain semantics. Service mesh tools don't match on DNS patterns for authorization.

**Option D (CN only):** Deprecated by RFC 9525. rustls and Go crypto/tls prefer SAN and may ignore CN. No structured format — just a flat string. No future compatibility with service mesh. Would require custom verifier code instead of standard SAN matching.

## References

- [ADR-0001: mTLS with Platform CA](0001-mtls-platform-ca.md)
- [SPIFFE ID specification](https://github.com/spiffe/spiffe/blob/main/standards/SPIFFE-ID.md)
- [SPIFFE X.509 SVID specification](https://github.com/spiffe/spiffe/blob/main/standards/X509-SVID.md)
- [RFC 9525 — Service Identity in TLS](https://www.rfc-editor.org/rfc/rfc9525.html)
- [RFC 6125 — Representation and Verification of Application-Layer Identities (obsoleted by 9525)](https://www.rfc-editor.org/rfc/rfc6125.html)

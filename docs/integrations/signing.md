# Integration package signing & verification

**Status:** spec proposed by #71 · **Key custody: OPEN — operator decision required (see last section).**

Packages are **data, not code**, so there is no sandbox to escape — **verification is
the entire security boundary** between the registry and a customer's cluster
(ADR-039, "Trust"). This document pins *what* is signed, *how* (algorithm + a
deterministic canonicalization), and the *verification path* the core enforces. It
deliberately does **not** decide who holds the signing key or where it lives — that
is a business/operations decision flagged to the operator and still open.

---

## 1. What is signed — canonical package digest

A signature over "the files" is only meaningful if two people packaging the same
bytes compute the same thing to sign. YAML key order, tar member order, and
whitespace are all non-deterministic, so we sign a **canonical digest** derived from
content, not from on-disk layout.

**Canonicalization (deterministic, order-independent):**

1. Take every file in the package directory **except** the manifest's own
   `signature` field (below) — i.e. `pipeline.json`, `index-template.json`,
   `saved-objects.ndjson`, `agent.conf.tmpl`, and `manifest.yaml`.
2. For **`manifest.yaml`**: parse the YAML, **remove the `signature` key**,
   serialize the result with **RFC 8785 JSON Canonicalization Scheme (JCS)**
   (sorted keys, fixed number/string form). Hash those bytes. Removing `signature`
   before hashing is what lets the signature live *inside* the manifest without a
   chicken-and-egg (ADR-039 says "the manifest carries a signature"; this honors it
   literally while staying deterministic).
3. For **every other file**: hash the raw bytes as-is (they are opaque to the
   signer — pipeline/template/ndjson/conf).
4. Build the entry list `[[relpath, "sha256:<hex>"], ...]`, **sort by `relpath`
   bytewise ascending**, serialize the list with JCS, and take
   `digest = SHA-256(that)`.

`digest` is the 32 bytes that get signed. It is independent of archive format,
file order, and YAML formatting, so the offline (air-gapped download) and online
(HTTPS pull) delivery routes in ADR-039 produce identical digests — the format
stays transport-agnostic.

**Signature carrier.** The `signature` block is embedded in `manifest.yaml`:

```yaml
signature:
  algorithm: ed25519
  key_id: velox-registry-2026
  value: <base64 ed25519 signature over `digest`>
```

`key_id` selects the public key; `value` is `ed25519_sign(privkey, digest)`.
Because step 2 strips `signature` before hashing, the block can sit in the same
file it helps protect.

---

## 2. Algorithm & key distribution — options

### Option A — ed25519 detached-inline signature (RECOMMENDED)

- 32-byte public keys, 64-byte signatures, no PKI, no certificate chain.
- The core embeds a small **trusted keyring** — a compile-time `const` table of
  `key_id -> ed25519 public key` — so verification needs **no network** and works
  in a fully egress-less install (matches ADR-039's air-gapped buyers). Adding or
  rotating a key is a core release.
- Rust verification is a few lines with `ed25519-dalek` (already common in the
  ecosystem) or the tiny `minisign-verify` crate; no OpenSSL, no GnuPG runtime.
- Tooling: `minisign`/`rsign2` or a 30-line signer are enough to produce `value`.

**Trade-offs:** key rotation ships with a core release (acceptable — the core is
already the release/review gate for everything that touches a cluster). No built-in
revocation beyond "next release drops the key_id"; adequate for a first-party
registry, revisit if third-party publishers are ever allowed.

### Option B — Sigstore / cosign keyless (OIDC + transparency log)

- No long-lived private key to guard; signing identity is an OIDC identity, logged
  in a public transparency log (Rekor). Strong provenance story.
- **Rejected for v1:** verification wants to reach Fulcio/Rekor (or ship and pin
  their roots + trust-bundle updates), which fights the **air-gapped, egress-less**
  requirement that is a first-class buyer profile here (Ministério-Público-class).
  It also pulls in a heavy verification stack for what is a small first-party
  registry. Good candidate to revisit **if** integrations ever open to third-party
  publishers, where its identity model earns its weight.

### Option C — GPG / OpenPGP detached signature

- Familiar, `.asc` detached sigs, established keyservers.
- **Rejected for v1:** OpenPGP is a large, footgun-prone format (key packet
  parsing, subkeys, expiry, cipher agility); pulling a GPG runtime or a full
  OpenPGP Rust stack into the core is a lot of attack surface for a one-line
  "verify 64 bytes" need. ed25519-only (Option A) is the same security with a
  fraction of the surface.

**Recommendation: Option A (ed25519, embedded keyring).** Smallest verifier,
smallest attack surface, offline-native, and it keeps the trust anchor *inside the
reviewed core binary* — which is exactly where ADR-039 already puts all
cluster-touching authority.

---

## 3. Verification path in the core (fail closed)

The engine runs this **before applying any asset** to a customer's cluster. Every
failure is a hard reject — the package is not partially applied.

1. **Parse manifest.** No `manifest.yaml`, or it fails `manifest.schema.json`
   (which requires `signature`) ⇒ **reject (malformed / unsigned)**.
2. **Known key.** `signature.key_id` not in the embedded trusted keyring ⇒
   **reject (unknown key)**.
3. **Recompute digest** per §1 (drop `signature`, JCS-canonicalize manifest, hash
   assets, sorted JCS list, SHA-256).
4. **Verify signature.** `ed25519_verify(pubkey, digest, base64_decode(value))`
   fails ⇒ **reject (bad signature / tampered)**. This catches any edit to any
   asset or manifest field.
5. **Interpolation scan.** Any asset contains a `{token}` outside the closed set
   (see `interpolation.md`) ⇒ **reject (foreign interpolation token)**.
6. **Apply.** Only now: PUT pipeline/template, `_bulk_create` saved objects, deploy
   agent — the existing `recipes::apply` steps, now fed from verified data.

Corresponding rejects, restated as the three states the card names:
*unsigned* → step 1; *mismatched/tampered* → step 4; *unknown-key* → step 2.

**Test obligations (for the engine card):**
- A valid package verifies and applies.
- Flipping one byte in any asset ⇒ step 4 reject (ADR-039's "signature
  verification rejects a tampered package" golden test).
- Stripping the `signature` block ⇒ step 1 reject.
- A signature from a key_id not in the keyring ⇒ step 2 reject.
- Re-ordering YAML keys / re-serializing the manifest ⇒ still verifies (proves the
  canonicalization is order-independent).

---

## 4. OPERATOR DECISION REQUIRED

The mechanism above is complete **except** for who holds the key and how the trust
anchor is governed. These are business/operations calls, not engineering ones, and
are **explicitly left open** for the operator:

1. **Signing-key custody.** Where does the ed25519 **private** key live, and who
   can use it? Candidates, in rough order of assurance:
   `cloud KMS / HSM (non-exportable)` › `hardware token (YubiKey)` ›
   `offline air-gapped signer laptop` › `CI secret in the registry pipeline`
   (weakest — key touches CI). **No default is assumed here.**
2. **Publish authorization.** Is a single signature enough, or does a registry
   publish require **dual control** (two approvers / two keys)?
3. **Rotation & revocation policy.** Cadence for rotating `key_id`; how a
   compromised key is retired. With Option A this is "drop the key_id in the next
   core release" — the operator must accept that release cadence as the revocation
   latency, or ask for a fetched revocation list (added scope).
4. **Single org key vs per-publisher keys.** One first-party key today. If
   third-party publishers are ever allowed, the keyring becomes multi-tenant and
   Option B (Sigstore) should be reconsidered — an operator direction, not a code
   default.
5. **Keyring delivery.** Confirmed-by-default here as **compiled into the core**
   (offline-native). If the operator wants out-of-band keyring updates without a
   core release, that is added scope to design.

Until (1) is answered, the registry can be built and packages can be *shaped* and
*validated*, but no production package can be *signed for release*. The example
`nginx/manifest.yaml` carries a structurally-valid placeholder `value`, not a real
signature, for exactly this reason.

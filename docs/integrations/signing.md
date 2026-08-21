# Integration package signing & verification

**Status:** accepted · **Key custody: the maintainers hold the private half; see [`keys/README.md`](../../keys/README.md).**

Packages are **data, not code**, so there is no sandbox to escape — **verification is
the entire security boundary** between the registry and a customer's cluster
(ADR-039, "Trust"). This document pins *what* is signed, *how* (algorithm + a
deterministic canonicalization), and the *verification path* the core enforces.
Who holds the signing key, and the rotation procedure, are settled in [`keys/README.md`](../../keys/README.md): the
private half is held by the project maintainers, is never in this repository nor
in the container image, and is used only by the registry's publishing tooling.

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

## 4. Key governance

The mechanism above is complete. This section records how the trust anchor is
governed — decisions that are operational, not engineering, and that were open
until the open-source release settled them.

1. **Signing-key custody.** The ed25519 private key is held by the project
   maintainers, outside this repository, outside the container image and
   **outside CI**. Signing is `velox sign`, run on a maintainer's own machine:

   ```sh
   velox sign integrations/<id> --key <key.pem>   # or --key - to read stdin
   velox verify integrations/<id>                 # no key needed
   ```

   `sign` verifies the signature it just produced before writing, so a package
   that would not check out never reaches the disk. It rewrites one line — the
   manifest's `value:` — because the manifest bytes are themselves an input to
   the digest, and because the comments in it are documentation.

   Keeping this step manual is a deliberate trade: packages change a few times a
   year, and automating that would mean a long-lived signing key sitting in CI
   permanently. Rotation is a core release (the keyring is compiled in), so a
   leak is unusually expensive to recover from. A contributor never needs it: proposing an integration
   is a pull request against the registry repository. Custody, and the exact
   format of the public half, are documented in
   [`keys/README.md`](../../keys/README.md).

   The assurance ladder is worth stating, because the current position is not
   the top of it: `cloud KMS / HSM (non-exportable)` › `hardware token` ›
   `offline signer` › `CI secret` (weakest — the key touches CI). Moving up it
   is a change to publishing tooling, not to the verification path, so it can
   happen without a core release.

2. **Publish authorization.** A single maintainer signature. Dual control (two
   approvers, two keys) is not implemented; it would be a change to the registry
   pipeline, not to this spec.

3. **Rotation and revocation.** Rotating or retiring a `key_id` is a **core
   release** — the same review gate as anything else that touches a cluster. The
   procedure, including the overlap window that keeps already-published packages
   verifying, is in [`keys/README.md`](../../keys/README.md).

   The consequence is explicit: **release cadence is the revocation latency.**
   A fetched revocation list would shorten it and is deliberately not
   implemented, because it would put a network dependency inside the one code
   path that must work with no network.

4. **One key, not per-publisher keys.** There is a single first-party key. The
   registry is maintainer-published; third-party publishers are not supported.
   If they ever are, the keyring becomes multi-tenant and a transparency-log
   approach (Sigstore) should be reconsidered against this one.

5. **Keyring delivery: compiled into the core.** Verification works in a fully
   egress-less install and cannot be redirected by a compromised registry.
   Out-of-band keyring updates without a core release are not supported, and
   that is the trade this option was chosen for.

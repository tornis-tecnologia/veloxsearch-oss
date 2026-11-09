# Licence signing keys

The **public** halves of the ed25519 keys that sign Velox Mesh licence
documents. `velox_license::keyring()` compiles these files into the binary,
so a licence verifies with no network, in an air-gapped install, exactly like
the core's registry keyring ([`keys/`](../../keys/README.md)).

| Key id | File | Status | Scope |
| --- | --- | --- | --- |
| `velox-license-2026-11` | `velox-license-2026-11.pub` | active (first issuing key) | Licence files and activation codes for Velox Mesh |

## File format

One line, **base64 of the raw 32-byte ed25519 public key** — the same form
as the registry keys, derived the same way:

```sh
openssl genpkey -algorithm ed25519 -out velox-license-2026-11.priv.pem
openssl pkey -in velox-license-2026-11.priv.pem -pubout -outform DER \
  | tail -c 32 | base64 -w0 > velox-license-2026-11.pub
```

## Custody

The private half is **never** in this repository nor in any image. Its home
is the maintainer vault (a GitHub Actions secret or private object storage,
ADR-054); a licence is issued by a maintainer with key-file access running
the issuing tool (`velox-mesh license issue`, PLAN.md fase 1). The free tier
needs no document at all, and under the AGPL anyone may remove the check
(ADR-068): the signature exists so a *forged* file is detectable in the
official build, not to stop a *removed* check.

## Rotation

Same rule as the registry keyring: add the new `.pub` **alongside** the old
one and add its id to `keyring()`, reissue, and only in a later release drop
the retired key. A customer's licence file predates the new key; dropping the
old one in the same release strands every issued licence.

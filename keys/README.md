# Registry signing keys

This directory holds the **public** halves of the ed25519 keys that sign
VeloxSearch integration packages. The keyring in
[`src/catalog.rs`](../src/catalog.rs) compiles these files into the binary, so
signature verification needs no network and works in a fully egress-less
install (`docs/integrations/signing.md` §2, option A).

| Key id | File | Status | Scope |
| --- | --- | --- | --- |
| `velox-registry-2026` | `velox-registry-2026.pub` | active | Packages published to [`veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry) |

## File format

One line: **base64 of the raw 32-byte ed25519 public key** — not PEM, not DER,
not OpenSSH. `catalog::verify_package` base64-decodes the file and hands the
bytes straight to `aws_lc_rs::signature::ED25519`. To derive this form from a
PEM private key:

```sh
openssl pkey -in velox-registry-2026.priv.pem -pubout -outform DER \
  | tail -c 32 | base64 -w0
```

(The last 32 bytes of an ed25519 SPKI DER blob are the raw key; the preceding
12 bytes are the algorithm header.)

## Custody

The private half is **never** in this repository, never in the container image,
and never in CI. It is held by the project maintainers and used only by
`velox sign`, run on a maintainer's own machine:

```sh
velox sign integrations/<id> --key velox-registry-2026.priv.pem
velox sign integrations/<id> --key -    # from stdin, so it never lands on disk
```

A contributor never needs it: proposing an integration means opening a PR
against the registry repo, and a maintainer signs it there. The registry's CI
refuses to merge a package still carrying the placeholder signature.

Anyone can check a package without any key:

```sh
velox verify integrations/<id>
```

## Rotation

Adding or rotating a key is a **core release**, deliberately — it goes through
the same review gate as anything that touches a cluster, because this keyring
is the entire security boundary for integration packages (packages are data,
not code; see `docs/integrations/signing.md` §3).

The procedure:

1. Generate the new pair; keep the private half in the maintainer vault.
2. Add the new `.pub` here **alongside** the old one and add its id to
   `KEYRING` in `src/catalog.rs`. Both keys verify during the overlap.
3. Re-sign the registry's packages with the new key and publish.
4. In a later release, drop the retired entry from `KEYRING` and delete its
   `.pub`. Packages still signed by it then fail as `UNKNOWN KEY` — which is
   the intended, loud outcome.

Never remove an old key in the same release that adds its replacement: that
strands every already-published package between the two rollouts.

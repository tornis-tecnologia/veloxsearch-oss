# ADR-054 — Registry and signing live on GitHub and Docker Hub

**Status:** accepted (supersedes the pre-OSS internal-registry "buffer" plan,
unpublished; supersedes ADR-039's registry-hosting and key-custody provisions)
**Date:** 2026-08-24

## Context

Before the open-source release there was no public home for artifacts: the
project predated any Docker Hub presence, and the plan for integration
packages was an internal registry "buffer" — a MinIO-style store on a rented
Contabo host. That infrastructure is gone. The 2026-08-20 alignment meeting
settled the opposite model: everything a customer or contributor needs lives
on public forges, and the only secret left is one ed25519 private key, held
outside the repository. This also closes what ADR-039 left open — where the
registry lives and who holds the key (`docs/integrations/signing.md` §4 called
these "operational, not engineering").

## Decision

- The container image publishes to Docker Hub:
  `docker.io/tornistecnologia/veloxsearch-oss`. The Contabo buffer is retired
  and never shipped.
- The signed integration-package registry is the public GitHub repository
  [`tornis-tecnologia/veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry).
- The image is signed **cosign keyless** (OIDC identity, Rekor log) — no
  long-lived image-signing key exists to leak. Package signing stays ed25519
  against the keyring compiled into the core (ADR-039's verification boundary,
  unchanged).
- A **new** signing key pair (key id `velox-registry-2026`) was generated for
  this model; the pre-OSS laptop-held key material is retired. The private
  half lives **outside this repository** — a GitHub Actions secret or private
  object storage, per the 2026-08-20 alignment — and is never committed.
- Release is `.github/workflows/release.yml` (merged in PR #15): a merge to
  `main` that bumps `version` in `Cargo.toml` builds, pushes, signs the image
  and cuts the release. Nothing publishes on any other trigger.

## Consequences

- No registry infrastructure to operate: the package registry is a git repo
  (reviews, history and mirroring come free), and an air-gapped buyer clones
  it — ADR-039's transport-agnostic format is what makes this work.
- A long-lived ed25519 secret available to the publish path sits on the weaker
  rungs of the assurance ladder in `signing.md` §4 (`cloud KMS/HSM › hardware
  token › offline signer › CI secret`). Accepted knowingly: rotation is a core
  release and the overlap procedure in `keys/README.md` already exists.
  `signing.md` §4 and `keys/README.md` are reconciled to this in the same
  change that adds this ADR.
- Image verification (cosign/Rekor) needs egress — acceptable, because
  pulling from Docker Hub already does; package verification stays offline.
- Docker Hub is now in the pull path: rate limits and account health are a
  release dependency we do not control (mitigated by the side-load path,
  ADR-025).

## Alternatives considered

- **Internal registry buffer on Contabo (the old plan).** Lost: running
  stateful infrastructure to buffer for a publisher that now exists solves
  nothing; the host is decommissioned; a public git repo reviews better.
- **Manual signing on a maintainer laptop** (as first documented with the
  signer in PR #2/#15). Lost: slow, single-person-bound, and laptop-held
  material already had to be retired once; a secret store is auditable and
  revocable.
- **Cosign keyless for packages too.** Rejected in `signing.md` §2 option B:
  package verification must work in a fully egress-less install.
- **GitLab registry (pre-OSS status quo).** Lost with the move to GitHub —
  one forge now holds code, packages and releases.

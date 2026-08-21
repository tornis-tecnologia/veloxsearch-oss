# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project is pre-1.0: minor versions may carry breaking changes, and those
are called out explicitly.

## [Unreleased]

### Added
- `README.es.md` — a Spanish README, at full parity with the English one (same
  sections, tables and examples). The three READMEs cross-link. Note that the
  UI itself is still pt + en: the Spanish README says so rather than implying
  otherwise, and points at `frontend/i18n.jsx` as the self-contained way in.
- `velox sign` and `velox verify`, plus `catalog::sign_package`. Signing a
  package had no supported procedure at all: the packages in the registry were
  signed by a throwaway test that no longer exists. `sign` verifies before it
  writes, so a package that would not check out never reaches the disk, and
  `--key -` reads the key from stdin so it need not touch it either.
- `release.yml` — merging a version bump on `main` now publishes: re-verify at
  the release commit, build and push the image, sign it with cosign **keyless**
  (no key to leak or rotate), then tag and publish a release with a
  digest-pinned `install.yaml` attached. Nothing in the pipeline writes to
  `main`.
- `registry-sync.yml` — a recipe change on `main` regenerates the registry's
  package assets and opens a pull request there, unsigned. Drift between the
  core's recipes and the registry was previously only ever *detected*.
- Open-source project structure: `LICENSE` (AGPL-3.0-only), `NOTICE`,
  `CONTRIBUTING`, `CODE_OF_CONDUCT`, `SECURITY`, `GOVERNANCE`, this changelog,
  and bilingual (en / pt-BR) README and contributing guides.
- GitHub Actions CI: formatting, clippy, tests against a live Postgres, MSRV
  check, frontend build, container-image build, `cargo deny` supply-chain gate,
  and a mechanical SPDX-header check.
- Issue and pull-request templates, `CODEOWNERS`, and Dependabot for cargo, npm,
  GitHub Actions and Docker.
- `keys/velox-registry-2026.pub` and `keys/README.md` — the public half of the
  integration-package signing key, with its custody and rotation policy. The
  crate previously failed to build without this file.
- `deploy/build-image.sh` — the canonical image build, with an optional
  `--push`. `deploy/build-image-local.sh` now delegates to it.
- `docs/ARCHITECTURE.md`, `docs/DEVELOPMENT.md`, `docs/DEPLOY.md`,
  `docs/SECRETS.md`, `docs/INSTALLER.md`, `docs/ROADMAP.md`, `docs/adr/` and
  `tests/README.md` — documents the code already referenced but that were never
  exported.

### Changed
- **Breaking (operators):** the install manifest to apply is now the release
  artifact, `releases/latest/download/install.yaml`, with the image pinned to a
  **digest**. `deploy/install.yaml` on `main` keeps a version tag and is the
  source the release is built from — applying it gives you whatever is on HEAD.
  `https://get.veloxsearch.ai/install.yml` is maintained by hand outside this
  repository and can lag behind the current release.
- **Breaking (operators):** the default image moved from
  `docker.io/ricardodacosta/veloxsearch:latest` to
  `docker.io/tornistecnologia/veloxsearch-oss:<version>`, and
  `imagePullPolicy` is now `IfNotPresent` against a pinned version tag instead
  of `Always` against `:latest`. Existing installs keep running; re-apply
  `deploy/install.yaml` to move.
- **Breaking (operators):** the default integration registry moved from a
  private GitLab repository to the public
  [`tornis-tecnologia/veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry).
  `VELOX_REGISTRY_TOKEN` is no longer needed for the default catalog.
- The integration-package signing key was rotated to a key generated for the
  open-source release. Packages signed with the previous key no longer verify;
  the public registry ships re-signed packages.
- `velox init --registry` now defaults to `docker.io` instead of
  `registry.gitlab.com`.
- Development moved to GitHub. This repository is the source of truth; it is no
  longer a one-way export and `main` is no longer force-pushed.

### Removed
- `docs/DECISIONS.md` — the full ADR log was exported by mistake; `docs/INSTALL.md`
  in the same snapshot said it was withheld "pending a redaction pass because it
  carries live client infrastructure detail", and it does. `docs/adr/README.md`
  carries what each ADR decided, with none of that detail.
- `style/main.scss` — dead since the Leptos-to-React migration, referenced by
  nothing.
- Internal operational content that was never meant to be published: a
  production-cluster Longhorn runbook (replaced by a generic operator example),
  a self-hosted CI-runner secret layout, and internal conformance-fleet
  hostnames.

### Fixed
- `ring` was being linked as a second rustls crypto provider beside `aws-lc-rs`,
  contradicting what `Cargo.toml` claimed in three places: `reqwest`'s
  `rustls-tls` feature resolves to `__rustls-ring`. Switched to
  `rustls-tls-webpki-roots-no-provider`, which keeps the same root store and
  defers to the provider `main.rs` and `velox` install explicitly. Drops `ring`,
  `quinn`, `quinn-proto`, `quinn-udp` and four more crates from the image.
- `rustls-pemfile` is unmaintained and archived (RUSTSEC-2025-0134). The CA
  parsing in the LDAP probe now uses `rustls-pki-types`' `PemObject` — the
  maintained home of the same parser, and already in the tree.
- `anyhow` bumped to 1.0.104 for RUSTSEC-2026-0190 (unsoundness in
  `Error::downcast_mut`, which this codebase never calls).
- The declared MSRV was wrong: the dependency tree requires 1.88, not 1.85.
  Clippy's MSRV lint only checks this crate's own API use, so only the `msrv`
  CI job catches it.
- Documentation links that pointed at files absent from the export
  (`DEPLOY.md`, `deploy/build-image.sh`, `docs/SECRETS.md`,
  `docs/INSTALLER.md`, `spec/signing.md`).
- SPDX license headers were missing from nine source files; the `headers` CI job
  now prevents recurrence.

## [0.7.0] - 2026-08-20

First public snapshot. The control plane provisions and operates OpenSearch
deployments on k3s, k0s, minikube and kubeadm: first-run conformity gate and
self-bootstrap, capacity-aware sizing, day-2 operations (snapshots, upgrades,
LDAP/OIDC providers), signed integration packages, and an OpenTelemetry
collection stack.

[Unreleased]: https://github.com/tornis-tecnologia/veloxsearch-oss/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/tornis-tecnologia/veloxsearch-oss/releases/tag/v0.7.0

# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project is pre-1.0: minor versions may carry breaking changes, and those
are called out explicitly.

## [Unreleased]

### Added
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
- `style/main.scss` — dead since the Leptos-to-React migration, referenced by
  nothing.
- Internal operational content that was never meant to be published: a
  production-cluster Longhorn runbook (replaced by a generic operator example),
  a self-hosted CI-runner secret layout, and internal conformance-fleet
  hostnames.

### Fixed
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

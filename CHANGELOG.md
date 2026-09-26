# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This project is pre-1.0: minor versions may carry breaking changes, and those
are called out explicitly.

## [Unreleased]

### Fixed
- **The Dashboards first-boot fix now sticks (#46):** the 0.10.5 run showed
  the operator reverts the `Recreate` strategy and startup budget on its
  Deployment within a second, and it reverted the remediation's 0/1 scale the
  same way. The operator's CR has no probe or strategy field, so the fix now
  goes through `spec.dashboards.replicas`. A new deployment's Dashboards is
  held at zero replicas until the cluster is initialized and green (or 20
  minutes old), so its first migration runs on a settled cluster. The
  `.kibana_1` remediation holds and releases through the CR too. The
  Deployment patch is gone, and so is the runtime `patch` grant on
  Deployments (ADR-063).

## [0.10.5] - 2026-09-24

### Changed
- `velox init --pull-token` is deprecated and now prints a deprecation
  warning (the pull Secret it creates has no effect on the default catalog);
  every doc that taught the flag now leads with the manual `velox-pull`
  Secret procedure, canonical in SECRETS.md (#74).

### Fixed
- A rolling restart wedged on a recovery stuck in `init` now actually gets
  remediated on fresh installs. The #27/#96 remediation armed, but its pod
  bounce was forbidden: the runtime RBAC had no pod `delete`, and only
  installs still holding the bootstrap cluster-admin binding could bounce.
  The grant is namespaced to `veloxsearch-system`, never cluster-wide.
- The Dashboards survivability patch (#46) lands again. Setting `Recreate`
  by server-side apply collided with the API server's defaulted
  `rollingUpdate`, and the whole patch was rejected, including the
  30-minute startup-probe budget. The strategy now goes through a merge
  patch that removes `rollingUpdate`.
- `tests/journey_check.py` addresses detail tabs inside the deployment's own
  `nav.tabs`, and `tests/day2_check.py` expects the #80 anti-enumeration
  refusal (`deployment not found`).

## [0.10.4] - 2026-09-21

### Added
- Stalled deployments now show the real blocker on the stalled view: a
  probe-kill loop (the kubelet restarting the container on failed startup
  probes) surfaces as a first-class fact with per-pod restart counts, the
  last terminated reason and exit code — read from the pod object the app
  already lists, no new RBAC and no logs (#97).

### Fixed
- Rolling restarts no longer wedge on security-index peer recovery stuck in
  `init` — the #27 remediation now covers restart waves (#96). While the
  operator's `RollingRestart` is in progress, a recovery sitting in `init` at
  0 bytes past 10 minutes arms the same proven remediation (transient
  `node_concurrent_recoveries` raise + bounce of the one node holding the
  wedged shard), and the raised setting is handed back once the recoveries
  drain. The stalled banner already names the recovery and the bounced pod.
- Storage usage on the deployment Overview shows the actual OpenSearch data
  size and marks capacity as not enforced under node-local provisioners
  (local-path), instead of summing each node's whole root disk against the
  PVC requests and printing figures like 164% (#95).

## [0.10.3] - 2026-09-20

### Changed
- **Deployment storage is now flexible: Longhorn when present, otherwise the
  cluster's default StorageClass** — node-local defaults create with a
  durability warning instead of refusing; only a fully StorageClass-less
  cluster triggers the Longhorn bootstrap (ADR-043 amended, #88).
- install.yaml ships `service/veloxsearch` as NodePort 30080 — the UI answers
  at `http://<node-ip>:30080` on any cluster, with or without an Ingress
  controller (#87).

### Fixed
- Bundled postgres PVC uses the cluster default StorageClass instead of
  pinning `longhorn` — postgres-0 no longer sits Pending on fresh clusters
  before bootstrap (#86).

## [0.10.2] - 2026-09-16

### Fixed
- **Deployment routes follow the stored access config on startup:** existing
  deployments only got their Ingresses when Settings → Access was saved, so a
  `veloxsearch-config` ConfigMap restored after a cluster rebuild, applied by
  GitOps or edited with kubectl left every deployment on port-forward until
  someone pressed Save on an unchanged form. The app now runs the same
  backfill once at startup (best-effort, never blocks serving), and the
  Settings save path shares that code.

## [0.10.1] - 2026-09-16

### Fixed
- **Cluster-health series keep the newest samples (#65):** the raw-sample
  query sorted ascending under a 10,000-hit cap, so a window holding more
  samples than that returned the oldest page and dropped the newest — a full
  7-day window at the default 60s cadence lost its last ~80 minutes. The query
  now pages newest-first; the charted series is unchanged below the cap.
- **Release tags are created on the released commit:** the release job did
  not name a target commit, so GitHub created the tag on the default branch
  (`develop` since #72) and `v0.10.0` points at `541844f` instead of the
  `main` commit `c7495d0` it was built from. The two trees are identical, so
  the released content is unaffected; the tag cannot be moved because release
  tags are protected. The job now tags `github.sha`.

## [0.10.0] - 2026-09-15

### Added
- **Which build is serving, in the app (#55):** a new admin-only
  `GET /api/build_info` returns the version and git commit compiled into the
  binary. No env var on the Deployment can change them, and a local build
  without a commit reports "commit unknown". It also returns the image digest
  the kubelet reports for the app's own Pod, the operator image, and the
  integration catalog source. Settings has an "About this installation"
  block, with a short digest and a copy button, and the top bar shows a
  `v<version> · <commit>` chip. Release images get the commit through
  `VELOX_BUILD_COMMIT`. `install.yaml` now passes `POD_NAME` through the
  downward API, used only to locate the Pod. `min_core_version` refusals
  quote the same version string, and the smoke lane asserts that
  `build_info.version` matches the release it installed (#76).
- **An install report form and an external validation brief (#60):** a new
  "Install report" issue form captures distribution, Kubernetes version,
  resources, the commands run, time to a first green deployment and every
  point of friction. `docs/EXTERNAL-VALIDATION.md` (and `.pt-BR.md`) is the
  brief for first-time installers working from the README alone, with a
  separate section for security review of the published surfaces.
- **Three proposed ADRs, documentation only (#57, #58, #59):** ADR-058
  (multiple integration catalog sources with per-source pinned keys), ADR-059
  (collector configuration for sources outside the cluster) and ADR-060
  (a bounded, pseudonymised cluster profile export). Nothing in this release
  implements them.

### Changed
- **K3S monitoring is a choice, not a baseline (#52):** every non-search
  deployment used to ship the `kubernetes` monitor unconditionally — the
  wizard's data-sources step was gone, `sources` was hardcoded on, and the
  create path re-seeded it on any empty selection. The Review step now shows
  a default-checked toggle ("K3S monitoring (this cluster)"), and an empty
  monitor selection — unchecking it, or an API create with no monitors — is
  respected verbatim: the deployment comes up with no collector, the
  Integrations tab remains the enable path, and the overview says
  "no monitors installed" honestly (#47).
- **The READMEs lead with the install command (#61):** what VeloxSearch is,
  the AGPL in plain words, the single `kubectl apply`, a "starting from zero"
  path (new `docs/INSTALL.md` §0), four static screenshots in place of the
  demo GIF (now referenced from INSTALL.md), benefits, the roadmap and the
  demo-request link — in all three languages.
- **`develop` is the integration branch (#72):** feature PRs now target
  `develop`, and every push there runs the full CI set including the Smoke
  (minikube) job; `main` is release-only and a promotion that changes
  `version` in `Cargo.toml` publishes. Contributor-facing only — see
  `CONTRIBUTING.md`.

### Fixed
- **A double-clicked "Create cluster" could create two deployments (#56):**
  the wizard only disabled its submit button when the create was going to
  install Longhorn, so on the common path the button stayed live while the
  request was out. Every create generates a fresh `<name>-<suffix>`, so a
  second click was not a re-apply — it was a second deployment with its own
  deferred provisioning. The button now disables from the first click and
  shows "Submitting…" with a spinner, re-arming if the create fails, and the
  backend refuses a create of the same name in the same namespace with 409
  while an earlier one is still being handled. `tests/create_submit_check.py`
  proves the button half without a cluster; `tests/journey_check.py` now
  submits with a double click and asserts one deployment.
- **Install docs described pre-0.9.0 behaviour (#61):** `docs/INSTALL.md`
  still said a foreign CSI default StorageClass is used as-is (ADR-043 made
  Longhorn the only deployment storage), that the manifest creates no
  Ingress, and that its ServiceAccount carries `imagePullSecrets` — it does
  not, so the documented private-mirror steps never used the pull Secret;
  they now patch the ServiceAccount. The side-load tag matches the manifest's
  image reference, the create wizard is described as its four steps with the
  optional K3S monitoring toggle, and `docs/INSTALLER.md` covers the released
  `velox-linux-amd64` binary and what `--dry-run` really prints.
- **`deploy/install.yaml` in the source tree still pinned 0.8.1:** the
  published release asset was always digest-pinned to the right image, but
  applying the manifest from a checkout installed 0.8.1. The tag now matches
  the crate version (part of #54).

### Security
- **rustls 0.23.45 for RUSTSEC-2026-0285 (#71):** rustls before 0.23.45
  accepted TLS 1.3 handshake messages across encryption-level boundaries.
  Lock-only bump (with `rustls-webpki` 0.103.15); no code change.
- **Registry credentials no longer follow cross-origin redirects (#75):** the
  catalog client sends the registry token in a custom `PRIVATE-TOKEN` header,
  which reqwest does not strip when a redirect changes host. Credential-bearing
  catalog fetches now follow redirects only within the same origin (host and
  effective port, no scheme downgrade) and refuse anything else with an
  explicit error. The default public registry does not redirect, so nothing
  changes there.

## [0.9.0] - 2026-09-10

### Fixed
- **A Dashboards crash-loop could hold a create hostage forever (#46):** the
  operator's hardcoded ~210s startup probe killed the first saved-objects
  migration mid-flight while the cluster was still settling, and the
  half-migrated `.kibana_1` deadlocked every restart (observed live: 235
  restarts over 17 hours). Creates now patch the Dashboards Deployment for
  survivability — a 30-minute startup budget and a `Recreate` strategy so a
  stuck rollout can never race two pods against the same migration — and a
  stall on the dashboards rung self-heals once, cooldown-guarded, by scaling
  to zero, deleting the dead `.kibana_1`, and scaling back (ADR-055; safe by
  construction: a Deployment that never served cannot have user objects).
  The stall panel now also states the Dashboards restart count and waiting
  reason while the rung is held.
- **`dashboards_ready` looked in the app's own namespace (#46):** a
  per-tenant deployment's Dashboards Deployment was never found, so the rung
  could silently never hold. It now reads the CR's namespace.

### Fixed (provisioning)
- **An exhausted provisioning schedule never retried itself when the
  dependency healed (#47):** deferred provisioning that exhausted its five
  attempts while Dashboards was unreachable stayed dead after Dashboards
  recovered — the deployment reported nothing wrong, the monitors were simply
  never installed (observed live: a full demo day). The metrics sampler now
  re-arms exactly one fresh wave when an exhausted record still owes work and
  the dependency is healthy again, bounded by the same CR counter as a human
  retry (ADR-056). A spent schedule is also now a visible banner on the
  deployment page — with the last error verbatim and a "re-apply now" button
  riding the SSE frames — instead of one ERROR log line.
- **The overview's "receiving" claimed self-telemetry as ingestion (#47):**
  the tile derived it from the `velox-metrics` indexing rate, which flows on
  a cluster with zero monitor data. It now derives from a bulk doc count over
  the deployment's monitor indices (carried on `metrics_series` at its
  existing 10s poll — the SSE stream gains no per-frame REST calls), and a
  deployment with no monitors says "no monitors installed" instead of
  claiming anything.

## [0.8.1] - 2026-08-27

### Fixed
- **Single-node and small clusters could never go green (#26):** the vendored
  Longhorn bundle pins three replicas per volume, unschedulable on a cluster
  with fewer nodes — every volume sat `faulted`, every deployment stalled. The
  storage reconcile now sizes replicas to the schedulable-node count through
  the `longhorn-storageclass` ConfigMap (the driver-deployer rebuilds the
  StorageClass from it — patching the object is immutable-refused, replacing
  it is undone), merge-patches the `default-replica-count` and
  `replica-soft-anti-affinity` settings, and heals volumes already stuck above
  the schedulable count: the in-place upgrade path for clusters that faulted
  on 0.8.0. Runtime RBAC gains what the reconcile touches. Clusters that fit
  the default (≥3 nodes) are untouched. Live-validated on the conformance
  fixture that found it: create gate hard-failure → 5 seconds.
- **A wedged rolling restart no longer hangs for days (#27):** the operator's
  node restart leaves dead peer-recovery sessions that squat OpenSearch's
  default two recovery slots forever, freezing the green gate with every edit
  locked. The stall diagnosis ADR-050 already runs now arms the
  fleet-proven remediation after a 10-minute budget (cooldown-guarded):
  transient recovery-throttle raise + bounce of the node holding the wedged
  recovery, surfaced on the activity panel in pt/en/es — stated only after it
  happened.
- `chacha20` yanked upstream redden the supply-chain gate; the lock moves to
  the replacement release.

### Added
- **UI honesty warnings (#26 follow-up, #32):** the wizard's review step and
  the deployment overview say when a sub-3-node cluster means a single
  Longhorn copy per volume (snapshots advised), and the wizard warns at
  version selection when a host node reports Debian kernel `6.1.0-52` —
  OpenSearch 3.8.x nodes crash at boot there (known incompatibility, see
  REQUIREMENTS). A warn, deliberately not a refusal: the affected matrix is
  one observed combination, and the fix is the user's. `kernel_version` now
  rides the capacity payload (absent on older servers → the warning stays
  silent rather than guessing).

### Changed
- `journey_check.py` modernized against the current wizard and deployment
  surface (it was last verified pre-SPA-rewrite): no default size in the
  4-step wizard, global nav that stays visible inside a deployment, the
  ADR-035 heap form, the generated-password security tab, and the settle
  wait on the tab that renders its sentinel.
- `docs/REQUIREMENTS.md`: every conformance row re-dated 2026-08-25 with live
  v0.8.0 evidence, the refusal fixture's first-ever run recorded, minikube
  moved to continuous smoke evidence, and the kernel known-incompatibility
  paragraph added.

## [0.8.0] - 2026-08-24

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
- A demo GIF in all three READMEs — the whole product story (first-run setup,
  conformity, a green deployment, integrations, capacity, the create wizard
  held at review) in 24 seconds, recorded against a mock API so nothing was
  provisioned. The READMEs now open with the logo, the tagline and a full badge
  row (CI, licence, Docker pulls, MSRV, Kubernetes floor, DCO).

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
- Dependency wave (the first Dependabot round, all nine PRs): `kube` 0.99 →
  **2.0.1** paired with `k8s-openapi` 0.24 → 0.26 (unpaired, two k8s-openapi
  copies compile and the unfeatured one fails its build script — the pairing is
  now part of the upgrade checklist), React and ReactDOM 18 → **19** (a
  peer-locked pair that cannot land as separate PRs), `bcrypt` 0.19, `base64`
  0.23, `tower-http` 0.7, plus the cargo/npm/actions minor groups. No API
  changes were needed for any of them.
- ADR-054: registry and signing live on GitHub and Docker Hub — the public
  `veloxsearch-registry` repo, a cosign-keyless image, and the signing key's
  private half held in a secret store rather than on a maintainer machine.
  Supersedes the pre-OSS internal-registry buffer plan and ADR-039's open
  hosting/custody provisions.

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
- The Vite dev proxy's `/api` prefix swallowed the SPA's own `api.jsx` module,
  so `npm run dev` against a live backend was a white page with a MIME error.
  The proxy key is now the regex `^/api/` (production builds were never
  affected).

## [0.7.0] - 2026-08-20

First public snapshot. The control plane provisions and operates OpenSearch
deployments on k3s, k0s, minikube and kubeadm: first-run conformity gate and
self-bootstrap, capacity-aware sizing, day-2 operations (snapshots, upgrades,
LDAP/OIDC providers), signed integration packages, and an OpenTelemetry
collection stack.

[Unreleased]: https://github.com/tornis-tecnologia/veloxsearch-oss/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/tornis-tecnologia/veloxsearch-oss/releases/tag/v0.7.0

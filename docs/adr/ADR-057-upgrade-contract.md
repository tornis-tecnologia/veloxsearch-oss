# ADR-057 — The upgrade contract: an upgrade changes VeloxSearch, not what it manages

**Status:** proposed
**Date:** 2026-09-14

## Context

The intended contract has always been that upgrading VeloxSearch replaces the
VeloxSearch Pod and nothing else. Upgrades have nonetheless needed hand repair on
existing deployments more than once, and reading the code showed that nothing
guaranteed the contract and nothing tested it (#54):

1. **Bootstrap force-applied over an operator that was merely not Ready.**
   `run_install` applied the vendored operator bundle, CRDs included, with
   server-side apply `.force()`, whenever `operator_ready` was false *at the
   instant it probed*. After a rollout the SPA sends an admin to the conformity
   screen whenever `bootstrap_status.ready` is false, and that screen calls
   `bootstrap_ensure` once, automatically. An operator Pod that is rescheduling,
   restarting or waiting on cert-manager during the same window is enough.
2. **The documented upgrade re-granted cluster-admin.** DEPLOY.md said
   `kubectl apply -f deploy/install.yaml`, which re-creates the
   `veloxsearch-bootstrap` binding that ADR-027 revokes after the first bootstrap.
   The revocation only ran during a first bootstrap or from the conformity
   screen, which a conformant cluster never shows, so the binding stayed. It was
   also what gave item 1 the power to rewrite CRDs.
3. **Following DEPLOY.md from `main` downgraded.** `deploy/install.yaml` pinned
   `0.8.1` while `Cargo.toml` said `0.9.0`; the release workflow pins the digest
   only in the release asset. Migrations are forward-only.
4. **Vendored and running operator versions were never compared.** A release
   that changed `deploy/bootstrap/operator.yaml` became an operator upgrade under
   the conditions of item 1, silently.
5. **ADR-055 is a deliberate exception** (VeloxSearch co-owns three fields of the
   operator's Dashboards Deployment) that "VeloxSearch never touches operator
   objects" did not account for.

## Decision

### What an upgrade may change

An upgrade is a rollout of a new VeloxSearch release. It may change:

- **The VeloxSearch control plane in its own namespace**: the `veloxsearch`
  Deployment (image, env, probes), its ServiceAccount, the `veloxsearch-runtime`
  ClusterRole/Roles and their bindings, the `veloxsearch-env` ConfigMap, the
  bundled Postgres StatefulSet and its Service, and the default Ingress — i.e.
  what `deploy/install.yaml` declares.
- **The control-plane database, through migrations** (forward-only; see
  DEPLOY.md "Rolling back").
- **Transiently, the `veloxsearch-bootstrap` ClusterRoleBinding**, which the
  manifest re-creates and the running app deletes again (below).
- **The ADR-055 fields** of an operator-owned Dashboards Deployment
  (`spec.strategy.type`, the Dashboards container's `startupProbe`, and
  `spec.replicas` during the one-shot `.kibana_1` remediation), under their
  dedicated field manager. This is the only listed exception.

### What an upgrade must never change

- The **OpenSearch operator Deployment**: its `.spec`, including its image.
- The **operator's and cert-manager's CRDs**, and **cert-manager's Deployments**.
- Any **`OpenSearchCluster` `.spec`**, except through a user-requested OpenSearch
  version upgrade (ADR-048), which is its own explicit action, not a side effect
  of a VeloxSearch rollout.

### How the code holds it

1. **Bootstrap installs only what is absent.** The install decision is a pure
   function of *presence* of the controller Deployment, not of readiness
   (`operator_step`, `cert_manager_step` in `src/bootstrap.rs`):
   absent → apply the vendored bundle; installed but not Ready → wait (bounded)
   and report, never apply; probe failed → refuse. CRDs without a controller
   still count as absent, because an install of ours interrupted before its
   Deployments landed leaves exactly that, and treating it as installed would
   wedge the retry. For cert-manager, any one of its three controller
   Deployments counts as present.
2. **Operator drift is reported, never applied.** `bootstrap_status` carries the
   running operator's image and the image the vendored bundle declares (read out
   of the bundle `run_install` applies, so the two cannot disagree). A mismatch
   is requirement **R9, warn-only**, with remediation text on the conformity
   screen and a notice in the main shell. Upgrading the operator is a separate,
   explicit step (DEPLOY.md "Operator version drift").
3. **The bootstrap binding is revoked again after every re-apply.** The app runs
   a background watch: once a minute, if `veloxsearch-bootstrap` exists, names
   this app's own ServiceAccount in its own namespace, no install job is running,
   and cert-manager, the operator and Longhorn are ready (the ADR-027/031
   condition), it deletes the binding.
4. **The upgrade instruction is the versioned release artifact.** DEPLOY.md
   points at `releases/download/v<version>/install.yaml` (digest-pinned) and
   says how to confirm the binding is gone. The whole manifest is applied, not
   only the image, because a release may need RBAC the previous one lacked.
5. **`deploy/install.yaml` is kept in lockstep with `Cargo.toml`.** CI's
   `manifest-version` job fails any tree whose manifest image tag is not the crate
   version, so applying the file from a checkout cannot roll an install back. The
   `velox` CLI compiles the same file in (the 0.9.0 CLI installed 0.8.1), so a
   unit test in `src/bin/velox.rs` holds the invariant on the embedded copy
   wherever `cargo test` runs, release re-verification included.
6. **The contract is tested.** `.github/workflows/upgrade.yml` installs the
   previous release on minikube, brings a deployment to green, rolls the
   candidate out over it, and asserts the operator Deployment spec and image,
   the CRDs and the `OpenSearchCluster` spec are unchanged, the deployment is
   still green, and the binding is gone. A variant scales the operator to 0
   before the rollout and asserts bootstrap applied nothing to operator-owned
   objects.

### The vendored operator bundle (item 5 of #54)

The bundle's chart label says `opensearch-operator-3.0.2` and its image says
`opensearchproject/opensearch-operator:3.0.0-alpha`. That is not a vendoring
mistake: the upstream chart 3.0.2 declares `appVersion: 3.0.0-alpha` and its
`manager.image.tag` defaults to `appVersion`, so a plain render of chart 3.0.2
produces exactly this image. The 20 CRDs in the bundle are identical to the
chart's `files/` CRDs. As of 2026-09-14 no `3.0.x` operator image exists on
Docker Hub; `3.0.0-alpha` and `latest` are the same digest. The bundle ships
`imagePullPolicy: Always` on that tag.

This ADR does **not** change the vendored operator image — that would itself be
an operator upgrade for fresh installs. Whether to digest-pin it is left to the
maintainers (see Consequences).

## Consequences

- **An installed-but-broken component is no longer auto-healed by re-applying
  the bundle.** Before, a cert-manager or operator someone had damaged got
  silently overwritten; now bootstrap waits five minutes, reports
  "installed but not Ready", and a human fixes it. That is the point, but it is
  a real loss of a (dangerous) convenience.
- **A partial cert-manager counts as installed.** If an install of ours were
  interrupted after one controller Deployment landed but before the others —
  possible only mid-apply-round — the retry waits and times out instead of
  finishing. Remediation is deleting the partial Deployments; the message names
  the one it waited on.
- **The drift report compares image references, not behaviour.** A digest pin of
  the same image reads as drift, and `imagePullPolicy: Always` on the mutable
  upstream tag means the operator's bytes can change on a Pod restart with the
  reference unchanged — R9 cannot see that. Digest-pinning the vendored image
  would close it, and is a maintainer decision because it changes what fresh
  installs run.
- **The binding is cluster-admin for up to about a minute after each
  re-apply**, longer if a component is not ready, and indefinitely on a
  cluster where Longhorn was never installed (unchanged from ADR-031). The
  watch costs one GET per minute in the steady state.
- **The watch deletes a ClusterRoleBinding without a human in the loop.** It is
  scoped by name *and* by subject to this app's ServiceAccount and namespace.
- **The upgrade lane is slow** (minikube, Longhorn, cert-manager, the operator
  and a three-node OpenSearch on one runner), so it runs on pull requests that
  touch the upgrade surface — `Cargo.toml` (every release PR), `deploy/`,
  `src/bootstrap.rs`, the lane itself — and on demand, not on every push.
- Operator upgrades are not exercised by CI; they are an explicit, documented
  act the operator owns.

## Alternatives considered

- **Image-only upgrades (`kubectl set image`).** Never re-creates the binding and
  never touches anything but the Pod. Lost because releases do change RBAC
  (ADR-055 added `patch` on `apps/deployments`); an image-only upgrade would start
  a binary without the permissions it needs, failing at the first use rather than
  at rollout.
- **Point DEPLOY.md only at the release asset, no CI gate.** Solves the
  documented path but leaves `deploy/install.yaml` on `main` naming an older
  release, which INSTALL.md and every checkout-based habit would keep applying.
  The gate is one cheap job.
- **Write the digest back into `deploy/install.yaml` from the release job.**
  Would need write access to `main` from CI, which DEPLOY.md deliberately rules
  out.
- **Split the bootstrap binding out of `install.yaml`** (a separate
  `bootstrap.yaml` applied only on first install). Removes the re-grant at the
  source, but makes the first install two commands and breaks `velox init` and
  every existing one-command instruction. Worth revisiting; the watch works
  either way.
- **Gate on readiness with a longer wait, then install.** Any timeout is a race
  against a slow node; the failure mode is still "rewrote the operator".
  Presence, not readiness, is the fact that decides whether something is ours to
  install.
- **Make drift a hard failure.** Would lock every existing install out the day a
  release vendors a newer operator — the coupling this ADR removes.
- **Upgrade the operator automatically when drift is seen.** Operator upgrades
  change CRDs and reconcile every `OpenSearchCluster`; doing that as a side
  effect of a control-plane rollout is exactly the failure #54 describes.

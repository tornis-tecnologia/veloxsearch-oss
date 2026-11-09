# ADR-068 — A `veloxagent/` workspace for Velox Mesh and Velox Agent, under the same licence as the core

**Status:** proposed
**Date:** 2026-11-09

## Context

Tornis wants fleet management for collection agents: Velox Mesh (a
Fleet-like controller) and Velox Agent (a per-host supervisor of Fluent Bit
and the OpenTelemetry Collector), with integrations as the unit of
activation. A first draft put this in an `ee/` tree under a proprietary
licence. That draft is withdrawn: the whole product stays `AGPL-3.0-only`,
the model Wazuh runs on (GPLv2 top to bottom, revenue from cloud, support and
the official build).

Licence aside, the agent and the controller still differ from the console in
ways that matter to the build:

- the agent is a static (musl) binary installed on hosts outside the cluster,
  and must not pull in `kube`, the console's HTTP stack or the SPA;
- the controller scales and upgrades on its own cadence, and a console
  upgrade must not restart the fleet's controller;
- the agent ships as `.deb`/`.rpm`/image with percentage rollouts, a release
  train unlike the console's.

Both sides need the integration renderer — the code that turns a signed
package plus variables into a collector fragment — which lives in the core
today (`src/agents.rs`, `src/integrations.rs`).

## Decision

1. **`veloxagent/` is a separate Cargo workspace** producing separate
   binaries (`velox-mesh`, `velox-agent`) and separate images. It talks to
   the core over HTTP. CI checks mechanically that `velox-agent`'s dependency
   graph contains neither the `veloxsearch` crate nor `kube`, because that is
   what keeps the agent small, not because of licensing.

2. **Same licence, same header, same contribution rules.** Every file carries
   the standard `AGPL-3.0-only` header and `check-headers.sh` applies to the
   tree unchanged. Contributions arrive under the DCO like anywhere else.

3. **What both sides share becomes a common crate.** The renderer, the closed
   interpolation token set and the manifest parser move to
   `crates/velox-integrations` (AGPL), consumed by the core and by
   `veloxagent/`. Since the licence does not change, this is a refactor, with
   no authorship audit or relicensing.

4. **Integrations stay data, signed, and identical on both paths.** Velox
   Mesh compiles policies from the same verified packages with the same
   renderer; it adds distribution and lifecycle, never content. The manifest
   schema gains two optional fields (`variables`, an OTel asset) as a
   specification change.

5. **The ten-agent free tier is a commercial signal, not a lock.** The seat
   rules in `velox-license` stay: the official build manages ten agents with
   no licence file, and a signed file raises the count. Under the AGPL anyone
   may remove the check; the file certifies a support subscription in the
   official build, and the signature detects a forged file, not a removed
   check.

## Consequences

- One repository, one licence, one contribution process. Selling a commercial
  (non-AGPL) licence of this code later would require a CLA or copyright
  assignment for the whole repository, not just this tree.
- The core gains one dependency (`velox-integrations`) and loses two modules'
  worth of code to it; behaviour is unchanged and the registry golden tests
  move with the code.
- A new CI job builds and tests `veloxagent/` on every PR; `cargo deny` runs
  there with its own allowlist.
- The seat rules live in `veloxagent/crates/velox-license` and are documented
  in `veloxagent/docs/ARCHITECTURE.md §6`; the survey, the Fluent Bit critique
  and the phased plan are in `veloxagent/docs/PLAN.md`.

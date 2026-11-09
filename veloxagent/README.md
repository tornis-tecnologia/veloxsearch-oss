# Velox Mesh and Velox Agent (`veloxagent/`)

Fleet management for VeloxSearch collection agents. **Velox Mesh** is the
central controller. **Velox Agent** is the per-host supervisor of Fluent Bit
and the OpenTelemetry Collector. Integrations are the unit of activation, and
they are the same signed packages the core's DaemonSet path uses.

Everything here is `AGPL-3.0-only`, like the rest of the repository
([LICENSE](../LICENSE)), and takes contributions under the same DCO
([CONTRIBUTING.md](../CONTRIBUTING.md)). It is a separate Cargo workspace for
build reasons, not licence reasons: the agent must stay a small static binary,
and the controller releases on its own cadence (ADR-068).

The official build of Velox Mesh manages up to **10 agents** with no licence
file. A signed licence file (`velox-mesh license load`) or an activation code
(`velox-mesh license activate`) raises the count and comes with a support
subscription.

## Layout

```
veloxagent/
  Cargo.toml                 separate Cargo workspace (not a member of the root crate)
  crates/
    velox-license/           signed licence documents, free tier, seat accounting (pure)
    velox-policy/            policy model + compiler over the shared integration renderer (pure)
    velox-opamp/             the OpAMP message subset the agent and the Mesh exchange
    velox-mesh/              the controller binary
    velox-agent/             the host agent binary
  docs/
    ARCHITECTURE.md          what is being built (pt-BR)
    PLAN.md                  survey of Fleet / Alloy / Vector / OpAMP, the Fluent Bit
                             critique, the phased plan and its criticism (pt-BR)
```

Documents are written in Portuguese first because the team deciding on them
reads Portuguese; code, comments and commit messages follow the repository's
English convention.

## Build and test

```bash
cd veloxagent
cargo test                       # every crate
cargo build --release            # target/release/velox-mesh, target/release/velox-agent
VELOX_MESH_ADDR=127.0.0.1:4320 ./target/release/velox-mesh
./target/release/velox-agent version
```

Source files carry the repository's standard header, checked by
`.github/scripts/check-headers.sh`:

```
// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
```

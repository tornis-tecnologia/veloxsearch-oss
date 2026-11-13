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

Velox Mesh has no agent limit: it manages every host enrolled, with no licence
file (ADR-068 decision 5, withdrawn 2026-11-12).

## Layout

```
../crates/velox-integrations/  the integration-package contract shared with the core:
                             manifest, closed token set, `render` (pure; ADR-068)
veloxagent/
  Cargo.toml                 separate Cargo workspace (not a member of the root crate)
  deny.toml                  cargo-deny policy for this workspace's own graph
  crates/
    velox-policy/            policy model + compiler; `PackageRenderer` over velox-integrations (pure)
    velox-opamp/             the OpAMP message subset the agent and the Mesh exchange
    velox-mesh/              the controller binary
    velox-agent/             the host agent binary
  deploy/
    install.sh               host installer: Fluent Bit from upstream, the agent from
                             its release, /opt/velox-agent, systemd (ADR-069 am. 1)
    docker/                  Dockerfile (FROM fluent/fluent-bit) + docker-compose.yml
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
../.github/scripts/check-agent-boundary.sh   # velox-agent links neither the core nor kube
cargo build --release            # target/release/velox-mesh, target/release/velox-agent
VELOX_MESH_ADDR=127.0.0.1:4320 ./target/release/velox-mesh
./target/release/velox-agent version
```

## Installing the agent on a host

The agent has its own release, `velox-agent-v<version>` (built by
`.github/workflows/agent-release.yml` when the workspace version changes):
static musl tarballs for x86_64 and aarch64, `install.sh`, `SHA256SUMS` and
a cosign bundle per file. The console's *Add agent* page prints the commands
with the Mesh URL and token filled in.

```bash
curl -fsSLO https://github.com/tornis-tecnologia/veloxsearch-oss/releases/download/velox-agent-v0.2.0/install.sh
sudo bash install.sh --mesh https://<deployment>-mesh.<domain> --token <token> [--insecure]
```

What it does, and the result on the host:

- reads `/etc/os-release` and installs Fluent Bit from `packages.fluentbit.io`
  (Debian, Ubuntu, Raspbian, RHEL/CentOS, Rocky/Alma, Amazon Linux) or the
  distribution package (Fedora). The package's own `fluent-bit.service` is
  disabled, because the agent runs the engine;
- downloads the agent and checks it against `SHA256SUMS`, plus the cosign
  signature when `cosign` is installed;
- creates the `velox-agent` system user, in `adm`/`systemd-journal`;
- lays out `/opt/velox-agent/{bin,etc/agent.env,state}`, with the state
  `0700` and owned by that user;
- writes `velox-agent.service`. The unit runs as `velox-agent` with
  `CAP_DAC_READ_SEARCH`, so it can read any log and write only to `state/`;
- enrols, starts the service and prints `velox-agent doctor`.

Options:

| Option | What it does |
| --- | --- |
| `--run-as-root` | Filebeat's model: the service runs as root |
| `--allow-privileged-ports` | Syslog on port 514 |
| `--skip-fluent-bit` | Use an engine that is already installed |
| `--binary ./velox-agent` | Offline install, or a development build |
| `--replace` | Takes over an agent started by hand and moves its identity |
| `--uninstall [--purge]` | Removes the agent. `--purge` also deletes the state and the user |

Re-running the script upgrades in place.

In a container ([deploy/docker/](deploy/docker/)), the image is upstream's
`fluent/fluent-bit`, unmodified, plus the agent:

```bash
cd veloxagent/deploy/docker && cp .env.example .env   # Mesh URL, token, host name
docker compose up -d
```

`run` enrols itself from `VELOX_MESH_URL` / `VELOX_ENROLL_TOKEN` when the
state volume has no identity yet. The container reads the host's `/var/log`
read-only, as root with every capability dropped except `DAC_READ_SEARCH`.

Agent environment:

| Variable | Default / meaning |
| --- | --- |
| `VELOX_AGENT_STATE` | `/opt/velox-agent/state` |
| `VELOX_FLUENT_BIT` | the engine binary (else `$PATH`, `/opt/fluent-bit/bin`) |
| `VELOX_AGENT_HOSTNAME` | the name the fleet shows (a container's `/etc/hostname` is its id) |
| `VELOX_MESH_URL`, `VELOX_ENROLL_TOKEN`, `VELOX_AGENT_INSECURE` | enrol on `run` when not enrolled yet |
| `VELOX_AGENT_POLL` | check-in interval in seconds (tests) |

Phase 0 has no screens (the Mesh console is phase 1). To inspect what it does
build, compile a policy against real packages and read what an agent would
receive — the shared renderer, the closed token set, schema-1.1 variables and
every fail-closed refusal:

```bash
cargo run -p velox-policy --example compile -- <veloxsearch-registry checkout>
cargo run -p velox-policy --example compile -- <checkout> nginx \
    --var log_path=/var/log/nginx/access.log --host logs.example.internal
```

Source files carry the repository's standard header, checked by
`.github/scripts/check-headers.sh`:

```
// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
```

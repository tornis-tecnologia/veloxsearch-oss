# ADR-069 — The collection engines are the operator's to install; Velox Agent supervises what it finds

**Status:** proposed
**Date:** 2026-11-09

## Context

Velox Agent is a supervisor, not a collector: it runs Fluent Bit (and, by
exception, an OpenTelemetry Collector) as child processes and never puts a
line of Velox code inside either (ADR-068, `veloxagent/docs/PLAN.md` §3.3).
That leaves one question the architecture had answered twice, in two
incompatible ways: **how does the engine binary get onto the host?**
`veloxagent/docs/ARCHITECTURE.md` §7 said both "engines pinned per agent
version, distributed together" (one artifact) and "upgrade by OpAMP
*packages*" (a download). Those are different mechanisms with different
promises, and neither was built.

What upstream actually publishes decides most of it. Verified on 2026-11-09
against `docs.fluentbit.io` and the project's releases:

- **There is no official standalone static Linux binary.** The Linux
  installation path is the project's own apt/yum repositories at
  `packages.fluentbit.io`, GPG-signed (key fingerprint
  `C3C0 A285 34B9 293E AF51 FABD 9F9D DC08 3888 C1CD`), or a build from
  source. The GitHub release for v5.2.0 carries no `.deb`, `.rpm` or
  tarball at all — only schema JSON and an experimental browser build.
  Windows is the exception: `.zip` / `.exe` / `.msi` per architecture, each
  with a `.sha256` sidecar.
- **Most distributions do not package Fluent Bit.** Debian and Ubuntu have
  no `fluent-bit` in their official archives, in any suite. Fedora carries
  4.2.2; EPEL 9 carries 4.2.2 and EPEL 8 only 2.2.2; Alpine has 5.0.9 in
  edge/testing; Arch has none in the official repositories.

So "ship a pinned engine with the agent" means *we* build Fluent Bit — a C
codebase with OpenSSL, libyaml and friends — for every architecture we
support, and we own its CVE response and its release cadence. That is a
second product. The team does not want it.

## Decision

1. **VeloxSearch does not distribute the collection engines.** No engine
   binary in the agent's package, in its tarball, or served by the Mesh. The
   agent ships alone and supervises the Fluent Bit the host already has.

2. **Installing Fluent Bit is the operator's step, and it is documented per
   distribution.** The console carries the procedure — one tab per
   distribution, the upstream repository commands verbatim, copy-paste — so
   the operator never has to reconcile our instructions with Fluent Bit's.
   **The set of targets is the project's, not ours**: the tabs mirror
   `docs.fluentbit.io/manual/installation/downloads`, including the ones we
   have no commands for (Buildroot, Yocto, macOS), which link to the project's
   page rather than inventing a procedure. We do not add a target the project
   does not document, even where a distribution happens to package Fluent Bit.
   Where the distribution packages Fluent Bit itself (Fedora, EPEL) that
   package is accepted; where it does not (Debian, Ubuntu, Arch) the upstream
   repository is required, and the screen says so rather than pretending
   `apt install fluent-bit` works.

3. **We declare a minimum version and the agent enforces it.** The minimum is
   **Fluent Bit 3.2**, because:
   - YAML became the standard configuration format in 3.2, which is what
     fixes the floor here. What the agent actually writes is the **classic**
     format, because that is what the signed integration packages ship
     (`agent.conf.tmpl`) and what the compiler assembles; the file is
     therefore named `fluent-bit.conf`. This matters more than it looks:
     Fluent Bit chooses its parser from the file **extension**, so the same
     body is accepted as `.conf` and rejected as `.yaml`. Moving the
     packages to YAML is a registry change, and until it happens the
     agent's staging, live and previous files all end in `.conf` so the
     dry-run validates the file that will actually run;
   - hot reload through `POST /api/v2/reload` arrived in 2.1.0;
   - the OTLP input (`opentelemetry`) arrived in 2.0.0, `prometheus_scrape`
     and `windows_exporter_metrics` in 1.9.
   `velox-agent doctor` finds the binary, reads `--version`, and refuses to
   run below the minimum with the installed version in the message. EPEL 8's
   2.2.2 therefore fails loudly at install time, not mysteriously at runtime.

4. **The agent writes absolute paths in every configuration it generates.**
   Fluent Bit only keeps track of the main configuration file's directory
   across a hot reload from 5.1.2 onwards; a relative path in a config
   reloaded on 3.2 or 4.x resolves against the wrong directory. Absolute
   paths make the minimum 3.2 instead of 5.1.2.

5. **`--dry-run` is probed, not assumed.** Upstream documents the flag but
   not the version that introduced it (it says only that 4.2 added full
   property validation on top of syntax checking). The agent probes it once
   against a trivial config: present, it is used as the apply plan's
   validation step; absent, the agent logs that it is applying without
   pre-validation and relies on the health confirmation and rollback that
   follow. A supervisor that assumed the flag would silently skip validation
   on an older engine.

6. **Air-gapped sites mirror the upstream repository.** We document that
   requirement and do not solve it. Mirroring an apt/yum repository is a
   solved operations problem; shipping a C toolchain is not.

## Consequences

- One mechanism instead of two, and `ARCHITECTURE.md` §7 is rewritten to say
  it: no bundling, no OpAMP `packages`, no engine download. If engine
  distribution ever becomes necessary, OpAMP `packages` served by the Mesh is
  the door to use, and it needs a new ADR plus the build pipeline this one
  declines.
- The engine version is the host's, not the fleet's. The Mesh reports the
  installed version per agent so an operator can see the spread, and a policy
  that needs a newer feature fails on the agents that cannot run it, by name.
  "Pinned per agent version" is gone from the vocabulary.
- The DaemonSet path keeps its own pinned image (`src/agents.rs`
  `AGENT_IMAGE`), because in Kubernetes the image *is* the install and no
  operator step exists. That image is today `fluent/fluent-bit:3.1.9`, below
  the host minimum this ADR sets; aligning the two is phase-1 work and is
  tracked in `veloxagent/docs/PLAN.md`.
- Windows is the one place upstream ships a redistributable artifact, and we
  still do not redistribute it: the same documented-install rule applies, with
  the `.msi` and its `.sha256`.
- The minimum version appears in three places — this ADR, the agent's
  `MIN_FLUENT_BIT`, and the console's install screen. The ADR is the source;
  the other two cite it.

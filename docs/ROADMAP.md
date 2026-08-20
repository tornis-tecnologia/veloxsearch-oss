# Roadmap

What is planned, what is open, and what is deliberately not being done. Built
from the state of the code, not from aspiration — every entry names the thing in
the repository it is about.

Dates are absent on purpose. This is a small project; sequence is a commitment,
schedule is not.

## Now

**Release automation.** CI builds the image but never pushes it. A tag should
build multi-arch, publish to `docker.io/tornistecnologia/veloxsearch-oss`,
rewrite the image pin in `deploy/install.yaml`, and cut a GitHub release from the
changelog. Today [DEPLOY.md](DEPLOY.md) documents that as a manual runbook.

**An end-to-end CI lane.** `tests/smoke_check.py` was written for a minikube CI
lane that does not exist yet. Everything but `smoke_check.py` needs a cluster
that meets the full platform contract, which a CI minikube deliberately does not
— so the lane is "smoke on minikube", with the rest staying manual against the
conformance fleet.

**arm64.** R5 restricts the supported envelope to amd64 because that is what is
built and tested. Multi-arch images plus an arm64 fixture would lift it. Wanted
for Raspberry-Pi-class and Graviton clusters.

## Next

**A Helm chart** alongside `deploy/install.yaml`, for people whose deployment
pipeline is Helm-shaped. The one-file manifest stays the primary path: it is
what makes the quickstart two commands.

**Multi-tenancy to GA.** `VELOX_MULTITENANT_AUTH` and the Postgres-backed
account store (`tenants.rs`, ADR-041) are complete enough to run but default
off. Reaching GA means an admin UI for tenant management, the enforcement gaps
`deploy/tenant-templates/README.md` is explicit about, and a migration path for
existing single-admin installs.

**More log integrations.** The catalog ships nginx as the reference package;
PostgreSQL, Kafka, Redis, MySQL and Kubernetes events exist as built-in recipes
that should become registry packages. This is the **best place to contribute**:
packages are data, not code, so they need no Rust. See
[integrations/](integrations/) and the
[registry repository](https://github.com/tornis-tecnologia/veloxsearch-registry).

**The OpenTelemetry stack to parity.** `otel_stack.rs` is ~3,900 lines and works,
but it is positioned as an additive second option beside the recipe-based
agents. Parity means the create wizard offering it as a first-class choice.

**Brownfield clusters.** Out of scope for v1 by decision (ADR-026, and the end of
[REQUIREMENTS.md](REQUIREMENTS.md)): a cluster with a foreign cert-manager
version, conflicting CRDs or an existing operator is refused rather than adopted.
Widening this is a "broaden support" phase, and it should happen after the narrow
envelope is bulletproof, not before.

## Open questions

These are genuinely undecided. An informed argument in an issue is welcome.

**The query layer over Postgres.** `db.rs` is deliberately the bare
`tokio-postgres` driver with a hand-rolled migration runner — the choice of sqlx
vs. diesel vs. staying bare was explicitly deferred, not overlooked. It should be
decided before the schema grows much further.

**High availability of the control plane.** The Deployment is one replica because
two replicas racing on cluster writes is a correctness problem. Real HA needs
leader election, and it needs a reason: the control plane being briefly
unavailable does not affect the OpenSearch clusters it manages.

**Widening the platform matrix.** R1–R8 is narrow by policy. Each addition costs
a conformance fixture that has to be run, so the question is not "can it work"
but "who runs the fleet".

**Restore.** Snapshots are scheduled and verified; restore is a UI and a set of
failure modes that has not been designed (ADR-015 was explicitly "basics first").

## Not planned

Stated so nobody spends effort on them:

- **A plugin system for integrations.** Packages are data, and signature
  verification is the entire security boundary. Executable integrations would
  discard that guarantee, and no sandbox would give it back.
- **Support for Kubernetes < 1.30.** The envelope is what is tested.
- **Windows nodes, OpenShift.** Neither is on the conformance fleet, and adding
  a platform nobody runs is a promise that cannot be kept.
- **A second UI framework or a router.** The SPA is deliberately plain React with
  no router and no state library; screens are swapped by application state. This
  is a small app and the constraint has held.

## Contributing to any of this

Pick something above, open an issue saying what you intend, and see
[CONTRIBUTING.md](../CONTRIBUTING.md). Anything that changes a boundary needs an
ADR first — see [adr/README.md](adr/README.md).

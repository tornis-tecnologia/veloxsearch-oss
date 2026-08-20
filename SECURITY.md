# Security Policy

## Reporting a vulnerability

**Do not open a public issue.**

Report privately through GitHub Security Advisories:
<https://github.com/tornis-tecnologia/veloxsearch-oss/security/advisories/new>

If you cannot use that form, email **tornis@tornis.com.br** with `SECURITY` in
the subject.

Please include: the version or commit, the cluster distribution and Kubernetes
version, what an attacker gains, and the smallest reproduction you have. A
proof-of-concept helps but is not required to report.

**What to expect**

| | |
| --- | --- |
| Acknowledgement | within 5 business days |
| Initial assessment | within 10 business days |
| Fix or mitigation plan | communicated with the assessment |
| Disclosure | coordinated with you, after a fix ships |

We will credit you in the advisory unless you ask us not to. There is no bug
bounty.

## Supported versions

VeloxSearch is pre-1.0. Security fixes land on `main` and in the next release;
older releases are not backported.

| Version | Supported |
| --- | --- |
| 0.7.x | ✅ |
| < 0.7 | ❌ |

## Threat model

Knowing what the design already guarantees will tell you whether what you found
is a bug or a deliberate boundary.

### Integration packages are the security boundary

Packages fetched from the registry are **data, not code**. Two properties hold
them in place, and a hole in either is a vulnerability:

- **Signature verification** (`catalog::verify_package`,
  [docs/integrations/signing.md](docs/integrations/signing.md)). Every package is
  ed25519-signed and checked against a keyring compiled into the binary — so it
  works with no network and cannot be redirected by a compromised registry.
  Verification fails closed, and *unsigned*, *unknown key* and *tampered* are
  three distinct hard rejects. Anything that applies package bytes to a cluster
  without passing this check is a critical finding.
- **Closed-set interpolation** (`integrations::CLOSED_TOKENS`,
  [docs/integrations/interpolation.md](docs/integrations/interpolation.md)).
  Exactly eight tokens are substituted into package assets. A way to make a
  package expand something outside that set — an env var, a file path, a
  Kubernetes object reference — is a finding.

The private signing key is never in this repository nor in the image. See
[keys/README.md](keys/README.md) for custody and rotation.

### Ownership is enforced by the type system

A handler cannot name a deployment it has not proved ownership of: the
Kubernetes layer takes `&Deployment`, and a `Deployment` can only be minted from
a `Scope` derived from the signed session cookie (`src/scope.rs`). A missing
check is a compile error. A way to construct a `Deployment` from a raw string,
or a Kubernetes-layer function taking `&str`, is a finding even with no
demonstrated exploit.

`Scope::resolve` deliberately cannot distinguish "does not exist" from "belongs
to someone else". If you find a path where the two are distinguishable — timing,
error text, status code — that is an enumeration finding.

### Session secret

`VELOX_SESSION_SECRET` signs the session cookie. In the managed path the app
generates a random secret and persists it in a Kubernetes Secret, which is what
a normal install uses. Outside that path it falls back to a **well-known
development default** (`src/auth.rs`). That fallback is documented, not a
vulnerability — but running an installation that uses it is one, so:

- **Always set `VELOX_SESSION_SECRET`** if you run the binary outside the
  managed install (bare process, custom manifest, docker run).
- Set `VELOX_COOKIE_SECURE=1` when serving over HTTPS.

### RBAC

The install manifest grants two roles (ADR-002): `veloxsearch-runtime` for
day-to-day work, and `veloxsearch-bootstrap` with cluster-admin **only** for the
one-time self-bootstrap. A permission that has migrated from bootstrap into
runtime, or a runtime permission wider than the operation that needs it, is a
finding.

### Off-cluster guard

`src/k8s.rs` falls back to a namespace that does not exist and refuses writes
loudly, so a developer machine pointed at a production kubeconfig cannot drive
it silently. A path that writes to a cluster without passing
`ensure_namespace_exists` is a finding.

## Out of scope

- The security posture of the OpenSearch clusters VeloxSearch provisions, beyond
  what VeloxSearch itself configures — report those to the OpenSearch project.
- Vulnerabilities in the vendored upstream manifests under `deploy/bootstrap/`
  (cert-manager, the OpenSearch operator, Longhorn). Report those upstream; tell
  us too if we ship an affected version so we can bump it.
- Anything that requires cluster-admin on the cluster VeloxSearch runs in — that
  is already above our privilege level.
- Missing hardening on a deliberately documented development default, where the
  documentation says to change it.

## Hardening checklist for operators

- Set `VELOX_SESSION_SECRET` and `VELOX_COOKIE_SECURE=1`.
- Do not expose the control plane to the internet without an authenticating
  proxy; the default install path is `port-forward` for exactly this reason.
- Pin the image by digest, not by tag.
- Review the catch-all Ingress in `deploy/install.yaml` before applying on a
  shared cluster — delete it if a catch-all route is unwanted.
- Keep `VELOX_REGISTRY_URL` pointed at a registry you trust; signature
  verification protects package contents, not your choice of catalog.

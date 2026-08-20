# Contributing to VeloxSearch

*[Leia em português](CONTRIBUTING.pt-BR.md)*

Thanks for considering a contribution. This document covers what you need to
build the project, the conventions the codebase holds to, and how a change gets
merged.

By participating you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to contribute that need no Rust

- **Log integrations.** An integration is a signed data package — manifest,
  ingest pipeline, index template, dashboards, Fluent Bit config. It contains no
  code. They live in
  [`veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry);
  the format is specified in [docs/integrations/](docs/integrations/).
- **Translations.** Every UI string lives in `frontend/i18n.jsx` (pt + en today).
  Adding a language means adding a key set there, not touching the views.
- **Documentation.** Anything in `docs/`. The English file is canonical; the
  `.pt-BR` mirror should follow in the same PR when you change one.
- **Reproductions.** A precise bug report against a named distro and version is
  worth a lot: the supported envelope is written down in
  [docs/REQUIREMENTS.md](docs/REQUIREMENTS.md), and gaps in it are findings.

## Getting set up

You need Rust (the MSRV is pinned as `rust-version` in `Cargo.toml`), Node 20+,
and — for anything that touches a cluster — minikube or another local
Kubernetes.

```sh
git clone https://github.com/tornis-tecnologia/veloxsearch-oss.git
cd veloxsearch-oss
cargo build            # the control plane; default feature is "ssr"
cargo test             # tests are inline #[cfg(test)] modules
cd frontend && npm ci && npm run build
```

The full local loop — running the backend and the Vite dev server together,
building the image, running the tests that need Postgres or a registry
checkout — is in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md).

**Off-cluster safety.** `src/k8s.rs` falls back to a deliberately nonexistent
`veloxsearch-dev` namespace, and `ensure_namespace_exists` turns every write
into a loud refusal. This exists so a dev box pointed at a live kubeconfig can
never silently drive production. Do not remove that guard, and do not "fix" it
by defaulting to a namespace that exists.

## The two conventions that are load-bearing

Read these before writing a handler or touching the Kubernetes layer. They are
not style preferences — they are the reason certain classes of bug cannot occur.

### 1. Ownership is a type, not a check (`src/scope.rs`)

A handler that names a deployment must turn the attacker-controlled `String`
into a `Deployment`, and the only way to mint one is through a `Scope` derived
from the signed session cookie. `Deployment` has private fields, no public
constructor, and no `From<&str>`. The Kubernetes layer takes `&Deployment`:

```rust
k8s::delete_cluster(&req.name)                        // does not compile
k8s::delete_cluster(&scope.require(&req.name).await?) // the only way in
```

A forgotten ownership check is therefore a compile error, not a security
incident. **Never** add a `&str`-taking escape hatch to the Kubernetes layer,
and never widen `Deployment`'s constructors.

`Scope::resolve` returns `None` identically for "does not exist" and "belongs to
someone else" — same namespace, same label selector, same code path. That
anti-enumeration property is by construction; preserve it when you touch
lookups.

### 2. Pure module, and `k8s.rs` does the writing

`snapshot`, `upgrade`, `auth_provider`, `provisioning`, `activity` and
`otel_stack` are deliberately pure: they render artifacts and decide rules with
no cluster calls, and `k8s.rs` performs the writes. That is what makes them
unit-testable without a cluster. Keep new logic on the pure side and let
`k8s.rs` apply it.

## Code conventions

- **Every source file starts with the two-line header** (`.rs`, `.jsx`, `.js`,
  `.css`, `.sh`):

  ```
  // Copyright (C) 2026 Tornis Desenvolvimento
  // SPDX-License-Identifier: AGPL-3.0-only
  ```

  CI checks this mechanically, so a missing header fails the build rather than
  the review.

- **Comments explain *why*, and cite the decision.** This codebase is unusually
  comment-dense on purpose. A comment that restates the code is noise; a comment
  that records why an obvious alternative was rejected is the point. Cite the ADR
  or issue where the decision lives (`ADR-039`, `#75`).

- **A new dependency carries a comment justifying it** in `Cargo.toml`. For
  TLS/crypto crates, explain why it reuses the process-wide `aws-lc-rs` rustls
  provider rather than pulling in `ring` as a second one.

- **Configuration is environment variables, all prefixed `VELOX_`.** Feature
  flags default to **off** and preserve prior behaviour when unset.

- **Tests are inline `#[cfg(test)] mod tests`** at the bottom of the module, not
  a `tests/` Rust target — `tests/` holds the Python end-to-end scripts.
  `src/scope/tests.rs` is the one split-out exception.

- **Adding an API endpoint** means four things, and the HTTP method must match
  on both sides: a DTO in `src/api.rs`, a handler in `src/api.rs`, a line in
  `routes()`, and a wrapper in `frontend/api.jsx`.

- **Frontend**: plain React 18, no router and no state library. Flat `.jsx`
  files, not a `src/` tree. UI text goes in `i18n.jsx`, never inline in a view.
  `localStorage` holds UI preferences only — never server state, never mock
  data.

- **Registry packages are data, not code.** Interpolation is a closed set of
  exactly eight tokens (`integrations::CLOSED_TOKENS`) and signature
  verification is the entire security boundary (`catalog::verify_package`). Do
  not add a bypass, and do not widen the token set without changing the spec
  first.

## Making a change

1. **Open an issue first** for anything beyond a bug fix or a doc correction. It
   is cheaper to disagree about an approach in an issue than in a finished PR.
2. **Branch from `main`.** Name it for what it does: `fix/pending-pvc-message`,
   `feat/kafka-recipe`.
3. **Keep the PR to one concern.** A refactor and a behaviour change in the same
   diff take three times as long to review.
4. **Update the docs in the same PR.** If you renamed a form field or reshaped a
   `nav.tabs` structure, check `tests/*_check.py` — the end-to-end tests drive
   rendered widgets, not URLs, so a UI rename can break them.
5. **Sign off your commits** (see below).
6. **Open the PR** against `main` and fill in the template.

### Commit messages

Present tense, imperative, and a body that says *why* when the *what* is not
self-evident:

```
capacity: propose pools from schedulable memory, not node count

A 3-node cluster with 2 GiB free per node cannot host the pool a node
count alone would suggest. Sizing off allocatable memory makes the
proposal fail loudly at plan time instead of at scheduling time (#118).

Signed-off-by: Your Name <you@example.com>
```

### Developer Certificate of Origin

This project uses the [DCO](https://developercertificate.org/) rather than a
CLA. Every commit must carry a `Signed-off-by` line, which `git commit -s` adds
for you. It certifies that you wrote the patch or otherwise have the right to
submit it under the AGPL-3.0-only license. Use your real name and a real email.

To sign off commits you already made:

```sh
git rebase --signoff main
```

## What CI checks

Every PR runs [`.github/workflows/ci.yml`](.github/workflows/ci.yml):

| Job | What it enforces |
| --- | --- |
| `rust` | `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`, plus the `#[ignore]`d tests against a Postgres service |
| `msrv` | The crate still builds on the `rust-version` pinned in `Cargo.toml` |
| `frontend` | `npm ci && npm run build` |
| `image` | `deploy/Dockerfile` builds (nothing is pushed) |
| `supply-chain` | `cargo deny check` — licenses, advisories, banned crates, sources |
| `headers` | Every source file carries the SPDX header |

Run the fast ones locally before pushing:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

## Review

Maintainers are listed in [`.github/CODEOWNERS`](.github/CODEOWNERS). Expect a
first response within a week; ping the PR if it goes quiet longer than that. A
change that touches `src/scope.rs`, `src/auth.rs`, `src/catalog.rs` or the
signing path gets a closer read than the rest — those are the security
boundaries.

## Security

Do **not** open a public issue for a vulnerability. Report it privately via
GitHub Security Advisories; see [SECURITY.md](SECURITY.md).

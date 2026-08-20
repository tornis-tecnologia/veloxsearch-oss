# Development

How to build, run and test VeloxSearch locally. For what the code *is*, read
[ARCHITECTURE.md](ARCHITECTURE.md); for the conventions a change must follow,
read [CONTRIBUTING.md](../CONTRIBUTING.md).

## Prerequisites

| | |
| --- | --- |
| Rust | The MSRV pinned as `rust-version` in `Cargo.toml`. Stable works. |
| Node | 20 or newer |
| Docker | Only for building the container image |
| minikube | Only for running against a real cluster |
| Postgres | Only for the `#[ignore]`d control-plane-account tests |

## Build

```sh
cargo build                      # the control plane (default feature = "ssr")
cargo build --release            # target/release/veloxsearch
cargo build --bin velox          # the operator CLI

cd frontend && npm ci && npm run build   # -> frontend/build
```

There are **two binaries**, so `--bin` is required whenever you `cargo run`.

## The local loop

Two processes: the backend on the port Vite proxies to, and the Vite dev server
that serves the SPA and forwards `/api/*` to it.

```sh
# terminal 1 — the control plane
VELOX_SITE_ADDR=127.0.0.1:3000 cargo run --bin veloxsearch

# terminal 2 — the SPA with hot reload
cd frontend && npm run dev
```

Open the URL Vite prints (not 3000 — that is the backend). The proxy target is
`http://127.0.0.1:3000` by default; override it with `VITE_API_TARGET`. The
`/api/events` SSE stream is explicitly configured to pass through unbuffered, so
the deployment list updates live in dev exactly as it does in production.

**No cluster attached?** That is fine for frontend and pure-module work. The
Kubernetes layer falls back to the nonexistent `veloxsearch-dev` namespace and
refuses writes loudly, which is the intended behaviour — see
[ARCHITECTURE.md § Off-cluster safety](ARCHITECTURE.md#off-cluster-safety).
Anything that needs real cluster state wants minikube.

## Tests

```sh
cargo test                       # everything that needs nothing external
```

Tests are inline `#[cfg(test)] mod tests` at the bottom of each module.

### Tests that need a live Postgres

The migration runner and the control-plane-account tests are `#[ignore]`d
without a database:

```sh
docker run --rm -d -p 5433:5432 -e POSTGRES_PASSWORD=t --name velox-pg postgres:16-alpine

VELOX_PG_TEST_URL=postgres://postgres:t@127.0.0.1:5433/postgres \
  cargo test -- --ignored --skip dump_manifests --test-threads=1
```

Both flags matter:

- `--skip dump_manifests` — it wears `#[ignore]` but is a dev tool, not a test:
  it asserts it is running against a real cluster namespace. Run it deliberately
  when you want it (see its doc-comment).
- `--test-threads=1` — these tests share one database and truncate it between
  cases. Run them concurrently and they clobber each other. Serial is the
  execution model they are written for.

### Tests that need the integration registry

`src/registry_golden.rs` checks that the in-binary recipe catalog and the
canonical packages in the
[registry repo](https://github.com/tornis-tecnologia/veloxsearch-registry) have
not drifted apart:

```sh
git clone https://github.com/tornis-tecnologia/veloxsearch-registry.git
VELOX_REGISTRY_PATH=/path/to/veloxsearch-registry cargo test
```

Without the variable these tests **skip locally with a loud banner**, and are a
**hard failure in CI** — a lane without the checkout would go green while
proving nothing.

### End-to-end checks

`tests/` holds standalone Python scripts, not a Rust test target. See
[tests/README.md](../tests/README.md) for what each one needs and how to run
them.

## Before you push

The CI jobs you can run in seconds:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
./.github/scripts/check-headers.sh
```

`cargo deny check` (the supply-chain gate) needs `cargo install cargo-deny`
once.

## Building the container image

```sh
deploy/build-image.sh                       # builds, does not push
deploy/build-image.sh --profile debug       # faster, bigger binary
deploy/build-image-local.sh                 # builds + side-loads into minikube
```

`deploy/build-image-local.sh` tags the result `veloxsearch:dev` — deliberately
not the published reference, so a stray `docker push` cannot publish a dev build
and `imagePullPolicy` cannot silently fetch a remote image over the side-loaded
one. It prints the two `kubectl` commands that point a running install at it.

The release path — versioning, pushing, pinning the digest — is
[DEPLOY.md](DEPLOY.md).

## Running against minikube

```sh
minikube start --memory=8192 --cpus=4
deploy/build-image-local.sh
kubectl apply -f deploy/install.yaml
kubectl -n veloxsearch-system set image deploy/veloxsearch veloxsearch=veloxsearch:dev
kubectl -n veloxsearch-system patch deploy/veloxsearch --type=json \
  -p='[{"op":"replace","path":"/spec/template/spec/containers/0/imagePullPolicy","value":"IfNotPresent"}]'
kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80
```

8 GiB is the practical floor: the first run bootstraps cert-manager, the
OpenSearch operator and (on minikube's node-local default StorageClass)
Longhorn, and then an OpenSearch cluster has to fit on top.

## Useful environment variables

| Variable | Effect |
| --- | --- |
| `VELOX_SITE_ADDR` | Bind address. `127.0.0.1:3000` is what the Vite proxy expects |
| `VELOX_STATIC_DIR` | Where the SPA build is served from |
| `VELOX_CONTROL_PLANE_NS` | Overrides the namespace the control plane manages |
| `VELOX_SESSION_SECRET` | Session-cookie signing key. **Set this** outside the managed install |
| `VELOX_COOKIE_SECURE` | `1` when serving over HTTPS |
| `VELOX_PG_ENABLED` | `1` turns on the Postgres-backed control-plane store |
| `VELOX_MULTITENANT_AUTH` | Self-serve accounts. Default off; needs `VELOX_PG_ENABLED=1` |
| `VELOX_REGISTRY_URL` | Integration registry. `https://` or `file://` |
| `VELOX_REGISTRY_TOKEN` | Only for a private registry mirror |
| `VITE_API_TARGET` | Dev-only: where the Vite proxy forwards `/api/*` |

Every flag defaults to **off** and preserves prior behaviour when unset. The
secrets among them are inventoried in [SECRETS.md](SECRETS.md).

## Things that will surprise you

- **Two binaries.** `cargo run` alone fails; pass `--bin`.
- **`frontend/` is flat.** No `src/` tree, no router, no state library. That is
  the design, not an oversight.
- **UI strings live in `i18n.jsx`.** A string inline in a view will not be
  translatable and will fail review.
- **The e2e tests drive widgets, not URLs.** Renaming a form field can break
  `tests/*_check.py` even though nothing about routing changed.
- **`ADR-0xx` citations are everywhere.** See [adr/README.md](adr/README.md) for
  what they refer to.

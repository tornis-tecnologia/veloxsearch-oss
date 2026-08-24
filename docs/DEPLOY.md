# Deploy and release

How a VeloxSearch release is built, published and rolled out. For installing a
published release, see [INSTALL.md](INSTALL.md).

## What a release consists of

Three artifacts that must agree with each other:

| Artifact | Where |
| --- | --- |
| The container image | `docker.io/tornistecnologia/veloxsearch-oss:<version>` |
| The install manifest | `deploy/install.yaml`, with the image pinned |
| The git tag | `v<version>` on `main` |

The version has one source: `version` in `Cargo.toml`. `deploy/build-image.sh`
reads it, and `deploy/install.yaml` pins the same value. If those two ever
disagree, the manifest wins for what runs and the binary wins for what the code
is — which is exactly the confusion the single source exists to prevent.

## Cutting a release

**Releasing is merging a pull request.** There is no tagging step, no manual
build, and no publish command — `.github/workflows/release.yml` does all of it
when it sees `version` change in `Cargo.toml` on `main`.

Open one pull request containing exactly three things:

1. **`version` in `Cargo.toml`**, then `cargo check` so `Cargo.lock` follows.
2. **`CHANGELOG.md`** — move the `Unreleased` entries under the new version with
   the date. Breaking changes get their own callout: this project is pre-1.0, so
   a minor version may break, and the changelog is the only place that says so.
   The release notes are lifted verbatim from this section, so write it for the
   person reading the release page.
3. **`deploy/install.yaml`** — the image tag, to the new version.

Get it reviewed and merge it. That is the release.

### What the workflow then does

| Step | |
| --- | --- |
| `gate` | Confirms the version actually changed, and refuses if a tag for it already exists |
| `verify` | Re-runs every CI gate **on the release commit**. `main` having been green earlier is a different claim from this tree being green |
| `publish` | Builds and pushes `<image>:<version>`, then signs it with cosign **keyless** — an OIDC identity and a ten-minute certificate, so there is no signing key to leak or rotate |
| `release` | Rewrites the manifest's image to the published **digest**, tags `v<version>`, and publishes the release with `install.yaml`, `velox-linux-amd64` and `SHA256SUMS` attached |

### Why the digest lives in the release, not in `main`

The artifact users apply is
`releases/latest/download/install.yaml`, and it is pinned to a digest — the same
URL applied twice gives the same bytes and the same image. `deploy/install.yaml`
on `main` keeps a version *tag*, because it is the source the release is built
from and because a tag is what `deploy/build-image-local.sh` can work with.

This is also why no job in the release pipeline has write access to `main`.
Nothing pushes back, so there is no loop to guard against and no branch
protection to bypass. The trigger condition is its own protection: a commit that
does not change the version cannot start a release.

### If it goes wrong

The workflow is not transactional. If `publish` succeeds and `release` fails,
the image is on Docker Hub but no tag or release exists. That is recoverable and
deliberately not automated: re-running the failed job republishes the same
digest (Docker Hub accepts the identical push), and `cosign sign` is idempotent
for a digest already signed.

Do **not** fix it by bumping the version again — that publishes a second image
for the same code.

## Publishing manually

The image build is deliberately separate from the push:

```sh
deploy/build-image.sh                                   # build only
deploy/build-image.sh --push                            # build and publish
deploy/build-image.sh --tag ghcr.io/you/velox:test --push   # somewhere else
VELOX_IMAGE=registry.example.com/velox deploy/build-image.sh --push
```

`--skip-build` stages artifacts that are already built rather than re-running
cargo and npm — useful when the binary and the SPA came from earlier CI jobs.

Pushing requires `docker login` against the target registry. Nothing in this
repository holds registry credentials, and nothing should.

## Rolling out an upgrade

```sh
kubectl apply -f deploy/install.yaml
kubectl -n veloxsearch-system rollout status deploy/veloxsearch
```

The Deployment is a single replica by design: the control plane holds no
in-memory state that a second replica could serve, and two replicas racing on
cluster writes is a correctness problem, not a scaling win. High availability is
[on the roadmap](ROADMAP.md) and needs the leader-election work first.

Database migrations run **before** the app serves and exit the process on
failure. A rollout that fails to migrate therefore fails closed: the old Pod
keeps serving until the new one is ready, and the new one never becomes ready
with a half-applied store.

## Rolling back

```sh
kubectl -n veloxsearch-system rollout undo deploy/veloxsearch
```

Migrations are forward-only. Rolling the image back to a version that predates a
migration is not supported and is not tested — if a release needs to be undone
after its migrations ran, cut a forward release that reverses the change.

## Air-gapped and side-loaded installs

Build once, carry the tarball, import per platform:

```sh
deploy/build-image.sh --tag veloxsearch:0.7.0
docker save veloxsearch:0.7.0 -o veloxsearch.tar
```

| Platform | Import command |
| --- | --- |
| minikube | `minikube image load veloxsearch.tar` |
| k3s / containerd | `sudo ctr -n k8s.io images import veloxsearch.tar` |
| k0s | `sudo k0s ctr -n k8s.io images import veloxsearch.tar` |

Then set `imagePullPolicy: IfNotPresent` and point the Deployment at the
side-loaded tag. The full per-platform walkthrough is in
[INSTALL.md](INSTALL.md#2c-side-load-offline--air-gapped--no-registry-at-all).

An air-gapped install also wants a local integration registry: point
`VELOX_REGISTRY_URL` at a `file://` checkout. Signature verification is
identical on every route, because the trusted key is compiled into the binary
rather than fetched.

## Publishing an integration package

Integration packages live in
[`veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry).
Signing them is the one publishing step that is **deliberately not automated**:
the ed25519 private key is not in CI, and putting it there to save a few minutes
a year would be a poor trade. Rotating that key is a core release — the keyring
is compiled into the binary — so a leak is unusually expensive.

```sh
gh pr checkout <number> --repo tornis-tecnologia/veloxsearch-registry
velox sign integrations/<id> --key ~/path/to/velox-registry-2026.priv.pem
git commit -am "sign: <id>" && git push
```

`velox sign` verifies the signature it just produced before writing, so a
package that would not check out never reaches the disk. The registry's CI
refuses to merge anything still carrying the placeholder.

**Changing a core recipe does not require doing this by hand.** A push to `main`
touching `src/recipes.rs`, `src/agents.rs` or `src/integrations.rs` runs
`registry-sync.yml`, which regenerates the assets and opens the pull request for
you — unsigned. You sign it as above.

See [integrations/signing.md](integrations/signing.md) and
[`keys/README.md`](../keys/README.md).

## Release checklist

Everything below the line is the workflow's job. Yours is the pull request.

- [ ] `Cargo.toml` version bumped, `Cargo.lock` updated
- [ ] `CHANGELOG.md` section written for a reader, breaking changes called out
- [ ] `deploy/install.yaml` image tag bumped to the new version
- [ ] CI green on the release PR

---

- [x] Re-verified at the release commit
- [x] Image built, pushed and signed
- [x] Tag created, release published with a digest-pinned `install.yaml`

Afterwards, worth a glance: `cosign verify` on the published image
([docs/INSTALL.md §5b](INSTALL.md)), and that the release asset applies cleanly
on a scratch cluster.

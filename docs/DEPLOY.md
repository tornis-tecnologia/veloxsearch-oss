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

1. **Bump the version.** `version` in `Cargo.toml`, then `cargo check` so
   `Cargo.lock` follows.
2. **Update `CHANGELOG.md`.** Move `Unreleased` entries under the new version
   with the date. Breaking changes get their own callout — this project is
   pre-1.0, so a minor version may break, and the changelog is the only place
   that says so.
3. **Pin the image in `deploy/install.yaml`** to the new version tag.
4. **Open a release PR** with those three changes. CI must be green.
5. **Merge, then tag:**

   ```sh
   git tag -s v0.7.1 -m "v0.7.1"
   git push origin v0.7.1
   ```

6. **Build and push the image:**

   ```sh
   deploy/build-image.sh --profile release --push
   ```

   The script prints the resulting `RepoDigests` entry.

7. **Pin the digest.** Replace the tag in `deploy/install.yaml` with
   `<image>@sha256:…` and push that as a follow-up commit. A tag can be moved;
   a digest cannot, and `kubectl apply` of a movable tag makes the running
   version a function of when the Pod last restarted.

8. **Create the GitHub release** against the tag, with the changelog section as
   the body.

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
[`veloxsearch-registry`](https://github.com/tornis-tecnologia/veloxsearch-registry)
and are signed at publish time with the private half of the key in
[`keys/`](../keys/README.md). A contributor never needs that key: proposing an
integration is a pull request against the registry repo, and a maintainer signs
it. See [integrations/signing.md](integrations/signing.md).

## Release checklist

- [ ] `Cargo.toml` version bumped, `Cargo.lock` updated
- [ ] `CHANGELOG.md` section written, breaking changes called out
- [ ] `deploy/install.yaml` pinned to the new version
- [ ] CI green on the release PR
- [ ] Signed tag pushed
- [ ] Image built and pushed
- [ ] `deploy/install.yaml` re-pinned to the digest
- [ ] GitHub release published
- [ ] The quickstart in both READMEs still installs the right thing

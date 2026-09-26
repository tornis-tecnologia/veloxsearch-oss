# The `velox` installer CLI

`velox` is a small operator CLI whose install-time job is the step
`kubectl apply -f install.yaml` cannot do by itself: creating an image-pull
Secret **before** applying the manifest.

**Most installs do not need it.** The default image is public and pulls
anonymously, so the one-command install in the [README](../README.md) is the
whole story. Reach for `velox` when you are pulling VeloxSearch from a private
mirror.

## Installing

Each release publishes a `velox-linux-amd64` binary next to `install.yaml`, with
both checksums in `SHA256SUMS`:

```sh
base=https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download
curl -fsSLO "$base/velox-linux-amd64" && curl -fsSLO "$base/SHA256SUMS"
sha256sum --ignore-missing -c SHA256SUMS      # velox-linux-amd64: OK
install -m 0755 velox-linux-amd64 ~/.local/bin/velox
```

The binary is linux-amd64 only. On any other platform, build it from source:

```sh
cargo build --release --bin velox     # target/release/velox
```

## `velox init`

```
velox init [OPTIONS]

  --pull-token <TOKEN>       DEPRECATED — creates a Secret that has no
                             effect on the default catalog. Prefer the manual
                             Secret (docs/SECRETS.md). When given, velox
                             creates a kubernetes.io/dockerconfigjson Secret in
                             veloxsearch-system BEFORE applying the manifest.
                             Omit it to install with no secret.
  --pull-user <USER>         Registry username             [default: veloxsearch]
  --registry <HOST>          Registry host the Secret authenticates to
                                                           [default: docker.io]
  --pull-secret-name <NAME>  Name of the created Secret     [default: velox-pull]
  --dry-run                  Print what would be applied — the manifest objects
                             and the pull Secret — without touching a cluster.
  -h, --help
```

The manifest it applies is `deploy/install.yaml`, compiled into the binary. The
CLI and the manifest are therefore always the same version, and there is no
"which install.yaml did you use?" question to answer during support.

## Private-mirror install

```sh
# 1. create the pull Secret manually (canonical — docs/SECRETS.md):
kubectl -n veloxsearch-system create secret docker-registry velox-pull \
  --docker-server=registry.example.com --docker-username=velox-deploy \
  --docker-password="$TOKEN"

# 2. apply the manifest:
velox init --registry registry.example.com
```

`velox init --pull-token "$TOKEN" --registry …` still applies the manifest and
creates the Secret, but the flag is **deprecated** (no effect on the default
catalog) and prints a deprecation warning. The manifest's `veloxsearch`
ServiceAccount does **not** reference `velox-pull` — it ships without
`imagePullSecrets`, because the default image is public — so attach the
Secret, then point the Deployment at your mirror:

```sh
kubectl -n veloxsearch-system patch serviceaccount veloxsearch \
  -p '{"imagePullSecrets":[{"name":"velox-pull"}]}'
kubectl -n veloxsearch-system set image deploy/veloxsearch \
  veloxsearch=registry.example.com/veloxsearch-oss:0.10.2
```

Patch the ServiceAccount first: a pod takes its ServiceAccount's pull secrets
when it is created, and `set image` is what creates the new pod.

## Seeing what it will do

```sh
velox init --dry-run
velox init --dry-run | less
# (the pull Secret is created manually — docs/SECRETS.md)
```

`--dry-run` lists every object it would apply — as `Kind/name (namespace)`, plus
the pull Secret when `--pull-token` is given, with the token redacted — and
touches no cluster. It prints names, not YAML: for the full manifest, read the
`install.yaml` asset of the same release.

## Managing the pull Secret with External Secrets

If you already run the External Secrets Operator, you do not need
`--pull-token` at all: `deploy/secrets/external-secrets.aws.example.yaml`
contains a worked `ExternalSecret` that materialises `velox-pull` from a vault
entry. Apply that instead, then apply the release's manifest
(`kubectl apply -f https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download/install.yaml`),
not `deploy/install.yaml` from a checkout, and attach the Secret to the
ServiceAccount as in [Private-mirror install](#private-mirror-install).

The vault entry is a JSON object:

```json
{"user": "velox-deploy", "token": "…"}
```

## What `velox init` does not do

- It does not bootstrap the cluster. cert-manager, the OpenSearch operator and
  Longhorn are installed by the app itself on first run — see
  [PREMISES.md](PREMISES.md).
- It does not create the admin account. That happens on the first-run screen.
- It does not upgrade anything. Rolling out a new version is applying that
  release's `install.yaml` artifact; see
  [DEPLOY.md](DEPLOY.md#rolling-out-an-upgrade).

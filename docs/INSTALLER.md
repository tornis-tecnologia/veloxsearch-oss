# The `velox` installer CLI

`velox` is a small operator CLI whose only job is the step
`kubectl apply -f install.yaml` cannot do by itself: creating an image-pull
Secret **before** the manifest that needs it.

**Most installs do not need it.** The default image is public and pulls
anonymously, so the two-command quickstart in the [README](../README.md) is the
whole story. Reach for `velox` when you are pulling VeloxSearch from a private
mirror.

## Installing

```sh
cargo build --release --bin velox     # target/release/velox
```

## `velox init`

```
velox init [OPTIONS]

  --pull-token <TOKEN>       Registry password or token. When given, velox
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
velox init \
  --registry registry.example.com \
  --pull-user velox-deploy \
  --pull-token "$TOKEN"
```

This creates the `velox-pull` Secret in `veloxsearch-system`, then applies the
manifest. The ServiceAccount references `velox-pull` in `imagePullSecrets`, so
the Deployment can pull immediately — no ordering race, no ImagePullBackOff on
first apply.

To use a mirrored image, also change the `image:` line in
`deploy/install.yaml`, or patch it after applying:

```sh
kubectl -n veloxsearch-system set image deploy/veloxsearch \
  veloxsearch=registry.example.com/veloxsearch-oss:0.7.0
```

## Seeing what it will do

```sh
velox init --dry-run
velox init --dry-run --pull-token fake | less
```

`--dry-run` renders every object, including the pull Secret, and touches no
cluster. Use it to diff against what is already installed, or to hand the
manifest to a GitOps pipeline instead of applying it directly.

## Managing the pull Secret with External Secrets

If you already run the External Secrets Operator, you do not need
`--pull-token` at all: `deploy/secrets/external-secrets.aws.example.yaml`
contains a worked `ExternalSecret` that materialises `velox-pull` from a vault
entry. Apply that instead, then `kubectl apply -f deploy/install.yaml`.

The vault entry is a JSON object:

```json
{"user": "velox-deploy", "token": "…"}
```

## What `velox init` does not do

- It does not bootstrap the cluster. cert-manager, the OpenSearch operator and
  Longhorn are installed by the app itself on first run — see
  [PREMISES.md](PREMISES.md).
- It does not create the admin account. That happens on the first-run screen.
- It does not upgrade anything. Rolling out a new version is
  `kubectl apply -f deploy/install.yaml`; see [DEPLOY.md](DEPLOY.md).

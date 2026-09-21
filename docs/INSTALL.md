# Installing VeloxSearch — minikube · k0s · k3s · vanilla k8s

This is the **canonical install guide.** On a cluster you already have, installing
is one command — the image is published to a **public registry**, so the cluster
pulls it anonymously and there is no client binary to install:

```bash
kubectl apply -f https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download/install.yaml
# open http://<node-ip>:30080 — the UI answers there on ANY cluster, ingress or
# not; port-forward and Ingress remain as alternatives
kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80
# open http://localhost:3000 — first run: create the admin account
```

No cluster yet? Start at [§0](#0-no-kubernetes-cluster-yet). Pulling from a
private mirror instead of the public image? That is the one case the `velox` CLI
exists for — see [§2b](#2b-private-mirror-authenticated-pull--alternative) and
[`INSTALLER.md`](INSTALLER.md).

This guide also covers the four supported single-cluster shapes and, for clusters
that can't reach the public image, an offline **side-load** alternative.

VeloxSearch ships as **one manifest** (`deploy/install.yaml`, ADR-027): the
`veloxsearch-system` and `velox-agents` namespaces, the service account +
two-phase RBAC, the wizard Deployment and its Service (a NodePort on 30080 —
the UI answers at `http://<node-ip>:30080/` on any cluster, #87), a small
bundled Postgres StatefulSet, and a catch-all Ingress with no host and no
`ingressClassName`. On a cluster with a default IngressClass (a fresh k3s, say)
that Ingress answers on `http://<node-ip>/`; on a cluster without one it stays
inert and the NodePort is the way in. On first run the app checks your cluster
against
[`REQUIREMENTS.md`](REQUIREMENTS.md) (R1–R8) and self-installs cert-manager + the
OpenSearch operator. Longhorn — the only supported deployment storage (R3,
ADR-043) — is installed when you create your first deployment, unless it is
already there.

> **Read first:** [`REQUIREMENTS.md`](REQUIREMENTS.md) is the platform contract.
> Everything below assumes Kubernetes ≥ 1.30, amd64 nodes, ≥ 8 GiB schedulable
> RAM + 2 vCPU, and outbound registry egress. If your cluster misses a
> requirement, the app says so at the conformity screen rather than half-installing.

---

## 0. No Kubernetes cluster yet?

VeloxSearch installs *into* a Kubernetes cluster; it does not create one. The
shortest path from a bare machine:

- **A Linux server or VM** (amd64, ≥ 12 GiB RAM / 4 vCPU recommended — see R4):
  install single-node k3s with its official one-liner, then follow
  [§3b](#3b-k3s-verified--k3s-greenfield) for the Longhorn node packages and
  the kubeconfig:

  ```bash
  curl -sfL https://get.k3s.io | sh -    # k3s quick start: https://docs.k3s.io/quick-start
  ```

- **A laptop** with Docker or a hypervisor: minikube, sized above the R4 floor —
  [§3a](#3a-minikube) has the exact `minikube start` line.

Either way, once `kubectl get nodes` shows a `Ready` node, the one-command install
at the top of this page is all that is left.

---

## 1. Supported platforms

| Platform | Default StorageClass | Longhorn installed? | Ingress out of the box | Conformance status |
|---|---|---|---|---|
| **minikube** | `standard` (`k8s.io/minikube-hostpath`) — node-local | **Yes**, at first deployment create (R3) | No (addon: `minikube addons enable ingress`) | Install-and-boot smoke-tested on every push to `main` (CI lane `Smoke (minikube)`); not a full conformance run |
| **k0s** (bare, `--single`) | none | **Yes**, at first deployment create (R3) | No (port-forward only, R8) | In the conformance fleet (`k0s-bare`) |
| **k3s** | `local-path` (`rancher.io/local-path`) — node-local | **Yes**, at first deployment create (R3) | Traefik is the default IngressClass, so the manifest's catch-all Ingress answers on `http://<node-ip>/` | In the conformance fleet (`k3s-greenfield`, a 3-node Longhorn cluster, and the `k3s-undersized` refusal fixture) |
| **vanilla k8s** (kubeadm/EKS/GKE/AKS) | depends on the cluster | **Yes**, at first deployment create, unless a `longhorn` StorageClass already exists (R3) | depends on the cluster | Documented, **expected** (kubeadm/EKS/GKE/AKS untested) |

What each fleet row last proved, and on which version, is recorded per row in
[`REQUIREMENTS.md`](REQUIREMENTS.md#tested-platforms-the-conformance-fleet-adr-026) — this table does not repeat
the dates so it cannot drift from them.

**How storage works (R3 / ADR-043, amending ADR-031).** Longhorn is the **only
supported deployment storage**: OpenSearch PVCs are pinned to the `longhorn`
StorageClass, whatever your cluster's default is.

- **A `longhorn` StorageClass already present** → used as-is; nothing is installed.
- **Anything else** — a node-local default (`rancher.io/local-path`, hostpath,
  minikube-hostpath), no default at all, **or a foreign CSI default** (EBS, PD,
  Azure Disk, Ceph…) → VeloxSearch installs Longhorn when you create your first
  deployment. It informs rather than asks: the wizard's review step says the
  install will happen, and shows its progress while it runs.

> **Longhorn node prerequisites:** every node needs `open-iscsi` (with `iscsid`
> running), an NFS client and `dmsetup`. A node missing one is named in the UI
> with the install command for its distro, and deployment creation is refused
> until Longhorn is usable — PVCs are never left `Pending`. On Debian/Ubuntu,
> for `open-iscsi`:
> `sudo apt-get install -y open-iscsi && sudo systemctl enable --now iscsid`.

**Verified vs expected.** **k3s** and **k0s** have live conformance fixtures
that drive the journey end-to-end (install → first-run → bootstrap → create
deployment). **minikube** has continuous install-and-boot evidence from CI but no
full-journey run, and **vanilla k8s** is documented from the same contract without
having been run through the fleet — treat those command blocks as
expected-correct, not conformance-proven.

---

## 2. Getting the image onto the cluster

### 2a. Public pull (the default) — zero credentials

The image is published to a **public registry**, so a cluster with outbound
registry egress pulls it anonymously — no namespace to pre-create, no pull secret,
no client binary. Apply the release manifest; it creates its own namespaces,
ServiceAccount and RBAC, and the kubelet pulls the public image:

```bash
kubectl apply -f https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download/install.yaml
kubectl -n veloxsearch-system rollout status deployment/veloxsearch --timeout=300s
```

Works from any workstation `kubectl` runs on — macOS and arm64 laptops included;
only the cluster's nodes need to be amd64 (R5).

### 2b. Private mirror (authenticated pull) — alternative

If you mirror the image into a **private** registry (e.g. your own
`registry.example.com/...` project), the cluster needs a credential to pull it.
The release manifest's ServiceAccount carries **no** `imagePullSecrets` (the
default image is public), so create the pull Secret, attach it to the
ServiceAccount, and point the Deployment at your mirror:

```bash
kubectl create namespace veloxsearch-system    # the manifest also creates it
kubectl -n veloxsearch-system create secret docker-registry velox-pull \
  --docker-server=registry.example.com \
  --docker-username=<deploy-token-username> \
  --docker-password=<deploy-token>
kubectl apply -f https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download/install.yaml
kubectl -n veloxsearch-system patch serviceaccount veloxsearch \
  -p '{"imagePullSecrets":[{"name":"velox-pull"}]}'
kubectl -n veloxsearch-system set image deploy/veloxsearch \
  veloxsearch=registry.example.com/veloxsearch-oss:0.10.2
```

The ServiceAccount patch must come before `set image`: a pod picks up its
ServiceAccount's pull secrets when it is created, and `set image` is what creates
the new pod. create the same `velox-pull` Secret manually (see
[SECRETS.md](SECRETS.md#private-registry-pull-credentials); the
`velox init --pull-token` path is deprecated and has no effect on the
default catalog)
before applying the manifest it was built with; the ServiceAccount patch and the
image change are still yours to do — see [`INSTALLER.md`](INSTALLER.md).

### 2c. Side-load (offline / air-gapped) — no registry at all

Build or obtain the image tarball, then import it into each platform's container
runtime. The tag you import must be **exactly** the `image:` reference of the
manifest you apply — the kubelet looks the image up by that name.
`deploy/build-image.sh` tags `docker.io/tornistecnologia/veloxsearch-oss:<version>`
by default, `<version>` being the one in `Cargo.toml`:

```bash
deploy/build-image.sh                                              # see DEPLOY.md
docker save docker.io/tornistecnologia/veloxsearch-oss:0.10.2 -o veloxsearch.tar
grep -n 'image: docker.io/tornistecnologia' deploy/install.yaml     # must name the same tag
```

The release manifest pins the image by **digest**, which a locally built image
does not carry — for a side-load, apply a manifest whose `image:` line names the
tag you imported.

Import per platform:

| Platform | Side-load command |
|---|---|
| **minikube** | `minikube image load veloxsearch.tar` |
| **k3s** | `sudo k3s ctr -n k8s.io images import veloxsearch.tar` |
| **k0s** | `sudo k0s ctr -n k8s.io images import veloxsearch.tar` |
| **vanilla k8s** | per node: `sudo ctr -n k8s.io images import veloxsearch.tar` (or `sudo nerdctl -n k8s.io load -i veloxsearch.tar`) — run on **every** schedulable node, or push to a registry the cluster can pull |

The Deployment uses `imagePullPolicy: IfNotPresent`, so once that tag is present
in the runtime the pod schedules against the local image without any registry
contact. The bundled Postgres (`docker.io/library/postgres:16-alpine`) is pulled
the same way and needs side-loading too.

---

## 3. Per-platform quick start

Every platform follows the same shape: **apply the manifest → port-forward → open
the wizard**. With public-image pull (§2a) you skip straight to `kubectl apply`;
the per-platform blocks below also show the **side-load** import command for
air-gapped clusters (§2c). Differences are the default StorageClass and how you
reach the cluster.

### 3a. minikube

```bash
# Resource floor: R4 needs >=8Gi schedulable RAM + 2 vCPU (3x2Gi OpenSearch +
# Dashboards + operator + cert-manager + agent). Recommended 12Gi/4 vCPU/60GB.
# minikube reserves overhead, so size the VM above the floor:
minikube start --memory=12288 --cpus=4 --disk-size=60g

# Default SC is `standard` (k8s.io/minikube-hostpath) = node-local ⇒ the wizard
# bootstraps Longhorn at first deployment create. Longhorn needs open-iscsi on
# the node:
minikube ssh -- 'sudo apt-get update && sudo apt-get install -y open-iscsi && sudo systemctl enable --now iscsid'

# Side-load the image INTO the minikube node (the host docker daemon is NOT the
# cluster runtime):
minikube image load veloxsearch.tar

kubectl apply -f deploy/install.yaml
kubectl -n veloxsearch-system rollout status deployment/veloxsearch --timeout=120s

kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80
# open http://localhost:3000  → first run: /setup
```

minikube gotchas:
- **`minikube image load` is mandatory** — an image only in your host docker
  daemon is invisible to the cluster.
- For **ingress mode** instead of port-forward: `minikube addons enable ingress`,
  then keep `minikube tunnel` running so the IngressClass `nginx` gets an address.

### 3b. k3s (verified — `k3s-greenfield`)

```bash
# kubeconfig: k3s writes /etc/rancher/k3s/k3s.yaml
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml      # or copy it to ~/.kube/config

# Default SC is local-path (rancher.io/local-path) = node-local ⇒ Longhorn
# bootstrap. Install the prereq on every node:
sudo apt-get install -y open-iscsi && sudo systemctl enable --now iscsid

sudo k3s ctr -n k8s.io images import veloxsearch.tar

kubectl apply -f deploy/install.yaml
kubectl -n veloxsearch-system rollout status deployment/veloxsearch --timeout=120s

kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80
# open http://localhost:3000  → first run: /setup
```

The `k3s-greenfield` fixture (single node) runs this path: install → R1–R8 ✓ →
cert-manager + operator auto-installed → Longhorn installed at first create from
the local-path default. A 3-node k3s cluster that already has a `longhorn`
StorageClass skips the Longhorn install. What each run last proved is in
[`REQUIREMENTS.md`](REQUIREMENTS.md#tested-platforms-the-conformance-fleet-adr-026).

### 3c. k0s (verified — `k0s-bare`)

```bash
# kubeconfig:
sudo k0s kubeconfig admin > ~/.kube/config
export KUBECONFIG=~/.kube/config

# A bare `k0s --single` has NO default StorageClass ⇒ Longhorn bootstrap.
# Install the prereq on every node:
sudo apt-get install -y open-iscsi && sudo systemctl enable --now iscsid

sudo k0s ctr -n k8s.io images import veloxsearch.tar

kubectl apply -f deploy/install.yaml
kubectl -n veloxsearch-system rollout status deployment/veloxsearch --timeout=120s

# Bare k0s has no ingress controller ⇒ port-forward is the only access mode (R8):
kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80
# open http://localhost:3000  → first run: /setup
```

The `k0s-bare` fixture (single node) runs this path: absent-default Longhorn
install + port-forward-only honesty (no IngressClass ⇒ the UI offers only
port-forward, and the manifest's catch-all Ingress stays inert). What each run
last proved is in
[`REQUIREMENTS.md`](REQUIREMENTS.md#tested-platforms-the-conformance-fleet-adr-026).

### 3d. vanilla k8s (kubeadm / EKS / GKE / AKS — expected, untested)

```bash
# Use your existing kubeconfig (cloud CLI, kubeadm admin.conf, etc.).

# Image: the default is the public pull (§2a) — `kubectl apply` and the kubelet
# fetches it. Air-gapped only? Side-load on EVERY schedulable node first (§2c):
#   sudo ctr -n k8s.io images import veloxsearch.tar
#   (or: sudo nerdctl -n k8s.io load -i veloxsearch.tar)

# Storage: Longhorn is the only supported deployment storage (R3, ADR-043) — a
# managed cloud default (gp2/gp3, pd-*, managed-csi) is NOT used; Longhorn is
# installed at first create unless a `longhorn` StorageClass already exists.
# Install open-iscsi, an NFS client and dmsetup on every node first.

kubectl apply -f deploy/install.yaml
kubectl -n veloxsearch-system rollout status deployment/veloxsearch --timeout=120s

kubectl -n veloxsearch-system port-forward svc/veloxsearch 3000:80
# open http://localhost:3000  → first run: /setup
```

Managed clusters (EKS/GKE/AKS) typically also have a real LoadBalancer/IngressClass,
so ingress mode is available in the Settings tab once you supply a domain (R8).
This shape is **expected-correct but not conformance-tested** — verify against
[`REQUIREMENTS.md`](REQUIREMENTS.md) on first use.

---

## 4. First run

A 24-second recording of this whole flow — setup, conformity, a green deployment,
the create wizard — is at [`.github/assets/demo.gif`](../.github/assets/demo.gif)
(recorded against a mock API; nothing is provisioned in it).

1. **Open the wizard** at `http://localhost:3000` (or your ingress host).
2. **`/setup`** — create the admin account on first boot (ADR-023). The password
   is bcrypt-hashed into the `veloxsearch-credentials` Secret; there are no env
   credentials baked into the manifest. Sessions survive pod restarts.
3. **Conformity probe** — the app checks R1–R8 ([`REQUIREMENTS.md`](REQUIREMENTS.md))
   and renders each as ✓ / ⚠ / ✗ with remediation text. A missing `longhorn`
   StorageClass shows as a remediation (VeloxSearch will install Longhorn), not
   a failure. Any hard ✗ (e.g. Kubernetes < 1.30, < 8 GiB RAM, arm64, a foreign
   operator) makes the installer **refuse to start** rather than half-install.
4. **Self-bootstrap** — once the probe passes, the app installs cert-manager +
   the OpenSearch operator from vendored bundles (`deploy/bootstrap/`), and
   Longhorn if needed. This needs the one-time `veloxsearch-bootstrap`
   cluster-admin binding, which the app **revokes itself** when bootstrap
   completes (ADR-027). Re-apply `install.yaml` only if you ever need to
   re-bootstrap (e.g. a component upgrade).
5. **Create your first deployment** — a four-step wizard (ADR-053):
   - **Purpose** — the name, the OpenSearch version, and what the deployment is
     for: Observability, Security or Search.
   - **Size** — a preset (small / medium / large) or a custom size. Every
     deployment is 3 nodes for quorum; presets vary memory, heap and disk.
   - **Snapshot** — an optional S3 snapshot repository. Skipping it costs
     nothing; the deployment's Backup tab configures it later.
   - **Review** — for Observability and Security deployments, a default-checked
     **K3S monitoring (this cluster)** toggle. Leave it on to ship the Kubernetes
     log collector with the deployment; uncheck it and the deployment is created
     with no monitors — install them later from its Integrations tab, and the
     overview says "no monitors installed" until you do (#52).

   Creation is gated on storage-ready (this is when Longhorn is installed, if it
   is missing); OpenSearch then comes up node by node and turns green at the end.

---

## 5. Bring your own domain + TLS certificate (issue #54)

Ingress mode is fully client-owned: **your domain, your certificate, any
issuer**. Nothing here depends on the cert-manager the app self-bootstraps
(that instance serves the OpenSearch operator's webhook certs) — but it can
issue the dashboards certificate too, if you want it to.

**Domain.** In *Settings → Dashboard access* pick **Ingress**, set your
**base domain** and the IngressClass detected on your cluster. Each deployment
is published at `https://<deployment>.<base-domain>`; point a wildcard DNS
record (`*.<base-domain>`) at your ingress controller / load balancer.

**Certificate — three equivalent ways to provide one** (all end in a
`kubernetes.io/tls` Secret in the app namespace that every dashboards Ingress
references via `spec.tls`):

```bash
# (a) Pre-created Secret — any PKI, no app involvement.
#     Use a wildcard cert (*.example.com): deployments share the base domain.
kubectl -n veloxsearch-system create secret tls veloxsearch-dashboards-tls \
  --cert=fullchain.pem --key=privkey.pem
# …then put "veloxsearch-dashboards-tls" in Settings → "TLS secret".
```

- **(b) Paste PEM in Settings** — fill the optional *TLS certificate* + *TLS
  private key* fields; the app creates/updates the Secret itself (named after
  the *TLS secret* field, default `veloxsearch-dashboards-tls`). Re-paste to
  rotate a renewed certificate. The PEM is stored only in the Secret — it is
  never echoed back by the API.
- **(c) Let an issuer maintain the Secret** — e.g. a cert-manager `Certificate`
  (with any `ClusterIssuer`: Let's Encrypt, Vault, your CA) whose
  `secretName` matches the name you set in Settings. Renewal is then the
  issuer's job; the Ingress keeps pointing at the same Secret.

**Default unchanged:** leave *TLS secret* empty and the Ingresses carry no
`spec.tls` at all — exactly the historical behavior, where TLS (if any) is
terminated by the edge (HAProxy, cloud LB) or the controller's default cert.

Changing the setting re-applies the Ingress of every existing deployment
immediately; no restart needed.

---

## 5b. Which manifest URL to use, and verifying what you pulled

Three places serve an install manifest. They are not equivalent:

| Source | What it is |
| --- | --- |
| `releases/latest/download/install.yaml` | **Use this.** A release artifact with the image pinned to a **digest**. Immutable: the same URL applied twice gives the same bytes and the same image |
| `releases/download/v0.10.2/install.yaml` | The same, pinned to one version instead of following the newest |
| `deploy/install.yaml` on `main` | The source the release is built from. The image is a version **tag**, not a digest, and `main` moves. Right for development, wrong for a cluster you care about |
| `https://get.veloxsearch.ai/install.yml` | A convenience redirect to `releases/latest/download/install.yaml`. The daily `Mirror watch` workflow fails if it stops redirecting there or its bytes differ from the release asset (#37) |

### Verifying the image

The image is signed at release time with [cosign](https://github.com/sigstore/cosign),
keyless: there is no signing key to steal, only a short-lived certificate bound
to the workflow that built it, recorded in the public Rekor transparency log.
Verifying is checking *which workflow, in which repository* produced the image:

```bash
cosign verify docker.io/tornistecnologia/veloxsearch-oss:0.10.2 \
  --certificate-identity-regexp '^https://github\.com/tornis-tecnologia/veloxsearch-oss/' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
```

A signature that verifies proves the image came out of this repository's release
workflow. It does **not** prove the image is free of bugs — provenance is not
quality.

Integration packages are signed separately, with an ed25519 key whose public
half is compiled into the binary; that check is not optional and not yours to
run — the control plane refuses an unsigned or tampered package before applying
it. See [`integrations/signing.md`](integrations/signing.md).

---

## 6. Status / honesty note

- **Default path:** the manifest references a **public image**, so
  `kubectl apply -f https://github.com/tornis-tecnologia/veloxsearch-oss/releases/latest/download/install.yaml`
  pulls it with **zero credentials** and no client binary, no namespace or pull
  secret to pre-create (§2a).
- **Air-gapped:** the **side-load + `kubectl apply`** path (§2c, §3) still works
  for clusters with no registry egress, and is what the conformance fleet runs.
- **Private mirror:** if you mirror the image into a private registry, create
  the `velox-pull` Secret by hand (see
[SECRETS.md](SECRETS.md#private-registry-pull-credentials)), attach it
  to the `veloxsearch` ServiceAccount — the manifest ships it without
  `imagePullSecrets` — and point the Deployment at your mirror (§2b).

See [`REQUIREMENTS.md`](REQUIREMENTS.md) for the full platform contract,
[`PREMISES.md`](PREMISES.md) for the three operational premises behind the
self-bootstrap (Longhorn / operator auto-install + per-deployment namespace),
[`DEPLOY.md`](DEPLOY.md) for the prod build/side-load/roll runbook, and
[`adr/README.md`](adr/README.md) for the ADRs referenced above.

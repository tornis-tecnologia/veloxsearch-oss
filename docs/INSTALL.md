# Installing VeloxSearch — minikube · k0s · k3s · vanilla k8s

This is the **canonical install guide.** It installs VeloxSearch onto a cluster you
already have, and it covers two distinct actions you should not conflate:

- **Install the `velox` CLI/wizard** onto your machine — the recommended start.
  One command fetches the CLI, checksum-verifies it, and puts it on your PATH; a
  second deploys VeloxSearch:

  ```bash
  curl -fsSL https://get.veloxsearch.ai/install.sh | sh   # installs the velox CLI
  velox init                                              # deploys VeloxSearch
  ```

- **Deploy OpenSearch in one command** — the dependency-free alternative, no client
  binary. The image is published to a **public registry**, so the cluster pulls it
  anonymously:

  ```bash
  kubectl apply -f https://get.veloxsearch.ai/install.yml
  ```

Both land the same thing: `velox init` server-side-applies the same manifest, waits
for the rollout, and prints the first-run URL; `kubectl apply` does the apply and
you port-forward yourself. The CLI is **linux-amd64** only today — on macOS/arm64
use the `kubectl apply` path, which has no client-side dependency.

This guide also covers the four supported single-cluster shapes and, for clusters
that can't reach the public image, an offline **side-load** alternative (and a
private authenticated-pull mirror).

VeloxSearch ships as **one manifest** (`deploy/install.yaml`, ADR-027): namespace
`veloxsearch-system`, the service account + two-phase RBAC, the wizard Deployment,
and a Service. It creates **no Ingress** — port-forward is the zero-assumption
default. On first run the app checks your cluster against
[`REQUIREMENTS.md`](REQUIREMENTS.md) (R1–R8) and self-installs cert-manager + the
OpenSearch operator (and Longhorn, if your default StorageClass is node-local or
absent — see R3).

> **Read first:** [`REQUIREMENTS.md`](REQUIREMENTS.md) is the platform contract.
> Everything below assumes Kubernetes ≥ 1.30, amd64 nodes, ≥ 8 GiB schedulable
> RAM + 2 vCPU, and outbound registry egress. If your cluster misses a
> requirement, the app says so at the conformity screen rather than half-installing.

---

## 1. Supported platforms

| Platform | Default StorageClass | Longhorn self-bootstrap? | Ingress out of the box | Conformance status |
|---|---|---|---|---|
| **minikube** | `standard` (`k8s.io/minikube-hostpath`) — node-local | **Yes** — node-local default ⇒ Longhorn installs (R3) | No (addon: `minikube addons enable ingress`) | Documented, **expected** (not in the conformance fleet) |
| **k0s** (bare, `--single`) | none | **Yes** — absent default ⇒ Longhorn installs (R3) | No (port-forward only, R8) | **Verified ✓** — `ct2-k0s-bare` (k0s v1.35.4) |
| **k3s** | `local-path` (`rancher.io/local-path`) — node-local | **Yes** — node-local default ⇒ Longhorn installs (R3) | Traefik present, but install defaults to port-forward | **Verified ✓** — `ct1-k3s-greenfield` (k3s v1.35.5); also live prod (3-node, real `longhorn` default ⇒ bootstrap no-ops) |
| **vanilla k8s** (kubeadm/EKS/GKE/AKS) | depends on the cluster | **Conditional** — a real CSI default is used as-is; a node-local/absent default ⇒ Longhorn installs | depends on the cluster | Documented, **expected** (kubeadm/EKS/GKE/AKS untested) |

**How the storage decision works (R3 / ADR-031).** The wizard inspects your
default StorageClass:

- **Real CSI default** (e.g. `longhorn`, EBS, PD, Azure Disk) → used as-is,
  Longhorn bootstrap is a no-op.
- **Node-local default** (`rancher.io/local-path`, hostpath, minikube-hostpath,
  openebs-local) **or no default at all** → VeloxSearch installs Longhorn so
  OpenSearch PVCs survive a pod reschedule.

> **Longhorn node prerequisite:** every node needs `open-iscsi` installed and
> `iscsid` running. Without it the storage-ready gate refuses (informatively)
> rather than leaving PVCs `Pending`. On Debian/Ubuntu:
> `sudo apt-get install -y open-iscsi && sudo systemctl enable --now iscsid`.

**Verified vs expected.** Only **k3s (ct1)** and **k0s (ct2)** have a live
conformance fixture that drives the whole journey end-to-end (install → first-run
→ bootstrap → create deployment → data in dashboards). **minikube** and **vanilla
k8s** are documented from the same contract and the `default_storage()` branch
logic, but have **not** been run through the fleet — treat their command blocks as
expected-correct, not conformance-proven.

---

## 2. Getting the image onto the cluster

### 2a. Public pull (the default) — zero credentials

The image is published to a **public registry**, so a cluster with outbound
registry egress pulls it anonymously — no namespace to pre-create, no pull secret.
Two ways to drive it.

**Primary — the `velox` CLI.** Install `velox` (linux-amd64; it verifies its own
checksum), then `velox init` server-side-applies the manifest, waits for the
Deployment rollout, and prints the first-run URL:

```bash
curl -fsSL https://get.veloxsearch.ai/install.sh | sh   # installs the velox CLI
velox init                                              # deploys VeloxSearch
```

The installer fetches `https://get.veloxsearch.ai/velox` and its `velox.sha256`,
verifies the checksum, makes it executable, and (with sudo if needed) drops it in
`/usr/local/bin`. To grab the binary by hand instead:

```bash
curl -fsSL https://get.veloxsearch.ai/velox -o velox && chmod +x velox && ./velox init
```

> **Platform:** `velox` is published for **linux-amd64** only today; macOS and
> arm64 are a future follow-up. On other platforms use the `kubectl apply` path
> below, which has no client-side dependency.

**No-dependency alternative — `kubectl apply`.** Skip the CLI and apply the
manifest directly; it creates its own `veloxsearch-system` namespace, ServiceAccount
and RBAC, and the kubelet pulls the public image:

```bash
kubectl apply -f https://get.veloxsearch.ai/install.yml
```

### 2b. Private mirror (authenticated pull) — alternative

If you mirror the image into a **private** registry (e.g. your own
`registry.gitlab.com/...` project), the cluster needs a credential to pull it.
Create a pull secret and let the manifest's ServiceAccount consume it:

```bash
kubectl create namespace veloxsearch-system    # idempotent; the manifest also creates it
kubectl -n veloxsearch-system create secret docker-registry velox-pull \
  --docker-server=registry.gitlab.com \
  --docker-username=<deploy-token-username> \
  --docker-password=<deploy-token>
kubectl apply -f https://get.veloxsearch.ai/install.yml
```

The one-command equivalent is `velox init --pull-token <token>` (with
`--registry` / `--pull-user` to match your mirror): it creates the `velox-pull`
Secret, server-side-applies the manifest, waits for the rollout, and prints the
next steps.

### 2c. Side-load (offline / air-gapped) — no registry at all

Build or obtain the image tarball, then import it into each platform's container
runtime. To produce the tar from a local build:

```bash
deploy/build-image.sh                       # builds veloxsearch:0.7.0 (see DEPLOY.md)
docker save veloxsearch:0.7.0 -o veloxsearch.tar
```

Import per platform:

| Platform | Side-load command |
|---|---|
| **minikube** | `minikube image load veloxsearch.tar` |
| **k3s** | `sudo k3s ctr -n k8s.io images import veloxsearch.tar` |
| **k0s** | `sudo k0s ctr -n k8s.io images import veloxsearch.tar` |
| **vanilla k8s** | per node: `sudo ctr -n k8s.io images import veloxsearch.tar` (or `sudo nerdctl -n k8s.io load -i veloxsearch.tar`) — run on **every** schedulable node, or push to a registry the cluster can pull |

The Deployment uses `imagePullPolicy: IfNotPresent`, so once `veloxsearch:0.7.0`
is present in the runtime, `kubectl apply -f deploy/install.yaml` schedules
against the local image without any registry contact.

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

### 3b. k3s (verified — ct1)

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

Conformance-verified on `ct1-k3s-greenfield` (k3s v1.35.5, single node): install →
all R1–R8 ✓ → cert-manager + operator auto-installed → Longhorn bootstrapped from
the local-path default → deployment green with 3 OpenSearch pods co-scheduled on
one node. The live 3-node prod cluster has a real `longhorn` default, so the
bootstrap no-ops there.

### 3c. k0s (verified — ct2)

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

Conformance-verified on `ct2-k0s-bare` (k0s v1.35.4, single node): absent-default
Longhorn bootstrap path + port-forward-only honesty (no IngressClass ⇒ the UI
offers only port-forward).

### 3d. vanilla k8s (kubeadm / EKS / GKE / AKS — expected, untested)

```bash
# Use your existing kubeconfig (cloud CLI, kubeadm admin.conf, etc.).

# Image: the default is the public pull (§2a) — `kubectl apply` and the kubelet
# fetches it. Air-gapped only? Side-load on EVERY schedulable node first (§2c):
#   sudo ctr -n k8s.io images import veloxsearch.tar
#   (or: sudo nerdctl -n k8s.io load -i veloxsearch.tar)

# Storage: a managed cloud default (gp2/gp3, pd-*, managed-csi) is a real CSI
# default ⇒ used as-is, no Longhorn. A bare kubeadm cluster usually has NO
# default SC ⇒ Longhorn bootstraps; install open-iscsi on every node first.

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

1. **Open the wizard** at `http://localhost:3000` (or your ingress host).
2. **`/setup`** — create the admin account on first boot (ADR-023). The password
   is bcrypt-hashed into the `veloxsearch-credentials` Secret; there are no env
   credentials baked into the manifest. Sessions survive pod restarts.
3. **Conformity probe** — the app checks R1–R8 ([`REQUIREMENTS.md`](REQUIREMENTS.md))
   and renders each as ✓ / ⚠ / ✗ with remediation text. A node-local/absent
   StorageClass shows as a remediation ("VeloxSearch will install Longhorn"), not
   a failure. Any hard ✗ (e.g. Kubernetes < 1.30, < 8 GiB RAM, arm64, a foreign
   operator) makes the installer **refuse to start** rather than half-install.
4. **Self-bootstrap** — once the probe passes, the app installs cert-manager +
   the OpenSearch operator from vendored bundles (`deploy/bootstrap/`), and
   Longhorn if needed. This needs the one-time `veloxsearch-bootstrap`
   cluster-admin binding, which the app **revokes itself** when bootstrap
   completes (ADR-027). Re-apply `install.yaml` only if you ever need to
   re-bootstrap (e.g. a component upgrade).
5. **Create your first deployment** — name + size preset + purpose
   (Observability / Security / Search). Creation is gated on storage-ready;
   OpenSearch comes up green (3 nodes for quorum), and selected recipes ship
   their collection agents + out-of-the-box dashboards.

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

## 6. Status / honesty note

- **Default path:** the manifest references a **public image**, so the `velox` CLI
  (`curl … | sh` then `velox init`) — or, with no client binary,
  `kubectl apply -f https://get.veloxsearch.ai/install.yml` — pulls it with **zero
  credentials**, no namespace or pull secret to pre-create (§2a).
- **Air-gapped:** the **side-load + `kubectl apply`** path (§2c, §3) still works
  for clusters with no registry egress, and is what the conformance fleet runs.
- **Private mirror:** if you mirror the image into a private registry, the
  manifest's `veloxsearch` ServiceAccount carries `imagePullSecrets: [velox-pull]`,
  and `velox init --pull-token` (or a `kubectl create secret docker-registry`)
  supplies the credential (§2b).

See [`REQUIREMENTS.md`](REQUIREMENTS.md) for the full platform contract,
[`PREMISES.md`](PREMISES.md) for the three operational premises behind the
self-bootstrap (Longhorn / operator auto-install + per-deployment namespace),
`DEPLOY.md` for the prod build/side-load/roll runbook, and `DECISIONS.md` for
the ADRs referenced above. Those two live in the GitLab source repository and
are deliberately not part of the public export — `DEPLOY.md` is an internal
runbook, and the ADR log is pending a redaction pass because it carries live
client infrastructure detail. ADR numbers are cited inline throughout these
docs so the reasoning is still traceable once it is published.

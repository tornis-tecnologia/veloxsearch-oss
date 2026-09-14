# External install validation

*[Leia em português](EXTERNAL-VALIDATION.pt-BR.md)*

Every install and upgrade of VeloxSearch so far has been run by the people who
built it. They know where the sharp edges are and step around them without
noticing. This round asks people who have never used VeloxSearch to install it
from the README alone and tell us, critically, what happened.

The question is not whether an expert can get it running. It is whether a
newcomer can install it, reach the UI and get a green deployment on their own.

The round is tracked in
[#60](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/60). There are
two roles; you can take one or both.

- **Install testers** — the [install run](#the-install-run) below.
- **Security reviewers** — [a separate section](#security-review) at the end.

---

## Who this is for

You are a good install tester if you are comfortable with `kubectl`, can stand
up a single-node k3s or minikube cluster from its own documentation, and know how
to read a pod's logs. **You do not need to know anything about VeloxSearch or
OpenSearch** — no context is what we are testing for.

## What you need

- **A cluster you own and can throw away.** Not a shared cluster, not
  production. Installing VeloxSearch uses cluster-admin once and installs
  cluster-wide components (cert-manager, the OpenSearch operator, Longhorn).
- **One amd64 machine or VM**, ideally 4 vCPU, 12 GiB RAM and 60 GB disk.
- **Outbound internet access** from the cluster to pull images.
- A text file for notes, and a clock.

The exact requirements are part of what the README should tell you, so the list
above is deliberately short.

---

## The rules

1. **Start from [`README.md`](../README.md) only.** Follow any link it gives you
   when you decide you need it, and note which document you opened and why.
2. **Write down every point where you got stuck, had to guess, or opened the
   source code** — or searched the issue tracker or the web. Put a timestamp on
   each one. These notes are the most valuable part of your report.
3. **Do not ask the team during the run.** If you are stuck, write it down, then
   either find your own way past or stop. A run that stopped is a result, and
   where it stopped is exactly what we need to know.
4. **Say when you used outside knowledge.** If you got past something because
   you already know Longhorn, cert-manager or your distribution well, say so — a
   newcomer would not have.
5. **Be critical.** Precise friction helps more than a kind summary.

---

## The install run

Run the scenarios in order on the same cluster; each one starts where the
previous one ended. Write down the time you start and finish each one.

### 1. Fresh install

Create a fresh single-node **k3s** or **minikube** cluster using that
distribution's own instructions, and record its version. Then install VeloxSearch
starting from the README.

**Done when** you have created the admin account, the conformity screen has
passed, and you can see the main tabs.

Note which install path you took, what the conformity screen reported, and
anything you had to install on the node yourself.

### 2. Create an observability deployment

Using the UI, create a deployment with the **Observability** purpose and the
smallest size.

**Done when** the deployment shows green and you can open its Dashboards. The
time from your first install command to this point is your *time to first green
deployment*.

If it stalls, capture what the activity screen says before you dig further.

### 3. Install an integration

From the deployment's **Integrations** tab, install one integration.

**Done when** data from that integration is visible in Dashboards.

### 4. Uninstall

Remove what you installed, in reverse: the integration, then the deployment, then
VeloxSearch itself.

**Done when** you believe the cluster is back to how it was before scenario 1.

If the docs do not tell you how to do a step, that is a finding: record what you
tried, and what was still in the cluster afterwards (namespaces, CRDs, volumes).

### 5. Upgrade — *pending [#54](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/54)*

Do **not** run this scenario yet. The upgrade contract is being defined in #54;
once it lands, this section will say which release to install first and where the
upgrade instructions are.

---

## What to capture

**Environment**

- Distribution and version, and the output of `kubectl version`
- Node count; CPU, RAM and disk per node; OS and kernel version
- The VeloxSearch release you installed — if you used `releases/latest`, the tag
  it resolved to

**Timeline** — timestamps for: first install command, UI reachable, admin
account created, conformity passed, main tabs visible, deployment green,
integration data visible.

**Friction log** — one entry per stuck/guess/source point:

```
00:31 · <document and section> · expected <…> · got <…> · guessed / stuck / opened source · how I got past it
```

**Evidence** for anything that failed — the error text or a screenshot, and:

```bash
kubectl -n veloxsearch-system logs deploy/veloxsearch --tail=200
kubectl get storageclass
kubectl get nodes -o wide
```

Redact credentials, tokens and internal hostnames before you share anything.

---

## How to report

Open an **[Install report](https://github.com/tornis-tecnologia/veloxsearch-oss/issues/new?template=install_report.yml)**
issue and paste your notes into it. One report per run.

- You do not need to file separate issues for each problem. Maintainers turn
  every blocker and confusion point in your report into its own issue, linked to
  #60.
- A report of a run that stopped at scenario 1 is as useful as one that finished.
- If you found something that looks like a security problem, **leave it out of
  the report** and follow [the security section](#security-review) instead.

---

## Security review

This part is for security-minded reviewers who want to try to break what
VeloxSearch publishes. It is separate from the install run: you may do both, but
report them separately.

### Scope

The published surfaces:

- **UI authentication** — the first-run admin setup, login and the session
  cookie, including reaching the UI through the catch-all Ingress created on
  clusters with a default IngressClass. The account model is described in
  [`auth/accounts.md`](auth/accounts.md).
- **The Dashboards Ingress** — each deployment's Dashboards published at
  `https://<deployment>.<base-domain>` in ingress mode (the domain and TLS
  section of [`INSTALL.md`](INSTALL.md)).
- **OTLP ingest** — the public OTLP routes a deployment publishes once the
  Observability Stack is installed from its Integrations tab, together with
  their credential and the deployment's IP allow-list.

Read the threat model and the out-of-scope list in
[`SECURITY.md`](../SECURITY.md) first: they tell you which properties the design
promises, so you can tell a bug from a deliberate boundary.

### Rules

- **Test only infrastructure you own**, or have written permission to test.
  Install VeloxSearch on your own cluster and attack that. Never test against
  anyone else's installation, the integration registry, the image registry, the
  project's download host or the source hosting.
- **No denial-of-service** or volumetric testing against anything you do not own.
- **Report privately, through [`SECURITY.md`](../SECURITY.md)** — a private
  security advisory. Never in an install report, a public issue, a discussion or
  a pull request.
- **Disclosure is coordinated** with you after a fix ships, on the timeline in
  `SECURITY.md`.

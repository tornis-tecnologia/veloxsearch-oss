# VeloxSearch — Design Decisions

Durable record of architectural decisions, the options weighed, and the rationale.
Dates are when the decision was made.

---

## ADR-001 — Bootstrap / installation method
**Decision:** Single `kubectl apply -f https://get.veloxsearch.ai/install.yaml` (generated from a Helm template via `helm template`). Self-bootstrapping per ADR-014.
**Date:** 2026-06-08 — **DEFERRED ("in the drawer")** as of 2026-06-09
**Options:** (A) Helm chart, (B) single install.yaml, (C) installer binary.
**Rationale:** Lowest friction for v1; no Helm dependency for the user. Generate it from a Helm template so we maintain one source. Installer binary (C) is a later polish item.
**Status / blockers:** Not built yet, parked until after the wizard widening. Real "install into a *foreign* cluster" is blocked on: (1) publishing the image to a **public registry** (currently side-loaded via `ctr import` — open decision: GHCR vs Docker Hub vs in-cluster registry); (2) ingress is environment-specific — foreign clusters have no HAProxy, so the app must expose itself via the cluster's own ingress/LoadBalancer, not assume ours. Design captured in `docs/INSTALLER.md`. `get.veloxsearch.ai` resolves (wildcard DNS) but is intentionally not served yet.

## ADR-002 — RBAC scope of the wizard ServiceAccount
**Decision:** `cluster-admin` for now (test cluster); tighten before product. → **Implemented as option C on 2026-06-10.**
**Date:** 2026-06-08, revised 2026-06-10
**Options:** (A) cluster-admin, (B) enumerated ClusterRole, (C) two-phase (privileged installer + narrow runtime SA).
**As built (option C, `deploy/k8s/veloxsearch.yaml`):**
- **`veloxsearch-runtime`** (ClusterRole + 2 namespaced Roles): opensearch.org CRs full lifecycle; cluster-wide *read* of deployments/statefulsets/daemonsets/pods/namespaces (discovery + readiness + Fluent-Bit-grant parity); namespace create; CRD read (conformity probe); clusterroles/bindings create *without* `escalate` (can only grant what it holds — exactly the velox-agent reads); secrets+ingresses CRUD in `veloxsearch-test`; configmaps/serviceaccounts/daemonsets CRUD in `velox-agents`.
- **`veloxsearch-bootstrap`** (ClusterRoleBinding → cluster-admin): phase-1 powers for the ADR-014 self-install only — cert-manager/operator bundles create CRDs, webhooks and broad ClusterRoles that would need `escalate` + bind anyway; enumerating them buys nothing. **Revoke after first-run:** `kubectl delete clusterrolebinding veloxsearch-bootstrap`. Fresh installs ship it; conformity-green clusters don't need it.
**Rationale for the split:** the runtime set is stable and auditable; the bootstrap set is unbounded by upstream bundles. Separating them makes "delete one binding" the entire hardening story instead of an RBAC archaeology project.

## ADR-003 — Wizard state storage
**Decision:** Kubernetes as the datastore (Secrets/ConfigMaps/CRs) — NO external DB. Audit/history is the exception → goes to OTEL/OpenSearch.
**Date:** 2026-06-08
**Options:** (A) SQLite on Longhorn PVC, (B) state in OpenSearch (dogfood), (C) K8s as DB.
**Rationale:** Fewest moving parts; survives pod restarts; `kubectl`-inspectable. Synergizes with ADR-007 (Secrets) and ADR-008 (OTEL).
- Users/auth → K8s `Secret` (bcrypt-hashed passwords)
- Sessions → stateless signed JWT/cookie, signing key in a Secret (do NOT store sessions in etcd)
- Deployment state → the `OpenSearchCluster` CR is the source of truth; recipe status as annotations/labels or a per-deployment ConfigMap
- **Audit log / history → NOT in ConfigMaps** (1MB cap, not append-friendly). Emit as structured events to OTEL → OpenSearch.

**Amended by ADR-041 (2026-07-20):** this decision stands for the **single-admin era**. Multi-user control-plane state (users/tenants/quotas/audit) moves to a small in-cluster Postgres; deployment state stays in the `OpenSearchCluster` CRs.

## ADR-004 — Collection agent
**Decision:** OTEL Collector for everything (logs + metrics + traces). No custom agent.
**Date:** 2026-06-08
**Options:** (A) custom packaged agent, (B) Fluent Bit + OTEL, (C) OTEL for everything.
**Rationale:** One agent, one config language, fewer dependencies. The "how-to" is a templating task: render an OTEL config with this deployment's endpoint + credentials + index, plus copy-paste install commands. Fallback in back pocket: Fluent Bit for logs only if OTEL log parsing for Nginx gets fiddly.

## ADR-005 — "Data has arrived" detection
**Decision:** Backend polls OpenSearch `docs.count` server-side; pushes status to the browser via SSE/WebSocket. Explicit state machine with a first-class timeout/troubleshoot screen.
**Date:** 2026-06-08
**Options:** (A) poll, (B) gRPC.
**Rationale:** OpenSearch has no change-stream/CDC — detection is fundamentally poll. OpenSearch 3.0's new gRPC (which the operator now exposes) is a bulk/search transport, NOT a subscribe channel, so it doesn't help detection. We remove the polling *smell* by making the browser hold an SSE/WebSocket stream while the backend polls. State machine: `WAITING → FIRST_DOC_SEEN (✨) → HEALTHY → (TIMEOUT → troubleshoot)`. Advanced v2 option: tee a push signal from the OTEL Collector on first export for true end-to-end push.
**As built (2026-06-10):** `GET /api/events` (axum SSE, behind auth) streams the full deployment-status snapshot every 3s; comment frames keep the pipe warm through transient K8s errors. Client (`use_deployments`): one `EventSource` feeds a signal driving the Status list **and** the deployment detail page; if the stream dies, EventSource auto-reconnects *and* a server-fn poll ticks only-while-dead as fallback. **Scope note:** per-recipe doc counts (`monitoring_status`) still use the 4s server-fn poll — they hit OpenSearch per deployment×recipe and only render on the Monitoring tab; folding them into the global stream would multiply OpenSearch queries for every connected client. Timeout/troubleshoot screen still TODO.

## ADR-006 — Build approach: vertical slice
**Decision:** Build one thin end-to-end slice first (deploy operator → create one hardcoded `OpenSearchCluster` → one screen shows "creating… ready, URL"), then widen.
**Date:** 2026-06-08
**Rationale:** Surfaces the riskiest integration (kube-rs ↔ operator ↔ OpenSearch) on day one; gives Rodrigo something demoable at the next daily. Parallelize the *widening* (recipes, endpoints, UI steps) with subagents once contracts exist — NOT the spike itself (too tightly coupled).

## ADR-007 — Secrets
**Decision:** K8s Secrets as the baseline for all wizard-generated credentials. Never bake creds into images or logs.
**Date:** 2026-06-08
**Options:** (A) plaintext status quo, (B) K8s Secrets, (C) Sealed Secrets/SOPS.
**Rationale:** B is sufficient for runtime. Upgrade to C (encrypted-at-rest, Git-safe) only if/when we adopt GitOps — you cannot commit plaintext Secrets to Git. Linked to ADR-003 and the deploy-path decision.

## ADR-008 — Self-observability
**Decision:** Instrument the Axum backend with `tracing` + `tracing-opentelemetry`, export OTLP to the existing `:4318` endpoint (or the managed OpenSearch). Doubles as the audit-log sink (ADR-003).
**Date:** 2026-06-08
**Rationale:** Near-free, on-brand (observability product observes itself), and gives us the audit trail without abusing ConfigMaps.
**Status note (2026-06-10):** OTLP exporter deferred again — the opentelemetry/tracing-opentelemetry/otlp stack is ~80 extra crates for a single-pod admin app; not worth it before the basics are signed off. We already have structured `tracing` logs, and once the **kubernetes recipe** is enabled on a managed cluster, the app's own pod logs flow into OpenSearch via our own agent — self-observability through dogfood, zero extra deps. Full OTLP→`:4318` when audit-grade trails are actually needed.

## ADR-009 — OpenSearch / operator version
**Decision:** Target OpenSearch 3.x via opensearch-k8s-operator.
**Date:** 2026-06-08
**Rationale:** Verified — operator 3.0.0 supports OpenSearch 2.19.2 through latest 3.x. Operator now exposes OpenSearch 3.0's gRPC port and resolved 3.0.0 upgrade deadlocks.

## ADR-010 — Tech stack
**Decision:** Rust everywhere — Axum backend (kube-rs for K8s, reqwest for OpenSearch), Leptos frontend (SSR + WASM).
**Date:** 2026-06-08
**Rationale:** Owner preference; single-language stack; strong async story.

## ADR-011 — TLS certificate issuance
**Decision:** Issue the `veloxsearch.ai` (wildcard `*.veloxsearch.ai`) cert via **certbot DNS-01 with the Cloudflare plugin**, using the `CLOUDFLARE_API_TOKEN`. PEM goes in `/etc/haproxy/certs/`.
**Date:** 2026-06-09
**Options:** (A) HTTP-01 via HAProxy port 80, (B) DNS-01 via Cloudflare, (C) Cloudflare Origin cert + keep proxy on.
**Rationale:** DNS-01 works even while the record is Cloudflare-proxied, supports wildcards, and needs no inbound port-80 challenge routing through HAProxy. We already hold the Cloudflare token. `certbot` is installed on the Proxmox host.

## ADR-012 — DNS exposure mode
**Decision:** Point the `veloxsearch.ai` A record at the deployment host's public IP as **grey-cloud (DNS-only)**, so the host's HAProxy terminates TLS with the Let's Encrypt cert rather than a CDN edge.
**Date:** 2026-06-09
**Rationale:** The existing HAProxy gateway pattern terminates LE certs per-domain; orange-cloud (proxied) would require Cloudflare origin certs and change the trust model. Keep it consistent. (Cert issuance via DNS-01 still uses the Cloudflare API regardless of cloud color.)

## ADR-013 — Auth required before public exposure
**Decision:** The app MUST have a login/password wall before it is exposed via HAProxy.
**Date:** 2026-06-09 — **IMPLEMENTED**
**Rationale:** The wizard provisions Kubernetes/OpenSearch clusters. An unauthenticated public endpoint = anyone can create/destroy clusters. Hard prerequisite for Phase 2 exposure.
**As built (`src/auth.rs`):** single admin credential from env (`VELOX_ADMIN_USER`, `VELOX_ADMIN_PASSWORD`; a K8s Secret in-cluster); stateless session = HMAC-SHA256-signed cookie (`user:exp:sig`, key `VELOX_SESSION_SECRET`, 24h TTL); Axum `from_fn` middleware gates every route except `/login`, `/api/login`, and static assets — browser navs → 303 `/login`, API calls → 401. `VELOX_COOKIE_SECURE=1` adds the `Secure` flag behind HTTPS. Verified: unauth block, 401 on protected APIs, wrong-creds reject, forged-cookie reject.
**Deferred hardening (still ADR-003 target):** bcrypt password hashing, multi-user, first-run credential setup.

## ADR-014 — Zero prerequisites / self-bootstrapping
**Decision:** The only prerequisite to run VeloxSearch is **a running Kubernetes/K3S cluster** (and kubectl access to install it). Nothing else. The app/installer detects and installs everything it needs:
- **cert-manager** — if the `cert-manager.io` CRDs are absent, install it.
- **OpenSearch operator** — if the `opensearch.org` CRDs are absent, install it.
- (anything else a recipe needs is installed on demand.)
**Date:** 2026-06-09
**Rationale:** Matches the product vision (Elastic-Agent-style onboarding) — the user should never pre-install operators, cert-manager, or CRDs. The wizard owns the full setup, idempotently. "If it has it, use it; if not, install it."
**Implications:**
- Detection = check for the relevant CRDs / namespaces via the K8s API (kube-rs).
- Install = apply the upstream all-in-one manifests (cert-manager `cert-manager.yaml`, operator bundle) idempotently via server-side apply, or run helm. Must be safe to re-run.
- This runs either at install time (install.yaml job) or lazily at first deployment creation in the app. Existing installs (like our dev cluster, which already has both) are no-ops.
**Status:** Principle adopted. Runtime self-install not yet implemented (dev cluster already has cert-manager + operator). Tracked under the installer epic.

## ADR-015 — Scope focus: basics first, advanced config deferred
**Decision:** Keep the v1 focus on the **basic panel functionalities** — cluster status monitoring, cluster creation from predefined templates/recipes, and enable/disable of monitoring targets (e.g. Nginx). **Advanced cluster configuration is deferred** to a later phase.
**Date:** 2026-06-10 (validated at the 2026-06-09 daily, `docs/meetings/2.md`)
**Rationale:** Ricardo demoed the live prototype (login-protected, cluster status, create-from-template, Nginx monitoring enable/disable). Rodrigo validated the progress and explicitly agreed to hold the initial scope on the basics while development continues, rather than expanding into advanced configuration now. Confirms the pre-baked-recipe direction (ADR-006 widening) and keeps the wizard's surface area small for the first product cut.
**Implications:**
- The "Advanced" config surface (raw `opensearch.yml` / `additionalConfig`, custom nodes/heap/disk) stays available but is **not** the priority — no further investment until the basics are signed off.
- Continue hardening the core happy path: create → monitor (Nginx + K3S) → dashboard.

## ADR-016 — Sizing model: always 3 nodes, presets vary disk
**Decision:** Every deployment is **always 3 nodes**. Sizing presets (small/medium/large) change **disk** (and heap), never the node count.
**Date:** 2026-06-10 (2026-06-09 daily, `docs/meetings/2.md`)
**Rationale:** Rodrigo: "sempre vai ser três nós por uma questão de segurança mesmo" — 3 nodes is the safe/quorum baseline; bigger profiles just add disk ("vou mudar apenas a quantidade de HD"). Removes the old "large = 5 nodes". Implemented in `k8s::sizing` (large now `replicas: 3`). Verified live (`size=large` → `replicas=3`).
**Future:** Rodrigo has a sizing **calculator** (exists in code/XML) — convert it to an API and feed the presets / a "custom size" profile later. Start basic for now.

## ADR-017 — Cluster "purpose" = Observability / Security / Search
**Decision:** Replace the free-text purpose with **three fixed profiles**: **Observability**, **Security**, **Search** — chosen up front like Elastic Cloud's "what do you want to do first?".
**Date:** 2026-06-10 (2026-06-09 daily)
**Rationale:** Mirrors Elastic Cloud onboarding (the model Rodrigo wants us to follow structurally). For now `purpose` is just a stored label; later each profile pre-bakes its own recipes/dashboards/ISM. Implemented as a `<select>` in Create + Configuration forms.

## ADR-018 — Monitor-at-creation is dynamic (discovery-driven)
**Decision:** The "Monitor at creation" options are **generated from a live cluster scan**, not hardcoded. On the Create form, VeloxSearch calls `discover()` and lists the monitorable workloads it actually finds (e.g. nginx in `demo/web`), plus the always-available "Kubernetes / K3S" cluster-self option.
**Date:** 2026-06-10 (2026-06-09 daily)
**Rationale:** Rodrigo: "quando eu crio, ele olha pro cluster e fala: quais são as coisas que estão aqui que eu posso monitorar." The static nginx checkbox was the placeholder. Selected recipe ids are submitted as a CSV hidden field and persisted to the `monitors` annotation. Catalog still pre-baked (ADR-006) — discovery only decides *what to offer*.
**As built, revised 2026-06-10 — deferred apply:** selecting monitors at creation originally only *recorded* them (annotation); no agent ever shipped, which is the likely root of Rodrigo's "data not visibly ingesting" report. Applying at create-time is impossible (OpenSearch isn't answering yet), so `create_cluster` now spawns a background task: wait for green (≤10 min), then apply each selected recipe (pipeline, index-pattern, dashboard, Fluent Bit agent) exactly as the Monitoring tab's Enable does. Failures land in the pod log; the Monitoring tab remains the manual retry path.

## ADR-019 — UX principles: minimalist SaaS, i18n, dark/light
**Decision:** UI must be **minimalist & simple ("à la Google")**, following SaaS navigation/usability best practices but stripped down — clear, objective. Plus: **internationalization (multilingual)** and a **dark/light mode** toggle.
**Date:** 2026-06-10 (2026-06-09 daily)
**Rationale:** Direct asks from Rodrigo. Structurally model the flow on Elastic Cloud (deployment list → create → manage), but visually keep it spartan. i18n and theme are first-class but lower priority than the onboarding happy-path. Maintainability matters: keep it easy for another dev to pick up. **Not yet implemented.**

## ADR-020 — Deployment naming must be unique (collision-safe)
**Decision:** A deployment's name → its subdomain (`<name>.veloxsearch.ai`) and CR name, so it must be **unique within the cluster**. We **always auto-append a short random suffix** (Elastic-style `name-xxxx`) on create.
**Date:** 2026-06-10 (2026-06-09 daily) — **IMPLEMENTED**
**Rationale:** Rodrigo: creating a second `velox-test` would fail; always suffixing (like competitors) sidesteps it entirely. The Ingress already does host routing; the wildcard `*.veloxsearch.ai` DNS + cert cover any generated subdomain (no DNS API call needed).
**As built:**
- `k8s::unique_name(base)` sanitizes the base to a DNS-1123 label (`sanitize_label`) and appends `-<4 base36 chars>` (`rand_suffix`: time ⊕ atomic counter), retrying against the live CR list until free (`get_opt`).
- **Two server fns** split the paths: `create_cluster` (NEW) generates the unique name and **returns it** (UI shows/links it); `save_cluster` (EDIT) upserts the name **verbatim** so editing never spawns a duplicate. Both share `parse_overrides`.
- Verified live: `blablabl`→`blablabl-pyxn`, `blablabl` again→`blablabl-snwg`, `My Logs!!`→`my-logs-pbef`; each got a matching ingress host; edit changed purpose in place (CR count unchanged). Cleaned up after.

## ADR-021 — Storage: use what the cluster provides
**Decision:** Use whatever storage the target cluster already exposes (today: emptyDir for test; a StorageClass where present). **Do not** build a storage-manager installer for now.
**Date:** 2026-06-10 (2026-06-09 daily)
**Rationale:** Rodrigo: "vamos começar com o básico, com o que tá disponível no disco." We're bound by what Kubernetes surfaces — can't provision storage the cluster doesn't offer. Revisit (offer to install a storage provider) only after the basics land.

## ADR-022 — Self-bootstrap implementation: vendored manifests + SSA, background job
**Decision:** Implement ADR-014's runtime self-install (`src/bootstrap.rs`) by **vendoring the upstream install manifests** into the repo (`deploy/bootstrap/cert-manager.yaml` v1.20.2 + `operator.yaml` = `helm template opensearch-operator --version 3.0.2 --include-crds`), embedding them in the binary (`include_str!`), and applying them with **server-side apply** through kube-rs. Installation runs as a **detached tokio task**; server fns return instantly and the UI polls `bootstrap_status` — a checklist screen gates the main tabs until the cluster "conforms".
**Date:** 2026-06-10
**Options:** (A) shell out to helm at runtime, (B) embed/render charts with a Rust helm lib, (C) vendor rendered manifests + SSA, (D) report-only with copy-paste instructions.
**Rationale:**
- (A) means shipping helm in the image and trusting outbound chart repos at runtime; (B) no mature crate; (D) violates ADR-014's "the wizard owns the setup".
- (C) is `kubectl apply -f` semantics with zero new runtime deps, pinned versions (= what we validated on the dev cluster), reproducible offline installs, and idempotent re-runs via SSA (`veloxsearch-bootstrap` field manager).
- **Install order matters:** the operator bundle contains cert-manager `Certificate`/`Issuer` CRs, so cert-manager is installed and waited ready first. Bundles are applied in rounds (Namespace → CRD → rest; failures retried with fresh API discovery) to absorb CRD-registration and webhook-startup races.
- **Background job, not a long request:** installs take minutes (image pulls); HAProxy/browser timeouts would kill a synchronous server fn. A `static Mutex<Job>` tracks Idle/Running(step)/Failed for the status endpoint.
**Residual risk:** the install path only runs on virgin clusters (dev cluster is a verified no-op). Validate on a disposable cluster before marketing "zero-prereq". Version bumps = re-vendor the two files.
**Verified:** live `bootstrap_status` on the dev cluster returns all-ready/conformity; UI gates render accordingly.

## ADR-023 — First-run admin setup: managed K8s Secret + bcrypt, env as break-glass
**Decision:** On first boot with no credentials configured, the app enters **first-run mode**: the auth middleware funnels every request to `/setup` (all APIs 401) until an admin account is created. Setup bcrypt-hashes the password (cost 12) and persists `username` / `password_hash` / a freshly generated `session_secret` in Secret **`veloxsearch-credentials`** (SSA, ADR-003/007). Resolution order: **managed Secret > env (`VELOX_ADMIN_*`) > first-run**. Setup auto-logs the new admin in; the route seals itself afterwards.
**Date:** 2026-06-10
**Options:** (A) keep env-only creds (status quo), (B) managed Secret w/ plaintext, (C) managed Secret w/ bcrypt + persisted session secret.
**Rationale:** Rodrigo's onboarding journey starts with "set user and password on first access" (Phase 4). (C) also closes ADR-013's deferred hardening (bcrypt) and fixes session invalidation on pod restart for free: the signing secret now lives in the Secret instead of a per-pod env/default. Env creds stay as **break-glass** (lost password → `kubectl delete secret veloxsearch-credentials` + set env, or just delete the secret to re-enter first-run after a restart).
**Notes:** auth state is cached in-process after the first successful probe (per-request K8s reads would be silly); probe *failures* are not cached. Deleting the Secret therefore needs a pod restart to re-trigger first-run — documented behavior, not a bug. Multi-user stays future work (ADR-003).
**Verified locally against the live cluster:** first-run seal (/ → /setup, APIs 401), confirm-mismatch + short-password rejections, setup 200 → Secret created with `$2b$12$` hash, /setup→/ redirect once configured, wrong-pass reject, new-creds login OK. Test secret removed afterwards.

## ADR-024 — i18n + theming: static dictionary + CSS vars, no frameworks
**Decision:** Implement ADR-019's i18n and dark/light asks with **zero new frameworks**:
- **i18n** (`src/i18n.rs`): a `Lang` enum (En/Pt) in a reactive context signal + one `t(lang, key)` match over ~90 static keys. Unknown key → renders the key itself (greppable, never panics). Persisted in `localStorage[velox-lang]`, defaulting from `navigator.language` (pt* → PT). Every UI string is a `move || t(lang.get(), …)` closure, so flipping language re-renders in place.
- **Theme**: dark stays the `:root` default; light is a `[data-theme="light"]` CSS-var override (all borders/inputs moved to vars). A 1-line inline script in `<head>` applies the stored theme **before first paint** (no flash); the toggle writes `localStorage[velox-theme]` + the attribute. Floating `PrefsBar` (☀️/🌙 + EN/PT) on every screen.
**Date:** 2026-06-10
**Options considered:** fluent/leptos-i18n crates; cookie-based SSR locale; `prefers-color-scheme` only.
**Rationale:** Two languages × ~90 strings don't justify a translation framework (build complexity, message files, macro DSLs) — a match statement is greppable, type-checked, and trivially extended; revisit if languages multiply or pluralization rules appear. Cookie-based SSR locale negotiation was skipped deliberately: SSR emits EN and the client corrects right after hydration — acceptable blip for an admin panel, zero plumbing. `prefers-color-scheme` alone gives no user override; the explicit toggle (persisted, pre-paint script) does, and OS-preference can still be added as the *default* later.
**Cost note:** added `web-sys` `Window/Navigator/Storage` features (already in the tree via leptos — feature unification, no new crate compiled).

---

## ADR-025 — CD path: manual apply stays until a registry exists
**Decision:** Keep deploys manual — `deploy/build-image.sh` (build + `ctr import` to each node) + `kubectl apply -f deploy/k8s/` over the tunnel. **Argo CD is deferred until the image lives in a real registry.**
**Date:** 2026-06-10 (was an open question since 2026-06-08)
**Rationale:** GitOps reconciles manifests that reference *pullable* images; ours is side-loaded into containerd on each node (no registry, ADR-001 blocker). Argo would add a component while the actual bottleneck — publish the image — remains. Sequence: registry decision (GHCR vs Docker Hub vs in-cluster) → push images → then Argo + Sealed Secrets (ADR-007) in one move. Revisit at the installer epic.

---

## ADR-026 — Requirements-first support + a 2-cluster conformance fleet
**Decision:** Define the v1 supported-platform envelope in **`docs/REQUIREMENTS.md`** (R1–R8: k8s ≥1.30, cluster-admin at install, default StorageClass, resource floor, amd64, registry egress, no conflicting pre-existing stack, ingress optional) and make the conformity probe its executable form — unsupported clusters get a **clear refusal with remediation text**, never a hang. Validate against a **2-cluster disposable fleet** on the Tornis Proxmox, managed by OpenTofu **in this repo** (`conformance/`, one state root per cluster = one `tofu apply` each):
- **ct1-k3s-greenfield** (single disposable VM, 12G/4c/60G): k3s latest, single node, Traefik + local-path present, nginx demo workload — must pass the whole journey.
- **ct2-k0s-bare** (single disposable VM, 6G/2c/30G): k0s `--single`, ships with *nothing* (no SC, no ingress) — must fail conformity informatively (R3) and only offer port-forward (R8).
**Date:** 2026-06-11 (discussion with Ricardo)
**Options:** (A) 3rd "brownfield" cluster (pre-existing cert-manager, version skew, ingress-nginx) — **dropped by Ricardo**: define requirements for correct function *first*, broaden support after. (B) Fleet configs in the sidecar repo (CLAUDE.md says VMs belong there) — kept here instead: these are disposable VeloxSearch *test fixtures* whose lifecycle follows this app, not Tornis base infra; documented as a deliberate bend of that rule.
**Rationale:** A narrow, testable contract beats a wide, untested one; the two fixtures cover both sides of it (must-pass / must-reject). Single-node everywhere because prod already exercises multi-node daily — single-node is the uncovered, most client-realistic case (also caught a real product gap: the small preset's 6Gi of requests sets a ~12GiB single-node floor → "starter" preset candidate). k0s chosen over a stripped k3s because it genuinely ships bare — no artificial fixture. Proxmox capacity verified: 146 GiB RAM / 519 GB disk free; fleet costs ~18 GiB.
**Infra notes:** clones a Debian-12 cloud-init template via the `bpg/proxmox` provider, deriving the VM ID from the address. Provider values (endpoint, node, storage, bridge) come from the operator's own config and are deliberately not recorded here — they are environment, not decision. Distro install runs as a tofu `remote-exec` provisioner through the Proxmox host as SSH bastion (avoids Proxmox snippet-upload plumbing; one `apply` still does VM + cluster). State/tfvars are gitignored; tfvars generated from `creds`/config.

## ADR-027 — Generic install: one manifest, access modes, self-revoking bootstrap RBAC
**Decision:** Ship a client-facing **`deploy/install.yaml`** (the future `kubectl apply -f get.veloxsearch.ai`): namespace `veloxsearch-system`, SA + enumerated `veloxsearch-runtime` ClusterRole + binding, the `veloxsearch-bootstrap` cluster-admin binding, Deployment, Service — and **no Ingress**. First contact is `kubectl port-forward`; everything after (first-run admin, self-bootstrap) already works on a fresh cluster by design (ADR-022/023). Three changes make the app itself generic:
1. **`BASE_DOMAIN` stops being a constant** (`src/k8s.rs`): replaced by an **access config** (ConfigMap `veloxsearch-config`): `mode = portforward` (default) | `ingress { base_domain, ingress_class }`. In portforward mode the UI shows the copy-paste `kubectl port-forward` command per deployment instead of an `https://<name>.<domain>` link; ingress mode is offered when R8 detects an IngressClass and the user supplies a domain. Tornis prod just sets `ingress { veloxsearch.ai, traefik }` — behavior unchanged.
2. **The app deletes its own `veloxsearch-bootstrap` binding** once the conformity probe reports the cluster fully bootstrapped — using the bootstrap powers themselves (cluster-admin can delete its own binding; the runtime role needs nothing extra). Idempotent: binding absent = step skipped. Re-bootstrap after revocation (e.g. operator upgrade) requires re-applying the binding — documented.
3. **Image stays side-loaded** (`ctr import`, works identically on k3s and k0s) with `imagePullPolicy: IfNotPresent` — the public-registry decision is drawered (Rodrigo: project may not go public). install.yaml is therefore testable today; publishing it at `get.veloxsearch.ai` only makes sense once the image is pullable (ADR-025).
**Date:** 2026-06-11
**Options:** (A) keep per-env manifests only (status quo — Tornis-shaped, unshippable); (B) Helm chart (client needs helm; one static file is the Elastic-style onboarding bar we set); (C) require an Ingress + domain up front (kills the "only has a cluster" persona — port-forward must be the zero-assumption default).
**Rationale:** The persona is "has a new K8s cluster, nothing else". Port-forward-first removes every infra assumption; ingress/domain becomes a *capability the wizard unlocks* when present (R8). Self-revocation closes ADR-002's "delete me afterwards" honesty gap — install one-liners and manual cleanup steps don't mix.
**Verified 2026-06-11 (ADR-026 fleet, first true virgin-cluster bootstrap — closes ADR-022's residual risk):** ct1 ran install.yaml → first-run → R1–R8 all ✓ → cert-manager+operator auto-installed → bootstrap binding self-revoked → UI-created deployment green in 110 s, 3 OpenSearch pods co-scheduled on the single node (no anti-affinity blocker), agents auto-deployed, data receiving; no Ingress objects in portforward mode, Overview hands out the kubectl command + creds. ct2 was refused (R3 no StorageClass ✗, R4 5.7Gi<8Gi ✗) with remediation text and **zero** objects installed. Prod (ingress mode CM) browser-verified unchanged on 0.3.0.
**Caveats:** (1) re-applying a manifest that ships the bootstrap binding re-creates it; conformant clusters skip the setup path, so the auto-revoke doesn't fire — delete manually after manifest re-applies (or hit /setup's ensure once). (2) `kubectl get opensearchclusters` right after the first install can say "No resources found" due to kubectl's stale discovery cache — use the full `opensearchclusters.opensearch.org` name.

---

## ADR-028 — Purpose profiles do real things: ISM retention, SIEM detectors, search tuning
**Decision:** The purpose chosen at create (ADR-017) now configures the cluster differently (`src/profiles.rs`, applied **after green** and **before** the monitoring recipes in the deferred task, re-applied on every save — all idempotent upserts):
- **observability** → ISM policy `velox-retention`: delete `nginx-logs*`/`k8s-logs*` indices after **30 d** (`ism_template` auto-attaches at index creation — immediate, verified; pre-existing indices attached explicitly via `_ism/add` + `change_policy`).
- **security** → same policy at **90 d** + two Security Analytics detectors loaded with *every* pre-packaged Sigma rule of their log type: `velox-nginx-threats` (`others_web`, 49 rules) and `velox-k8s-threats` (`linux`, 188 rules), schedule 5 min; detector creation auto-creates the alerting workflow/monitor.
- **search** → **no agents** (enforced server-side at create *and* save — save removes running agents, data stays), retention policy actively deleted (keep-forever is the contract), search-tuned `_index_template` `velox-search-defaults` on `app-*` (refresh 1 s, 1 replica).
The UI's purpose `<select>`s became a 3-card comparable radio group (RPG-class-style presentation, sober language — Rodrigo's "macro UX" brief): identical rows *Keeps data / Collects / Sets up / Best for*; picking Search hides the monitor checkboxes (presentation only — the server enforces). Wire format unchanged (`name="purpose"`, same values).
**Date:** 2026-06-11
**Options:** (A) profiles as pure labels (status quo — dishonest marketing); (B) AI-generated per-purpose config (violates the pre-baked-recipes decision); (C) this — fixed, verified recipes per purpose.
**Rationale & evidence:** Every REST shape was verified against a live OpenSearch 3.0.0 before being coded (`docs/research/2026-06-11-profile-apis-opensearch300.md`, produced by a research agent against ct1): the ISM upsert needs the 409 → GET → `?if_seq_no&if_primary_term` dance; SA has **no kubernetes/nginx log types** (23 types in 3.0.0) so `others_web`+`linux` are the honest nearest fit; SA's `rules`/`detectors` searches need `nested` query wrappers (flat queries return 0 silently — and even a nested `prefix` missed; trust nested `term`/`match_all` only); there is no "all pre-packaged rules" shortcut — ids are enumerated then passed.
**Verified 2026-06-11 on ct1 (0.4.0, save-path purpose cycling on `demo-mplk`):** security → policy 90 d + both detectors (49/188 rules) + both pre-existing indices managed; observability → policy `_version 2` @ 30 d (upsert path); search → policy 404, template present, both agent DaemonSets deleted, data intact (5 795 docs); restore → agents + 30 d policy back. Prod rolled to 0.4.0, browser_check + cards interaction PASS (release build, 0 console errors).
**Caveats (honest limits, roadmap):**
1. **Detection efficacy ≠ detector installation.** Pre-packaged Sigma rules query parsed fields (`cs-uri-query`, `auditd.log.*`); Fluent Bit ships raw lines in `message`/`log`, so most rules compile but can't match until ingestion parses logs into those fields (the nginx grok pipeline is a start; SA alias mappings are the lever — only `timestamp→@timestamp` auto-maps today). Security profile v1 = *real detectors, growing coverage*, not a full SIEM.
2. **Profile artifacts are additive.** Switching purpose never garbage-collects the previous profile's detectors/search template (only retention is actively removed by search). Leftovers are harmless and arguably useful; revisit if users ask.
3. Existing deployments get their profile on next save — the 0.4.0 upgrade itself changes nothing until then.
4. R3 (default StorageClass) vs reality: node pools still use `persistence: emptyDir` (Longhorn bootstrap-race workaround) — nothing claims a PVC today, so R3 is currently aspirational. Decide: move to PVCs (then R3 is real) or relax R3 to a warning. **→ Resolved by ADR-031: move to PVCs *and* bootstrap Longhorn when the default StorageClass is node-local/absent.**

---

## ADR-029 — Recipe catalog grows: PostgreSQL + Kubernetes Events (and what that forced)
**Decision:** Two new pre-baked recipes (v0.5.0): **`postgres`** (grok pipeline for the official images' stderr format → `postgres-logs`, levels/pid/user@db parsed, PostgreSQL Overview dashboard) and **`k8s-events`** (Fluent Bit `kubernetes_events` input watching the API server → `k8s-events`, typed template + Kubernetes Events dashboard; offered always-on next to "Kubernetes / K3S" since every cluster has events). Discovery now maps `postgres` images to an enableable recipe.
Four structural consequences, all deliberate:
1. **The events watcher is a single-replica Deployment, not a DaemonSet** — it reads the API server, so per-node copies would index every event once per node. `deploy_agent` now branches on recipe shape; tailers keep the DaemonSet + `/var/log` hostPath, the watcher gets neither.
2. **RBAC widened in both manifests**: runtime ClusterRole += `events` get/list/watch (groups `""` + `events.k8s.io`) — required by the no-escalate parity rule (the app can only grant the agent what it holds); velox-agents Role += `deployments`. Re-applying the manifests re-created `veloxsearch-bootstrap` on both clusters (known ADR-027 caveat) — re-revoked immediately.
3. **Retention patterns are now derived from the catalog** (`profiles.rs::log_patterns` iterates `RECIPES`) — a new recipe can never silently escape the purpose profile's ISM window again.
4. **Agent pod templates carry a config-hash annotation** — subPath mounts never update live, so a changed ConfigMap previously left agents running the old config with no rollout. Found the hard way (see below).
**Date:** 2026-06-11
**Found during verification (ct1):** Fluent Bit 3.1.9's **opensearch output has no `Include_Time_Key`** (that's an `es`-output option; config crashloops the agent). Events therefore get `@timestamp` from a `k8s-events` ingest pipeline (lastTimestamp → eventTime → metadata.creationTimestamp), consistent with how nginx/postgres pipelines do it.
**Verified 2026-06-11 on ct1 (demo-mplk + `demo/pg` postgres:16-alpine fixture):** discovery offers PostgreSQL with recipe; enable → DaemonSet (postgres) + 1-replica Deployment (events) in velox-agents; `postgres-logs` 116 docs with parsed `level`/`pid`/`pg_message` + pipeline `@timestamp`; `k8s-events` 52 docs with `type`/`reason`/`metadata.namespace`/`involvedObject.kind`; **both indices auto-attached to `velox-retention`** via the updated ism_template; all four dashboards present; browser_check PASS on ct1 + prod.
**Still open in the catalog:** redis, mysql/mariadb, Traefik (the k3s-native "nginx"), mongo/rabbitmq/kafka; security sources (ssh/auth, k8s audit) for the security profile.

---

## Open questions (decide with Rodrigo)
- Rotate the plaintext credentials currently in `creds`/chat once the product handles its own.
- Registry choice for public images (GHCR / Docker Hub / in-cluster) — gates ADR-001 installer + ADR-025 GitOps. **2026-06-11: drawered** (project may stay private); generic install proceeds with side-loaded images (ADR-027).
- "Starter" sizing preset for small single-node clusters (small's 6Gi requests ≈ 12GiB node floor) — found via ADR-026 fleet math; needs Rodrigo's sizing-calculator input (ADR-016).

---

## ADR-030 — Orchestration screen leveled up (meeting 3): per-node Overview, Edit/Integrations/Security split, JVM = ½ memory, per-deployment admin creds
**Decision:** Reworked the per-deployment panel per the 2026-06-12 daily (`docs/meetings/3.md`), treating a selected deployment as *existing with fixed finality* — you operate and resize it, you don't re-create it:
- **Detail tabs** `overview | edit | integrations | security` (was `overview | config | monitoring`). `Edit` is resize-only (size/nodes/memory/disk) — the purpose cards are gone (purpose is preserved verbatim in a hidden field). `Integrations` is the former monitoring/recipe-enable panel, renamed. `Security` is new.
- **Overview = cloud-design node blocks** (`NodeCards`): per-node CPU / JVM-heap / disk meters + doc count from OpenSearch `_nodes/stats` (`src/metrics.rs`), refreshed on the shared poll tick, last-good snapshot held so transient errors don't blank the grid. The old purpose/monitors text list ("proposta") was dropped for a compact health/size/disk summary line.
- **JVM heap is always half the node memory** (`k8s::heap_for`): never user-supplied. The free `heap` override became a `memory` override; `sizing()` presets are memory+disk tiers and heap is derived (small 3Gi→1536m, med/large 4Gi→2g). Cap 31g, floor 256m. Verified live on ct1: `size=small`→`-Xms1536m -Xmx1536m`, `memory=8Gi`→`-Xms4g -Xmx4g`.
- **Per-deployment admin creds** (`k8s::admin_creds`): single secret-first accessor (Secret `<name>-admin-credentials`, falls back to the bootstrap constants) now used by every `recipes`/`profiles`/`metrics` OpenSearch call — de-hardcodes `ADMIN_PASSWORD` so a reset propagates everywhere.
- **Metrics source = OpenSearch `_nodes/stats`** (not Kubernetes metrics-server): one source, no extra RBAC, also seeds indexing volumetry. Every field path was verified live on ct1 (`os.cpu.percent`, `jvm.mem.heap_used_percent`/`_in_bytes`/`heap_max_in_bytes`, `fs.total.{total,available}_in_bytes`, `indices.docs.count`).
**Date:** 2026-06-15
**Tests:** Rust unit tests (`heap_for` ½/cap/floor, unit parsing, replicas-always-3) + `tests/journey_check.py` (login→create→Overview/Edit/Integrations/Security→delete), run green against ct1 via the local binary (k8s API over the tunnel).
**Admin password reset — how it had to be done.** Live research on ct1 (background agent) proved the operator-seeded `admin` user is `is_reserved: true`, so its password CANNOT be changed via the security REST API — both `/_plugins/_security/api/account` and `/api/internalusers/admin` return `403 "Resource 'admin' is reserved"`. So `reset_admin_password` instead rewrites the `adminCredentialsSecret` (the hash source) and bumps a CR annotation (`veloxsearch.ai/security-reset`, under a dedicated SSA field manager so it can't prune the spec) to force an operator reconcile. **Verified live on ct1 (0.6.0, demo-mplk):** the operator re-runs its `securityconfig-update` job within ~10–20 s and reseeds — the new password starts returning 200 while the old returns 401; the revert direction behaves identically; and the app keeps authenticating throughout via `admin_creds` reading the updated Secret (`node_stats` stayed 200). Round-trip clean.

---

## ADR-031 — PVC-backed OpenSearch storage + per-deployment disk metric; bootstrap Longhorn when storage is node-local/absent
**Decision:** Resolve ADR-028 caveat 4 toward **real persistent storage**, and make the Overview's disk meter report the *deployment's* storage instead of the *node's*:
1. **Node pools claim a PVC, not `emptyDir`.** The CR's `persistence` (`src/k8s.rs`, today `{ emptyDir: {} }`) becomes `{ pvc: { storageClass: <default-or-selected>, accessModes: [ReadWriteOnce] } }` sized at the preset `diskSize`. Drops the emptyDir workaround — data survives pod reschedule and R3 stops being aspirational.
2. **The node card's disk meter = the PVC, not the node disk.** The current meter reads `_nodes/stats` `fs.total.{total,available}_in_bytes` (ADR-030); with `emptyDir` the data path lives on the node root fs, so it reports the **whole node HDD** — the reported bug. With a dedicated PV mounted at the data path that same field reflects the PVC; read the per-data-path mount (`fs.data[]`) to be explicit, and also surface PVC **phase** (Bound/Pending) + capacity from the K8s PVC objects so an unbound volume shows as Pending rather than a zeroed/node-sized meter.
3. **Storage-provider self-bootstrap (Longhorn).** Extends ADR-014/022's "if it has it, use it; if not, install it" to storage. A default StorageClass whose provisioner is **node-local** — `rancher.io/local-path` (k3s default), `kubernetes.io/no-provisioner`, hostpath / `openebs.io/local` variants — **or no default at all** is treated as *not real persistent storage*: the wizard installs **Longhorn** (vendored manifest + SSA + detached job, same machinery as cert-manager/operator) and waits for its StorageClass to be default-ready before provisioning any cluster. A real distributed/CSI default (prod's `longhorn` = `driver.longhorn.io`, Ceph/rook, cloud CSI) is left untouched.

**This amends ADR-021** ("use what the cluster provides; don't build a storage installer"). ADR-021 said to revisit "only after the basics land" — they have. We still *prefer* a real provisioner the cluster already has; we only install one when the alternative is node-local or nothing.
**Date:** 2026-06-15
**Status:** decided, **not yet built** (tracked as Phase 8 in `docs/TODO.md`).
**Options:** (A) keep emptyDir + show node disk (status quo — dishonest meter, data lost on reschedule); (B) PVCs only where a real SC already exists, else refuse (R3 hard-fail — bad UX on k3s/k0s); (C) this — PVCs always, bootstrap Longhorn when storage is node-local/absent.
**Cluster reality (verified 2026-06-15):** prod default SC = `longhorn` (`driver.longhorn.io`) → no bootstrap, just claim it; no PVCs in `veloxsearch-test` today (emptyDir). ct1 (k3s greenfield) default = `rancher.io/local-path` → node-local → now triggers the Longhorn bootstrap (previously passed R3 as-is). ct2 (k0s bare) = no SC → bootstrap.
**Consequences / caveats:**
1. **R3 becomes a remediation step, not a hard fail** (REQUIREMENTS.md): node-local/absent default → "VeloxSearch will install Longhorn"; a real CSI default is used as-is.
2. **Longhorn has its own node prerequisites** (`open-iscsi` / `nfs-common` on every node, a usable disk). Where a node can't run it, the bootstrap must **fail informatively** (the honesty rule) rather than leave PVCs Pending forever.
3. **The emptyDir reason was a "Longhorn bootstrap-PVC race"** (provisioning before the CSI was ready). The conformity gate already blocks cluster creation until bootstrap is green; storage joins that gate, so ordering closes the race.
4. **ct1 conformance expectation shifts** — the fleet test asserting ct1 passes R3 as-is must now assert it bootstraps Longhorn. Update `conformance/` + the REQUIREMENTS compat table.
5. **Existing emptyDir deployments are not auto-migrated** (changing persistence recreates the StatefulSet/data). New deployments get PVCs; document a recreate path for any old ones.

---

## ADR-032 — Frontend rewrite: React SPA over an Axum JSON API; Leptos/WASM removed
**Decision:** Replace the Leptos SSR+WASM frontend (ADR-010) with a **single-page React app** that talks to a plain **JSON HTTP API** exposed by the existing Axum backend. The browser no longer downloads a WASM bundle and the server no longer renders/hydrates HTML — Axum serves static assets + `/api/*` JSON; React owns all rendering client-side. **This amends ADR-010** ("Rust everywhere — Leptos frontend"); the backend stays Rust/Axum, only the UI layer changes. **Epic: react-rewrite (#23).** Merged into `develop` 2026-06-17.
**Date:** 2026-06-17
**Options:** (A) keep Leptos SSR+WASM; (B) React SPA + JSON API (chosen); (C) a server-rendered template stack (Askama/Maud) + sprinkle JS.
**Rationale:**
- The frontend churns far faster than the backend (meetings 2/3 reworked the wizard, the deployment panel, the purpose cards). Leptos coupled UI iteration to Rust recompiles + WASM rebuilds + hydration debugging (the recurring "curl can't see hydration panics" lesson — ADR-024). A JS SPA decouples UI iteration from the Rust build entirely.
- The transport split is clean: every former Leptos `#[server]` fn already had an `endpoint=` string, so each became one Axum handler at the **same path under `/api`** — the URL contract is unchanged, only the transport moved from server-fn POSTs to plain JSON (`src/api.rs`).
- React (vs hand-rolled JS, option C) gives component state for a stateful wizard without a framework war; the existing prototype views were already React-shaped.

**The `/api/*` contract (build clients + tests to this — `src/api.rs::routes`):**
- **Method rule:** no-argument reads are **GET** (`auth_state`, `bootstrap_status`, `discover`, `list_deployments`, `access_settings`); the deployment stream is **SSE** at `GET /api/events` (full `Vec<ClusterStatus>` snapshot every 3s, ADR-005); **everything else is POST** with a JSON body whose field names match the old server-fn params.
- **Auth:** all `/api/*` routes sit behind the session-cookie middleware **except `GET /api/auth_state`, `POST /api/login`, `POST /api/setup_admin`**, which must be reachable pre-auth. `auth_state` returns `{ first_run, authenticated, username }` — the SPA's single source of truth for which screen to mount. `login`/`logout`/`setup_admin` set the session cookie directly on the response.
- **Success:** the DTO as JSON (200), or an empty 200 for unit results (`Result<(), _>` → empty body; clients must tolerate an empty body). `create_cluster` returns the generated unique name as a bare JSON string.
- **Errors:** a uniform envelope `{ "error": "<message>" }` with a 4xx/5xx status — **400** validation, **401** bad credentials, **500** from the K8s/OpenSearch layer. Clients surface `error` in the UI; they must **not** log non-2xx to the console (the #32 browser gate fails on console errors). Best-effort reads that have no data yet (`node_stats` on a not-yet-green cluster) return an empty 200, never a 5xx, to keep the network panel clean.

**SPA shape (it is a single-URL app):**
- One HTML document; the visible screen is swapped by **React state**, not by routes. There are **no** server routes like `/login`, `/setup`, `/d/:name` — drive/test the UI by interacting with the rendered widgets, not by visiting paths.
- **Boot sequence** (`frontend/app.jsx`): probe `GET /api/auth_state` → `first_run` ⇒ **setup**, else `!authenticated` ⇒ **login**, else fetch `bootstrap_status` → `!ready` ⇒ **bootstrap/conformity gate** (ADR-014/026), else **main app**. Deployments arrive over the SSE stream; `localStorage` holds **only** UI prefs (theme, lang).
- Stable selectors the frontend exposes (the contract the Playwright checks bind to): `nav.tabs` (top tabs in the app; the 4 detail tabs in a deployment), `ul.req-list` (bootstrap requirements), `ul.setup-list` (install checklist), a `.prefs` bar on the pre-auth screens (theme button then lang button), and form fields `input[name="username|password|confirm"]`.

**Build/serving — Vite bundle (#31, done):** the JSX is bundled by **Vite** (`@vitejs/plugin-react`) into a hashed, minified, production-React bundle. `frontend/index.html` is the Vite entry — a single `<script type="module" src="/main.jsx">` (plus the pre-paint theme bootstrap); the `.jsx` files wire up via real ES `import`/`export` (the old `window.*` publish/consume seam and the `@babel/standalone` + React-UMD CDN tags are gone). `npm run build` emits `frontend/build/` (`index.html` + `assets/<name>-<hash>.{js,css}` + `favicon.ico`); `deploy/build-image.sh` runs `npm ci && npm run build` and stages that as the container's `/app/dist`. The server serves it unchanged (`ServeDir` + SPA fallback; assets live under `/assets/`, matched by `src/auth.rs::is_asset`). *(Superseded the original Babel-in-browser approach — JSX `type="text/babel"` compiled per-load by `@babel/standalone`, no build step — which was acceptable for an internal single-admin tool but shipped a dev-build React and a compiler on every load.)*
**Tests (this epic):** the Playwright checks were rewritten for the SPA + `/api/*` paths — `tests/firstrun_check.py` (first-run setup → conformity → bootstrap, #30), `tests/journey_check.py` (login → create wizard → manage tabs → delete, #30), and `tests/browser_check.py` (login/setup → bootstrap gate → home → create flow, now also capturing **network**: failed requests + non-2xx on `/api/*`/assets, exiting non-zero on any console-error / pageerror / failed-or-4xx/5xx request — #32). All Leptos route-based navigation and selectors were removed.
**Status:** backend (`src/api.rs`) + React SPA (`frontend/`) merged to `develop`; Leptos removed. Real build tooling landed (Vite, #31). **Follow-ups:** delete the transitional `app.rs` server-fn shims once nothing references `parse_overrides`/`bootstrap_dto` (#26).

---

## ADR-033 — App vs distribution surfaces; `get.veloxsearch.ai` is a CI-published artifact
**Decision:** Make explicit that this repo holds **two surfaces**, and stop hand-copying the public install site onto its box:
- **the app** — the control plane (`src/`, `frontend/`), shipped as a container image into a cluster;
- **the distribution** — `deploy/install.yaml`, the `velox` CLI (`velox init`), `conformance/`, and the `get.veloxsearch.ai` landing site.

The two surfaces stay in **one repo**: the install manifest and the landing copy must be version-locked to the app they install (a manifest that references an image tag, requirements the conformity screen re-checks, and brand copy that mirrors the app are all only correct *for a given app version*). Splitting them into a second repo would re-introduce exactly the drift we're removing.

The landing site is now **sourced from the repo and published by CI**, not maintained by hand on the LXC:
- A new top-level **`get-site/`** dir is the source: `index.html` (the only authored asset), `nginx.conf` (the live server block, mirrored), `assemble.sh`, `README.md`.
- **`get-site/assemble.sh`** is the single place that gathers the publishable bundle from canonical sources — `deploy/install.yaml` → `install.yml` (served `application/yaml`), `frontend/styles.css` → `styles.css`, `public/favicon.ico` → `favicon.ico`, and `get-site/index.html`. **No brand asset is duplicated under `get-site/`**: the stylesheet and favicon are pulled from `frontend/`/`public/` at publish time, so the landing identity stays coupled to the app — change the app's accent or favicon and the next publish carries it.
- A **`publish:get-site`** CI job (new `publish` stage in `.gitlab-ci.yml`) runs `assemble.sh` then `rsync --delete`s the bundle to the LXC web root, making the published site an exact mirror of the repo with no stale files.

**Date:** 2026-06-30
**Options:** (A) status quo — landing HTML + a copied `install.yaml` hand-edited on the box (drifts from the repo, the bug this fixes); (B) split the distribution into its own repo (loses the app↔manifest version lock); (C) this — one repo, explicit surfaces, get-site assembled from canonical sources and CI-published.
**Topology:** `get.veloxsearch.ai` is a standalone container on the deployment host's internal network, running nginx over a static document root. The host's **HAProxy** terminates TLS with the `*.veloxsearch.ai` wildcard and routes `get.veloxsearch.ai` to that container, health-checking `GET /healthz`. The live site already serves the brand-matched landing (`/styles.css` + `/favicon.ico`, app design tokens) and `/install.yml` as `application/yaml`; this ADR makes the repo its source of truth.
**Consequences / caveats:**
1. **The publish job needs an in-net runner.** The site container is on a private network that hosted CI runners cannot reach, so `publish:get-site` is tagged for a self-hosted runner and gated to `main` — the public site publishes on release, like `deploy:k3s`.
2. **Auth is an operator-provisioned deploy key.** A masked/protected CI var `GET_SITE_SSH_KEY` holds the private key; the public key is authorized on CT123. No key is embedded in the repo. Setup steps live in `get-site/README.md`.
3. **`nginx.conf` is mirrored, not applied by CI.** The publish job ships *content*, not server config; the nginx block is versioned here so the box config has a source of truth, but installing it stays a manual operator step (it changes rarely).
4. **`rsync --delete` makes the web root authoritative.** Anything not in the assembled bundle is removed on publish — the site cannot accrue stale hand-placed files.

## ADR-034 — Secrets centralization: vault-sourced operator secrets, ConfigMap'd config, history purge
**Decision:** Split every credential/config item into three explicit classes and give each one home (full inventory: `docs/SECRETS.md`):
1. **App-managed secrets** (`veloxsearch-credentials`, `<deployment>-admin-credentials`) stay runtime-generated K8s Secrets — ADR-023/030 unchanged; a vault never sees them.
2. **Operator-provisioned secrets** (`velox-pull`, `gitlab-runner-config`, break-glass `VELOX_ADMIN_*`) get their source of truth in a **central vault**, synced into the cluster by **External Secrets Operator** — manifests in `deploy/secrets/` (`ClusterSecretStore` + one `ExternalSecret` per item; references only, committable). The store is **provider-agnostic**; the documented backend is **AWS Secrets Manager** (the issue's "AWS Vault"), and swapping to e.g. HashiCorp Vault touches only the provider block. No hard dependency: the zero-credential install (ADR-027/033) works without ESO; the `.example.yaml` suffix keeps unconfigured stores out of `kubectl apply -f deploy/`.
3. **Non-secret pod env** (`RUST_LOG`, `VELOX_COOKIE_SECURE`) moves out of Deployment env literals into a new **`veloxsearch-env` ConfigMap** owned by the manifests (`envFrom`). It is deliberately separate from `veloxsearch-config`, which the app server-side-applies as its own field manager (`src/access.rs`) — sharing that object would put pod-startup env under app ownership.

The **git-history purge** of the two leaked credentials (the de-hardcoded OpenSearch admin literal + the `/setup` admin password once in `docs/TODO.md`) is prepared as an operator-gated runbook — `docs/runbooks/2026-07-02-git-history-secret-purge.md` (inventory verified with gitleaks over all refs, `git-filter-repo --replace-text` procedure, rotation-first checklist, GitLab repository-cleanup steps). Not executed here: it force-updates shared history and needs a client-coordinated freeze.
**Date:** 2026-07-02
**Options:** (A) hand-provisioned K8s Secrets forever (status quo; no central source, rotation is tribal knowledge); (B) Sealed Secrets/SOPS — encrypted secrets in git (ADR-007's upgrade path, but it puts ciphertext in the repo and still lacks a central vault); (C) secrets-store CSI driver (mount-time only, no plain Secret for `imagePullSecrets`/env, heavier per-node footprint); (D) this — ESO pulling from a managed vault, plain Secrets as the cluster-side interface.
**Rationale:** D keeps ADR-003/007's "K8s Secrets as the runtime interface" (everything that consumes creds today keeps working) while adding exactly what was missing: one canonical, access-controlled, auditable place operators provision from. ESO is the smallest piece that does that and is backend-portable, which matters because "AWS Vault" is a client direction, not yet provisioned infra.

---

## ADR-035 — Operator-managed JVM heap (the "Memory Operator", issue #55)
**Decision:** Delegate JVM/heap sizing to the **OpenSearch operator's built-in memory management** instead of hand-rendering `-Xms/-Xmx` into the CR. The `OpenSearchCluster` node pool now carries **no `jvm` field**; VeloxSearch drives memory exclusively through `resources` with **request = limit** (one user-facing number, Guaranteed QoS), and the operator computes the heap.

**What the operator manages (grounded in the vendored version — image `v3.0.0-alpha`, chart 3.0.2, ADR-023):** when a node pool's `jvm` string contains no explicit `Xms`/`Xmx`, the operator computes `-Xms/-Xmx = memory request / 2` (in MiB; 512Mi default when there is no request) and injects it via `OPENSEARCH_JAVA_OPTS` — `helpers.CalculateJvmHeapSizeSettings` + `AppendJvmHeapSizeSettings`, read from the operator source at tag `v3.0.0-alpha`. Our previous explicit `jvm` string **overrode** exactly this capability; the meeting's "Memory Operator" is this built-in heap management, not a separate operator/CRD — nothing was invented.

**How it surfaces to users (unchanged knob, new owner):** memory stays the single tuning input — wizard size cards, Advanced custom size, and the Edit tab's "Node memory" (day-2 memory up/down). On save the backend patches the CR's `resources` and the operator recomputes the heap and rolls the nodes. The UI shows the heap the operator *will* derive (resolved server-side via `custom_sizing`; `k8s::heap_mib` mirrors the operator formula for display only) and labels it "operator-managed".

**What changed concretely:**
- `k8s::node_pool()` (extracted, unit-tested): no `jvm`, memory request = limit. `heap_for()` deleted.
- **Sizing presets collapse to one memory number** (their previous *requests*, so scheduling footprints and the capacity planner are unchanged): small 2Gi → heap 1g, medium/large 3Gi → heap 1536m. The former burst headroom (limit > request) is gone deliberately — a data node should never be overcommitted, and with req=lim the "heap = half the memory" rule (meeting 3, ADR-030) is visibly exact.
- **Memory input guard** (`memory_check`): the operator applies no floor/cap of its own, so the backend refuses memory < 1Gi (heap under 512Mi) or > 62Gi (heap past the 31g compressed-oops ceiling) with a clear message — replaces the old silent clamp. Guardrail *validation flows* for day-2 ops stay in #52's lane.
- **Existing deployments** keep their explicit `jvm` (shown verbatim in Status) until their next Edit-save, when server-side apply drops the field and the operator takes over — heap changes to half the (new single) memory and the nodes roll once. No surprise restarts before a user-initiated save.

**Date:** 2026-07-02
**Options:** (A) keep hand-rendering `jvm` (operator capability stays overridden — what #55 asks to stop); (B) delegate but keep request < limit presets (operator halves the *request*, so the displayed "memory" (limit) would visibly break the ½ rule); (C) this — delegate + one memory number at the old request values (footprints unchanged, rule exact).
**Tests:** `k8s::tests` — `node_pool_delegates_heap_to_the_operator` (no `jvm`, req=lim), `heap_mirrors_the_operator_rule` (formula parity incl. the 512Mi default), `memory_check_bounds_the_delegated_heap`, preset/custom profile updates. Live cluster not reachable from the dev host at merge time — the operator-side recompute + roll is asserted from the operator's source and must be confirmed on ct1 alongside #52's memory up/down validation.

---

## ADR-036 — Lead capture persists in a local Postgres on CT123 (socket-only, INSERT-only role)
**Decision:** Replace the landing page's dead Formspree placeholder (`https://formspree.io/f/REPLACE_ME`) with a **first-party ingest on the distribution box itself**: the form POSTs to `/api/lead` on get.veloxsearch.ai; nginx proxies it to a one-file stdlib service (`get-site/lead-api/lead_api.py`, 127.0.0.1:8088) that persists `{email, locale, user_agent≤200}` into a **local Postgres reachable only over its unix socket** (`listen_addresses=''` — no TCP at all). The service's DB role `lead_ingest` holds **INSERT-only** on the single `lead` table (unique on `lower(email)`, `ON CONFLICT DO NOTHING`); there are **no read endpoints** — operators read leads via psql over ssh. On any DB failure the lead spools to `/var/lib/lead-api/spool.jsonl` and the visitor is never shown an error: the response is **always** a 303 back to `/<locale>#lead-thanks` (whitelist `en|es|pt`, default pt), where a CSS `:target` rule reveals the confirmation with zero JS. Bot suppression is a visually-offscreen `website` honeypot (fake success, store nothing) plus an nginx `limit_req` of 6 r/m per IP (burst 3) and a 2k body cap. The unit runs as a dedicated `leadapi` user under `ProtectSystem=strict` + `MemoryMax=64M`; a nightly `pg_dump` keeps 7 days in `/var/backups/lead-api/`. Provisioning is manual per `docs/runbooks/2026-07-07-ct123-lead-api-postgres.md` and must precede the publish of the changed pages.

**Date:** 2026-07-07
**Options:** (A) Formspree/hosted form backend (third party holds the lead list, free-tier limits, another external dependency for a page that is otherwise self-contained); (B) reuse the app's control-plane API (couples marketing capture to the product's deploy cycle and puts PII in the app's DB); (C) SQLite file (no role separation — the web-facing process could read the whole list); (D) this — local Postgres, socket-only, write-only role, spool fallback.
**Rationale:** D keeps the lead list on infrastructure we already run, with the smallest possible exposure: nothing listens on the network but nginx, the ingest path cannot read what it writes, and a DB outage degrades to an append-only spool instead of a lost visitor. The always-303 + honeypot design leaks nothing to bots probing the endpoint.
**Tests:** `get-site/lead-api/test_lead_api.py` (validation/honeypot/locale/redirect logic, DB-free, `uv run pytest`); browser gate on the assembled bundle (clean console/network, `#lead-thanks` hidden by default and revealed on the fragment); end-to-end verify incl. the spool-fallback drill is in the runbook.

---

## ADR-037 — Authenticated read path for captured leads: hidden `/adm` panel, SELECT-only role
**Decision:** Add the first (and only) web read path to the CT123 lead store: `GET /adm` (server-rendered table, newest first, 500/page, spool line-count surfaced) and `GET /adm/export.csv` (all leads, RFC 4180 + CSV-formula-injection-safe), served by the same one-file `lead_api.py`. ADR-036's "no read endpoints whatsoever" is amended, not abandoned — every layer of that threat model keeps an equivalent here: **least privilege** — reads run as a new `lead_read` role (SELECT-only on `lead`; `lead_ingest` stays INSERT-only) over a separate per-request unix-socket connection, so a compromised ingest path still cannot read the list and the read path cannot write it; **exposure** — auth is nginx's job (basic auth over the HAProxy-terminated TLS, htpasswd minted at provision time, never committed), rate-limited 30 r/m per IP with `X-Robots-Tag: noindex, nofollow`; the panel is linked from nowhere — the published bundle is byte-identical and `/adm` is reachable only by knowing the path; **defense in depth** — the service 404s any `/adm` request lacking nginx's `X-Velox-Adm: 1` header, so direct hits on 8088 (localhost-only anyway) are indistinguishable from unknown paths; **injection** — every DB-sourced value (email/user_agent are attacker-controlled POST data) is HTML-escaped in the panel and formula-prefixed + RFC 4180-quoted in the CSV; **no internals leak** — any DB failure is a plain `503 leads db unavailable`, never a spool read (the spool stays write-only from the web; only its line count is shown).

**Date:** 2026-07-07 · **Issue:** #61
**Options:** (A) keep ssh+psql only (reading leads needs the Proxmox host, root, and psql fluency — in practice nobody looks); (B) a separate admin app/subdomain with sessions (a second deployable + session state for one table); (C) broaden `lead_ingest`'s grants (destroys ADR-036's write-only guarantee outright); (D) this — same service, new SELECT-only role, nginx basic auth on an unlinked route.
**Rationale:** D adds the read path while keeping every ADR-036 property enforced by infrastructure rather than code discipline: role separation lives in Postgres grants, auth terminates in nginx before the service sees a byte, and the landing page — the only public, crawled surface — does not change at all.
**Tests:** `test_lead_api.py` (HTML escaping of `<script>` payloads in email/UA, CSV injection prefixing + RFC 4180 quoting, pagination math, the `X-Velox-Adm` 404 gate, spool line count — all DB-free); provisioning + live verification (401 without creds / 200 with, CSV export, ingest still INSERT-only, direct 8088 `/adm` → 404) in the runbook's "Phase 2 — /adm read panel".

---

## ADR-038 — Demo gate: qualified lead capture (name + phone + corporate email)
**Decision:** Turn the bottom-of-page "keep me posted" email-only form into a **demo gate** for qualified leads (Rodrigo's ask, 2026-07-11 meeting): a primary **Demo** CTA at the top of all three locales (`index.{en,pt,es}.html`) scrolls to a form collecting **name + work email + phone**, and the server is the gate. `lead_api.py` gains `validate_name`/`validate_phone` and a corporate-email check, `is_free_email`, that rejects a **denylist** of free/consumer/disposable mailboxes — exact domains and their subdomains (`FREE_EMAIL_DOMAINS`) plus a `<brand>.<public-suffix>` rule so `yahoo.com.br`/`hotmail.fr` are caught without rejecting a real company's `live.acme-corp.io`. An allowlist of "corporate domains" is unenumerable, so it stays a denylist. The always-303 + `:target` no-JS pattern from ADR-036 is kept but split into **three** fragments: `#lead-thanks` (accepted **and** every honeypot drop — a bot must not learn it was caught), `#lead-work-email` (a free mailbox: a *localized message*, because a prospect who typed a personal address is worth telling, unlike a bot), and `#lead-incomplete` (name/phone/email missing or unusable). Order in `process_lead` is deliberate: honeypot first (silent success), then completeness, then the corporate gate. Persistence adds nullable `name` + `phone` columns (online `ADD COLUMN`, existing grants cover them — see the runbook's "Phase 3"); `CSV_COLUMNS`, the CSV export and the `/adm` panel carry both new fields.

**Date:** 2026-07-14 · **Issue:** #62
**Options:** (A) client-side-only validation (trivially bypassed — the whole point is qualifying who asks, so it must hold server-side); (B) allowlist of corporate domains (cannot be enumerated; guarantees false rejects of real prospects); (C) silently drop free-mailbox submissions like the honeypot (wrong — a human who used the wrong address gets no feedback and is lost); (D) this — server-side denylist gate with a friendly, localized "use your work email" message, name+phone captured, honeypot unchanged.
**Rationale:** D qualifies the lead where it cannot be bypassed (the server), tells a mis-addressed human how to fix it while telling a bot nothing, and adds the two fields with a zero-downtime nullable migration. The denylist is conservative (a false reject costs a lead) and the rejection copy still points at the contact mailto, so no visitor is stranded.
**Tests:** `test_lead_api.py` — `is_free_email` (global webmail, subdomains, `<brand>.<suffix>` country mailboxes, real corporate domains pass), `validate_name`/`validate_phone` shape, and the full `process_lead` decision (corporate accept → `#lead-thanks`; free mailbox → `#lead-work-email`, stores nothing; honeypot → `#lead-thanks`, stores nothing even over a free address; missing name/phone/email → `#lead-incomplete`); CSV + `/adm` rendering with the name/phone columns — all DB-free, `uv run pytest`. Live migration + probe verification in the runbook's "Phase 3 — demo-gate fields".

---

## ADR-039 — Integrations are declarative packages in a separate registry, applied by a fixed engine (no code ships from the registry)

**Decision:** Move the 12 built-in monitoring recipes out of the compiled binary (`src/recipes.rs` + `src/agents.rs`) and into a **separate git registry** the core pulls from at runtime. An **integration package is pure declarative data — never code** — and the core keeps a single fixed *apply engine* that knows how to install any package. Adding an integration becomes "commit a directory to the registry"; it never again means editing Rust, recompiling, or rebuilding the image (issue #70; Rodrigo 2026-07-14: "um repositório à parte… como se fosse um plugin", and integrations must "não vir já de antemão" — not ship pre-loaded in the core).

A package is a directory with a `manifest.yaml` plus the four asset kinds that a recipe is *already made of today* — this ADR only relocates and freezes them, it does not invent a new shape:

```
integrations/nginx/
  manifest.yaml            # id, version, title (i18n), index, dashboard slug,
                           #   discovery predicate, teardown list, signature
  pipeline.json            # → PUT {os}/_ingest/pipeline/<id>          (grok/date/json)
  index-template.json      # → PUT {os}/_index_template/<id>           (patterns+mappings)
  saved-objects.ndjson     # → POST {osd}/api/saved_objects/_bulk_create?overwrite=true
                           #   (the index-pattern + dashboard + visualizations,
                           #    RENDERED — see below)
  agent.conf.tmpl          # Fluent Bit config, interpolated then applied as today
```

The rendered-saved-objects point is the load-bearing simplification: today `viz()` / `dashboard_obj()` in `recipes.rs` are a Rust DSL whose **output is already standard OpenSearch-Dashboards saved-object JSON** posted to `/api/saved_objects/_bulk_create`. The extraction is therefore mechanical — freeze each recipe's current generated JSON to `saved-objects.ndjson` — not a rewrite. Likewise `pipeline.json` / `index-template.json` are verbatim the JSON each `configure_*` already PUTs.

**Templating is a closed, enumerated variable set — this is what makes "no code" safe.** Every value the core interpolates into an asset is one of a fixed list the *engine* owns, never something the package can extend: `{deployment}`, `{index}`, `{os_user}`, `{os_password}`, `{os_host}`, `{ns}`, `{tenant}`, `{recipe_id}`. A package supplies data with `{…}` holes; it cannot express a shell command, a URL to fetch, or a code path. So the registry's threat surface is "a malicious mapping / pipeline / dashboard / Fluent-Bit stanza" — bounded and reviewable — **not** arbitrary code execution against a customer's cluster. Any integration that genuinely needs new behaviour (a novel input type, a non-templatable step) is a change to the **core engine** behind normal code review + release, not a package drop.

**Transport = online pull** (operator decision, 2026-07-14): the Velox instance fetches the catalog index and package directories directly from the registry over HTTPS, installs on click. **But the package format is deliberately transport-agnostic** — a package is a self-contained, signed directory, so the offline path Rodrigo also described ("faz um download… e ele vai lá e carrega") stays open for air-gapped/on-prem customers (Ministério-Público-class buyers with no cluster egress) with **no format change** — only a second delivery route. Do not couple the format to the wire.

**Trust:** every package manifest carries a signature over the package contents; the core **verifies before applying** and refuses unsigned/mismatched packages. Because packages are data, verification is the whole security boundary — there is no sandbox to escape because there is no code to run.

**Uninstall becomes real.** Today `recipes::disable()` only calls `remove_agent()` — it leaves the ingest pipeline, index template, and saved objects orphaned in the customer's OpenSearch. A package therefore declares an explicit **`teardown`** list in its manifest (pipeline id, template id, saved-object ids, index-pattern), and the engine's uninstall path removes exactly those. Clean install ⇒ clean uninstall is now a property of the format, not an afterthought.

**Versioning:** each integration is independently semver'd; the registry ships a `catalog.json` index (id → latest version, title, summary, min-core-version). Install and upgrade are the same idempotent operation — the assets are `PUT … / overwrite=true` today, so re-applying a newer version converges. The core records installed id+version per deployment (extends the existing per-deployment `monitors` list) so the Integrations tab can show *installed vs available vs update-available*.

**Date:** 2026-07-15 · **Epic:** #70 · **Depends on:** transport decision (online pull) recorded on #70
**Options:**
- **(A) Status quo — recipes baked into the binary.** Every new integration is a Rust edit → recompile → image rebuild → redeploy of the customer's cluster; the catalog can only grow at the core's release cadence, and a customer cannot get a new integration without a full app upgrade. This is precisely the coupling #70 exists to remove.
- **(B) Plugins that ship code (WASM module / embedded script per integration).** Maximally flexible, but every integration becomes executable code running against a customer's cluster — a supply-chain and sandboxing problem, and a much larger thing to sign, review, and trust. Overkill: the 12 existing recipes are ~90% static JSON + a templated agent config; almost none of them need to *compute* anything.
- **(C) Adopt Elastic's integration-package spec (Fleet/EPR) wholesale.** Battle-tested and familiar to the market we're chasing, but it is a large, Elastic-shaped spec carrying concepts we don't have (Fleet server, agent policies, its own asset taxonomy); adopting it would bend the product to the spec rather than the reverse.
- **(D) This — declarative data packages + a fixed core engine, transport-agnostic, signed.** Matches what recipes already are, keeps all executable behaviour in the reviewed core, makes the registry a low-trust content repo, and preserves both the online and offline delivery stories.

**Rationale:** D turns "add an integration" into a data commit while keeping the only code that touches a customer's cluster inside the core's own release + review gate. The extraction is mostly mechanical because the current recipes are already declarative JSON behind a thin Rust driver; the parts that look like code (the dashboard builder) merely *emit* the saved-object JSON we would otherwise hand-author. The closed interpolation set is the linchpin — it is what lets the registry be an open, fast-moving, low-trust repo without ever handing a customer's cluster an instruction the core did not author. It also fixes the latent uninstall leak in `disable()` as a side effect of having to name a package's assets explicitly.

**Consequences / to settle in the child issues (#70):** the exact manifest schema and signing mechanism (key custody, verification path); how `discover()`'s per-source predicate is expressed as manifest data rather than Rust (feeds #65's suggestion layer); the failure mode when the registry is unreachable (Integrations tab degrades, deployment screen must not break); and whether the core embeds a minimal built-in catalog (e.g. `kubernetes`) as a bootstrap floor so a brand-new, egress-less install is not empty. **The last two are resolved by ADR-047** (#75): stale-tolerant cache with an explicit `stale`/`error` on the wire, and a bootstrap floor of exactly `kubernetes`.
**Tests:** golden test that the frozen `pipeline.json` / `index-template.json` / `saved-objects.ndjson` for each of the 12 extracted integrations are byte-equivalent to the current Rust-generated output (the extraction must not regress any existing recipe); an install→uninstall round-trip leaves the deployment's OpenSearch with zero orphaned pipelines/templates/saved-objects; signature verification rejects a tampered package.

## ADR-040 — OpenSearch deploy target 3.0.0 → 3.7.0 (Agent Traces / AI observability)

**Decision:** Bump the OpenSearch version VeloxSearch deploys — `general.version` and `dashboards.version` in the rendered `OpenSearchCluster` CR (`src/k8s.rs`), the saved-object panel version (`src/recipes.rs`), and `deploy/opensearch-test-cluster.yaml` — from **3.0.0 to 3.7.0**. The vendored operator stays at image v3.0.0-alpha / chart 3.0.2, and the sizing model stays 3 nodes (ADR-016 untouched).
**Date:** 2026-07-17
**Rationale:** OpenSearch ≥3.6 ships **Agent Traces** — AI/LLM observability on OTel GenAI semantic conventions (OTel-based tracing of agent invocations, LLM calls, and tool executions, with DAG and token-usage views in Dashboards; 3.6.0 release notes). That capability is slated to replace Langfuse for LLM tracing, which puts a hard floor of 3.6 under our deploy target. 3.7.0 is the current latest (released 2026-06-09; both `opensearchproject/opensearch:3.7.0` and `opensearchproject/opensearch-dashboards:3.7.0` tags verified live on Docker Hub); **3.6 is the first designated LTS of the 3.x line** (OpenSearch Software Foundation LTS program, ≥18 months) and is the fallback if 3.7.x misbehaves.
**Operator compatibility (extends ADR-009):** the vendored operator is still the newest release — no operator version newer than 3.0.0 exists (the `3.0.0-alpha` image tag is the 3.0.0 release's naming quirk; newest operator chart is 3.0.2). Its README compatibility matrix pins min 2.19.2 and an open-ended "supports the latest OpenSearch 3.x version" max, so no operator bump is needed or possible.
**API compatibility 3.0 → 3.7:** the official breaking-changes ledger (docs.opensearch.org/latest/breaking-changes/) ends at 3.0.0 — no 3.1–3.7 sections — and none of the 3.1.0–3.7.0 aggregate release notes contain a breaking-changes section. Every surface VeloxSearch calls (ISM policies, security-analytics detectors/mappings/rules, `_index_template`, `_ingest/pipeline`, security tenants API, Dashboards saved-objects API) has no documented change. Details + advisories in `docs/research/2026-07-17-opensearch-370-bump-validation.md`; the live-shape verification of `docs/research/2026-06-11-profile-apis-opensearch300.md` therefore carries over to 3.7.0.

## ADR-041 — Control-plane user/tenant store: a small in-cluster Postgres (amends ADR-003 for multi-user state)

**Decision:** Introduce a **small dedicated Postgres inside the app's K3S namespace** as the source of truth for the multi-tenant *control plane* — **users, tenants, tenant↔namespace mapping, membership/ownership, per-tenant quotas, and an audit log** — the moment VeloxSearch stops being a single-admin app (M1, #79/#80/#84). It **mirrors the ADR-036 lead-capture Postgres *pattern*** (least-privilege application role, credentials sourced from a K8s Secret per ADR-034, PVC-backed durability per ADR-031, nightly `pg_dump`) but is a **separate instance, not the ADR-036 box**: CT123's lead DB is `listen_addresses=''` socket-only on the marketing/distribution host and is unreachable from — and a different trust domain than — the k3s control plane (product state vs. marketing PII). **Deployment state does NOT move**: the `OpenSearchCluster` CR remains the single source of truth for what a deployment *is* (nodes, version, storage, recipe status), and per-deployment credentials stay in K8s Secrets. Postgres owns *identity and org* (who exists, who owns which tenant, which namespace a tenant maps to, what that tenant is allowed); the CRs own *deployment shape*. Ownership is stamped onto each CR as a `veloxsearch.io/tenant` label for cheap K8s-side filtering, but that label is a **denormalized cache reconciled from Postgres** — when the two disagree, Postgres wins. Tables are created and versioned by **plain ordered `.sql` files applied by a ~40-line idempotent runner at app startup** (a `schema_migrations(version)` ledger), not a heavy migration framework. This ADR is the *contract*; the Rust query layer, the migration runner, and the auth wiring are #79's build. **The `Done when` of #78 is that an empty-but-migratable schema + bootstrap path exists** — no multi-tenant code ships in this card.

**Date:** 2026-07-20 · **Epic:** #77 · **Blocks:** #79 → #80 (the M1 spine) and #84 (quota gate) · **Amends:** ADR-003 · **Mirrors:** ADR-036 (pattern), ADR-034 (secret sourcing), ADR-031 (PVC durability)

**Options:**
- **(A) Keep K8s-as-datastore (ADR-003) — one Secret/ConfigMap/CRD per user & tenant.** This is exactly what ADR-003 chose for the single-admin case, and it does not carry to multi-user. etcd is a key-value store, not a query engine: "list the tenants over quota", "who are the members of tenant X", and an append-only audit trail are all relational joins that become O(all-objects) scans and hand-rolled indexing over label selectors. There is no referential integrity (a dangling `tenant_users` membership can't be a foreign key), the 1 MB object cap makes an audit log a non-starter (ADR-003 itself already punted audit to OTEL for this reason), and per-object RBAC gymnastics grow with every user. Fine for one admin credential; collapses at N users × M tenants.
- **(B) SQLite on a PVC.** Zero new infra, one file — but it pins the control plane to a single pod holding one `ReadWriteOnce` volume, which fights the app's plan to run more than one replica behind Traefik (no concurrent writers, no HA), and it offers **no role separation**: the web-facing process can read and write everything, so it cannot mirror ADR-036's least-privilege split (the single most valuable property of that pattern). Migrating SQLite→Postgres later is a data move under a schema we'd rather author once.
- **(C) A heavier managed/HA database — cloud RDS, or an in-cluster HA operator (CloudNativePG/Zalando).** Real durability and failover, but it is infrastructure we do not run yet, it adds an operator/vendor dependency and standing cost, and it is disproportionate to a five-table MVP schema. Crucially it is **not a schema decision** — it is the same Postgres — so it can be adopted later with zero migration if load ever demands HA. Choosing it now is premature.
- **(D) A small dedicated Postgres in-cluster mirroring the ADR-036 pattern — RECOMMENDED.** Relational integrity for membership/quotas/audit, a queryable store for the enforcement paths #80/#84 need, a least-privilege application role (no superuser; `SELECT/INSERT/UPDATE/DELETE` scoped to the control-plane tables only), credentials from a Secret, PVC-backed, nightly `pg_dump` — coherent with a database technology and an operational pattern we already run, while the `OpenSearchCluster` CRs stay untouched.

**Rationale:** D is the smallest honest datastore that the M1 spine can be built on. The three things multi-tenancy actually needs from a store — *referential integrity* (membership and ownership are relations), *cheap ad-hoc queries over the org* (quota admission in #84 asks "how many deployments does this tenant have?"; the security review in #87 asks "can tenant A see anything of tenant B?"), and a *durable append-only audit* independent of OpenSearch availability — are precisely the three things ADR-003's K8s-as-datastore cannot give and never claimed to. Reusing the ADR-036 *pattern* rather than the *instance* keeps the least-privilege and operational-durability wins without coupling product state to a socket-only marketing box on another host in another trust domain. Keeping deployment state in the CRs preserves everything ADR-003/ADR-009/ADR-031 got right (the operator reconciles CRs; `kubectl`-inspectable; PVC durability) and draws a boundary #80 can rely on: **ownership is read from Postgres, deployment shape is read from the CR.** Postgres-as-C-when-load-demands means the option-C upgrade path costs nothing structural.

**Schema (MVP sketch — illustrative, the live DDL is #79's migration, not this ADR):**
```sql
-- 001_init.sql (shape only; column types/constraints finalize in #79)
CREATE EXTENSION IF NOT EXISTS citext;          -- case-insensitive email

CREATE TABLE users (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email         citext UNIQUE NOT NULL,        -- lower-cased identity (cf. ADR-036 unique lower(email))
    password_hash text   NOT NULL,               -- bcrypt, same scheme as src/auth.rs today
    email_verified boolean NOT NULL DEFAULT false,-- reuse ADR-038 corporate-email denylist at signup
    status        text   NOT NULL DEFAULT 'active', -- active | suspended
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tenants (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug          text UNIQUE NOT NULL,          -- URL/label-safe tenant handle
    namespace     text UNIQUE NOT NULL,          -- the dedicated K8s namespace (the tenant↔namespace mapping #80 reads)
    display_name  text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tenant_users (                      -- membership + ownership (M:N)
    tenant_id  uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id    uuid NOT NULL REFERENCES users(id)   ON DELETE CASCADE,
    role       text NOT NULL DEFAULT 'member',      -- owner | member
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);

CREATE TABLE quotas (                            -- per-tenant limits; feeds #84 admission gate
    tenant_id       uuid PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    max_deployments integer NOT NULL DEFAULT 1,
    max_total_disk_gb integer NOT NULL DEFAULT 50,
    max_nodes       integer NOT NULL DEFAULT 3,   -- sizing is 3-node per ADR-016
    updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE audit (                             -- who did what; durable, queryable, per-tenant
    id         bigserial PRIMARY KEY,
    at         timestamptz NOT NULL DEFAULT now(),
    actor_user_id uuid REFERENCES users(id),      -- nullable: system/bootstrap actions
    tenant_id  uuid REFERENCES tenants(id),        -- nullable: account-level events
    action     text NOT NULL,                      -- e.g. deployment.create, tenant.invite, login
    target     text,                               -- deployment name / namespace / user
    detail     jsonb NOT NULL DEFAULT '{}'
);

CREATE TABLE schema_migrations (version text PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now());
```

**Where it runs:** a **dedicated in-cluster Postgres** (single-instance StatefulSet, PVC-backed via the ADR-031 storage story), in the app's namespace, reached over the ClusterIP on the pod network — **not** the ADR-036 CT123 instance (socket-only, off-cluster, marketing trust domain). It reuses ADR-036's *operational shape*: a non-superuser application role scoped to these tables, credentials in a K8s Secret sourced per ADR-034, and a nightly `pg_dump`. Trade-off vs. sharing the ADR-036 box: a second small database to operate, bought for a clean trust boundary between product identity data and marketing PII and for reachability from k3s at all. If HA is ever needed, promote to option C in place — same schema, same data, no migration.

**Migrations:** ordered plain-SQL files in-repo (`migrations/NNN_name.sql`), applied at app startup by a tiny idempotent runner that records each applied `version` in `schema_migrations` and skips already-applied files — no Diesel/`sqlx migrate`/framework dependency for a five-table schema. Re-running against a current DB is a no-op; a fresh empty DB migrates to head. (The query layer — `sqlx` vs. `diesel` — is #79's call and out of scope here.)

**Boundary (Postgres vs. CRs / K8s) — the crisp statement #80 relies on:**
- **Postgres (source of truth):** user identity & credentials; tenants; the **tenant→namespace mapping** (`tenants.namespace`); **ownership & membership** (`tenant_users`); per-tenant **quotas**; the **audit log**.
- **`OpenSearchCluster` CRs / K8s (unchanged, ADR-003/009/031):** deployment shape and lifecycle (nodes, version, storage, recipe status); per-deployment Secrets; the operator's reconciliation. Each CR carries a `veloxsearch.io/tenant` **owner label** stamped at create for K8s-side filtering, but that label is a **cache reconciled from Postgres — Postgres is authoritative on ownership; the label is never trusted over it.**
- **Sessions:** stay stateless HMAC-signed cookies (ADR-003, `src/auth.rs`); whether the signing secret moves into this store or remains a K8s Secret is #79's call.

**Consequences / to settle in #79/#80/#84:** the Rust query layer and connection/secret wiring (ADR-034 vault); the email-verification token lifecycle (reuse the ADR-038 corporate-email denylist at signup); the reconciler that keeps CR owner-labels in sync with Postgres (and the failure mode when they drift); whether the HMAC session secret migrates into Postgres; and the exact quota columns #84 enforces against.

**Tests (bootstrap-scope for #78; auth/ownership behaviour belongs to #79/#80):** migrations apply cleanly from an empty database to head and are **idempotent** (re-running is a no-op); after bootstrap the five control-plane tables + their constraints exist; a DB outage during startup fails closed (the control plane does not serve unauthenticated) rather than silently degrading.

**Amendment to ADR-003:** ADR-003 ("Kubernetes as the datastore — NO external DB") **stands for the single-admin era** — the managed-credentials Secret, stateless session cookies, and CR-as-deployment-truth are all unchanged. For **multi-user control-plane state (users/tenants/quotas/audit)** this ADR supersedes it: that state moves to Postgres. ADR-003's own carve-out (audit → OTEL/OpenSearch because ConfigMaps can't hold an append-only log) is subsumed here — the **authoritative** control-plane audit lives in the `audit` table so it is queryable per-tenant and survives independently of OpenSearch; OTEL/OpenSearch telemetry (ADR-008) continues for observability, not as the accountability system of record.
## ADR-042 — Snapshot repo moves from a shared `fs` repo to an S3-compatible object store, registered per tenant; default in-cluster MinIO, external bucket as a per-deployment override

**Decision:** Amend the `fs`-repo premise behind the snapshot foundation (`src/profiles.rs`: `SNAPSHOT_REPO = "velox-snapshots"`, the ISM `hot → snapshot → delete` policy, and `ensure_retention`'s best-effort `{"type":"fs", …}` registration) to an **S3-compatible object store** (`repository-s3`), registered **per tenant / per deployment**. The `fs` repo needed a `path.repo` shared across all nodes — unworkable once each tenant is an isolated namespace on the shared K3S (a shared `path.repo` PV mounted into every tenant's pods would let one tenant read another's snapshot files, breaking the exact isolation the MVP hinges on). The S3 repo removes the shared-filesystem requirement: every deployment talks to a bucket over the S3 API with its own scoped credential.

**Backend — support both behind one repo config; default to in-cluster MinIO, external S3 as a per-deployment override.** The repo settings differ only by `endpoint` + credentials, so a single `type: s3` code path serves both; the operator chooses per deployment (platform default = MinIO). **MinIO is deployed once as tenant-agnostic platform infra** (one shared MinIO on its own dedicated storage — the ADR-036 precedent: we already run small in-cluster infra, one Postgres for all leads; here one MinIO for all tenants' snapshots), **not** a MinIO per tenant. Isolation is per-tenant *within* that MinIO, at the bucket boundary (below).

**Per-tenant layout — bucket per tenant, `base_path` per deployment.** Bucket-per-tenant (`velox-snap-<tenant>`), with the OpenSearch `_snapshot/velox-snapshots` repo pointing at that bucket and `base_path = <deployment>` (a tenant may hold more than one deployment). Chosen over shared-bucket-with-prefix because the roadmap makes cross-tenant isolation the make-or-break security property, and a bucket boundary is a harder wall than an `s3:prefix`-conditioned IAM policy (one prefix-policy misconfiguration leaks every tenant); it also mirrors the one-boundary-per-tenant model (tenant → dedicated namespace) and makes offboarding a single bucket drop. Repo settings: `{ bucket, base_path, endpoint, path_style_access: true }` (`path_style_access` is required for MinIO and harmless against most external S3).

**Credentials — per-deployment scoped key, in the OpenSearch keystore, never in repo settings.** Each deployment gets its own MinIO/S3 access key scoped (bucket policy) to exactly its tenant bucket, stored as a per-deployment K8s Secret in the tenant namespace and loaded into the OpenSearch keystore as `s3.client.<name>.access_key` / `secret_key` (the `repository-s3` plugin reads credentials from the keystore, not from the repo body). A compromised deployment credential can touch only its own bucket — the ADR-036/037 least-privilege pattern, enforced by IAM rather than code discipline.

**Lifecycle — register at deployment creation, not lazily in `ensure_retention`.** Today `ensure_retention` best-effort-registers the `fs` repo on every profile apply. Move bucket creation + scoped-key mint + keystore load + `PUT _snapshot/velox-snapshots` into the **per-tenant provisioning path** (`src/bootstrap.rs` namespace/quota bootstrap), so the repo exists before the ISM snapshot state — or an on-demand snapshot (#83) — first needs it; `ensure_retention` stops owning registration and simply references the repo (an idempotent re-assert is acceptable, but it is no longer the source of truth).

**Date:** 2026-07-17 · **Issue:** #82 · **Amends:** the `fs`-repo premise in `src/profiles.rs` (ADR-005/028 notes) · **Unblocks:** #83 (backup/restore build)
**Options:**
- **(A) Keep the `fs` repo.** Requires a `path.repo` reachable by every node of every tenant's cluster — i.e. a shared filesystem. On an isolated-namespace-per-tenant SaaS there is no such shared path that does not also become a cross-tenant read channel; the current registration is best-effort precisely because that prerequisite is rarely met. Fails the multi-tenant isolation requirement outright — this is the premise #82 exists to amend.
- **(B) In-cluster MinIO.** Self-contained: no external dependency, no cloud account, no egress, works for the air-gapped / on-prem buyers the product already commits to (ADR-039's offline story, Ministério-Público-class clusters with no egress). Cost: it consumes storage and ops on the same finite 3-node K3S whose capacity is the anchor concern, and snapshots living on the same cluster they back up are **not** durable against whole-cluster loss — it protects pod-reschedule + accidental-delete (the Phase 8 / ADR-031 SLO), not disaster recovery.
- **(C) External S3 bucket.** Off-cluster and genuinely disaster-durable (survives cluster loss — the truest form of "backup"), and it offloads storage off the scarce 3-node cluster. Cost: it adds a hard external dependency, per-tenant cloud credential + egress management, and a running bucket Tornis must operate/pay for — friction for a product that is otherwise self-contained, and a non-starter for air-gapped buyers.
- **(D) This — one `type: s3` repo config supporting both, default in-cluster MinIO, external bucket as a per-deployment override; MinIO as shared platform infra; bucket-per-tenant, `base_path` per deployment; keystore-scoped per-deployment credentials; registered at provisioning time.**

**Rationale:** D gets the MVP — a gated free beta on our own shared K3S — to "per-tenant snapshots work" fastest with zero external dependency, honouring the product's self-contained, air-gapped-capable DNA and reusing the "run small in-cluster infra" precedent (ADR-036). It does not paint us into a corner: because B and C are the same code path differing only by endpoint + credentials, an operator who wants true off-site durability sets an external bucket per deployment without a format or code change — the same transport-agnostic instinct as ADR-039. The honest caveat is stated, not hidden: in-cluster MinIO on the backed-up cluster is reschedule/accidental-delete protection, not DR; the external override exists exactly for operators who need DR. Isolation is enforced by infrastructure (bucket boundary + IAM-scoped keystore credential), not by code remembering to filter — the ADR-036/037 discipline carried forward.

**Out of scope (MVP):** billing/cost controls. Snapshot volume is already bounded by the ISM age window (`hot → snapshot → delete`); do **not** build object-lifecycle tiering, cross-region replication, or per-tenant storage-cost metering for the free beta (roadmap Part 4 — billing is out of MVP scope). MinIO HA/erasure-coding sizing and external-bucket DR topology are ops-track concerns, not this decision.

**Consequences / to settle in #83:** the on-demand + scheduled (SM policy) snapshot routes and the restore flow build against this repo contract; the concrete MinIO deployment manifest + its dedicated storage (its own PVC/StorageClass, so snapshot storage is accounted separately from live-index storage on the capacity meter); the exact scoped-bucket IAM policy shape (MinIO policy vs external-provider IAM); and the offboarding path (drop the tenant bucket + key on tenant deletion). Proof-of-concept for "done" (per #82): a deployment registers `velox-snapshots` against MinIO and takes one snapshot.
**Tests:** a pure assertion that the rendered repo body is `type: s3` with `bucket`/`base_path`/`endpoint`/`path_style_access` and carries **no** inline credentials (they live in the keystore); a provisioning round-trip that registers the repo and takes + lists one snapshot against in-cluster MinIO (ct1); a cross-tenant probe confirming tenant B's deployment credential is rejected (403) against tenant A's bucket — the isolation property, verified at the storage layer, folded into the Month-3 security review (roadmap Part 6).

## ADR-043 — Longhorn is the only deployment storage (amends ADR-031/R3)

**Decision:** Longhorn is the **only** supported storage for OpenSearch deployments. The storage gate stops asking "is there a real default StorageClass?" and asks "is the `longhorn` SC (provisioner `driver.longhorn.io`) present and usable?" — a foreign real CSI default (EBS/gp2, Ceph…) no longer satisfies it; Longhorn is auto-installed and used regardless. The OpenSearchCluster CR pins `nodePools[].persistence.pvc.storageClass: longhorn` instead of falling through to the cluster default.

**Why (owner directive, 2026-07-27):** a predictable storage layer — one tested path for every deployment — and explicit PVC pinning so a cluster's default class can never silently redirect deployment volumes. Foreign-CSI support can return later behind a tested gate.

**What changed:** `needs_longhorn()` semantics (true unless the Longhorn SC exists — `bootstrap.rs` classifier now `DeploymentStorage`); the `storageClassName` pin in the CR builder (`k8s.rs::node_persistence`); and structured missing-node-package UX — Longhorn `MissingDependency` messages map through an extensible table to `{node, package, reason, install:{debian,ubuntu,arch}}`, served via `storage_status.missing_packages` and rendered in the create flow with copyable per-distro commands, blocking creation until resolved. The auto-install-then-inform flow (2026-06-30) is unchanged; refusals now name Longhorn explicitly.

**Date:** 2026-07-27 · **Amends:** ADR-031, REQUIREMENTS.md R3

**Amendment 2026-08-12 — deleting a deployment reclaims its volumes.** StatefulSet `volumeClaimTemplates` PVCs carry no ownerReference, so nothing ever collected them: `delete_cluster` removed the CR, the ingress, the Secrets and the agents, and left every data volume Bound forever. Under Longhorn each orphan keeps its slice of the **scheduling budget** reserved even though the bytes are free, so the leak is not merely untidy — it is fatal. Measured live: 27 orphans from 11 deleted deployments held 135 GiB of a 157 GiB budget (disk max 224 GiB minus the 30% default-disk reserve), after which every new volume came up with zero replicas (`faulted`, "No available disk candidates to create a new replica") and the pod hung on `AttachVolume.Attach failed … volume is not ready for workloads`. Note the failure does **not** look like a full disk: `storageAvailable` still read 80 GiB and the disk still reported `Schedulable: True`.

`delete_cluster` now sweeps the PVCs, selecting by the operator's `opensearch.org/opensearch-cluster` label rather than by name prefix — that catches both the per-node `data-*` claims and the `<name>-bootstrap-data` one, and cannot misfire on a sibling deployment whose name shares a prefix. **Ordering is load-bearing:** the sweep waits for the StatefulSet to disappear first, because its controller recreates each PVC as fast as they are deleted. That wait can run to tens of seconds, so the sweep is detached from the HTTP response. **Known hole:** an app restart between the delete and the end of the sweep leaks the volumes again; closing it properly means stamping an ownerReference on the PVCs so the Kubernetes garbage collector owns the lifecycle.

Corollary found the same day: the runtime Role granted **no PVC verbs at all**, so `data_pvcs` — which degrades silently to an empty map by design — had been feeding the Overview's PVC meter (ADR-031) nothing since the bootstrap cluster-admin binding was revoked. The Role now grants `get/list/watch/delete`.

## ADR-044 — Multi-tenant isolation model: namespace-per-tenant, default-deny NetworkPolicy, rendered ResourceQuota/LimitRange

**Decision:** Each tenant gets a **dedicated Kubernetes namespace** (`velox-t-<slug>`, recorded in `tenants.namespace` per ADR-041), provisioned at signup together with three cluster-level primitives rendered from vendored templates (`deploy/tenant-templates/`, the ADR-022 `include_str!` + token-replace precedent): a **ResourceQuota** derived from the tenant's ADR-041 `quotas` row, a **LimitRange** supplying default container requests/limits, and a **default-deny NetworkPolicy set** that allows intra-namespace traffic plus five enumerated flows and nothing else. The tenant's `OpenSearchCluster` CRs, per-deployment admin Secrets (ADR-030), and dashboards Ingresses move into that namespace; the control plane, operator, and Postgres stay in the app namespace. This is the **cluster-level floor under the app-layer walls**: #80's per-request ownership enforcement remains the first line, and these primitives are what still holds when an app-layer check is buggy or bypassed. The isolation story is now five layers, each independently owned: **(1)** app-layer ownership (#80, Postgres-authoritative per ADR-041) → **(2)** namespace boundary + namespaced Secrets/RBAC (this ADR) → **(3)** NetworkPolicy default-deny between tenant namespaces and toward the control plane (this ADR) → **(4)** ResourceQuota/LimitRange against noisy neighbours (this ADR, backstopping #84's admission gate) → **(5)** per-deployment OpenSearch admin creds (ADR-030) and per-tenant snapshot buckets with keystore-scoped keys (ADR-042). **This ADR ships the model and the templates only** — the provisioning wiring (`src/bootstrap.rs`/`src/k8s.rs`) lands as a follow-up after #92's datastore bring-up merges, because it threads through the same core surfaces #92 is changing.

**Date:** 2026-08-01 · **Issue:** #81 · **Epic:** #77 · **Builds on:** ADR-041 (`tenants.namespace` mapping, quotas table), ADR-042 (bucket-per-tenant symmetry), ADR-030 (per-deployment creds), ADR-043 (Longhorn pin), ADR-002 (RBAC split) · **Composes with:** #80 (ownership), #84 (admission gate), #87 (security review)

**Options:**
- **(A) Shared namespace + app-layer ownership only (status quo extended).** Every tenant's pods, Secrets, and CRs live side by side in one namespace; isolation is purely code discipline in `src/api.rs`. One missed ownership check — the exact class of bug #80 exists to hunt — exposes every tenant's Secrets and 9200 endpoints to every other tenant's pods, because the pod network is flat and namespaced RBAC distinguishes nothing. ResourceQuota is namespace-scoped, so per-tenant resource capping is structurally impossible here. This is defense-in-depth with one layer; rejected.
- **(B) Namespace per deployment.** Maximum granularity, but the unit of trust, quota, and offboarding is the **tenant** (ADR-041 keys `quotas` by `tenant_id`; ADR-042 buckets per tenant): a tenant's own deployments have no adversarial relationship to justify walls between them, quota would have to be split/rebalanced across N namespaces to enforce one tenant-level number, and offboarding becomes a multi-namespace sweep. Object count triples for zero security gain over C.
- **(C) Namespace per tenant — RECOMMENDED.** One namespace = one tenant = one trust boundary: the ResourceQuota IS the tenant quota (namespace-scoped, one object), the NetworkPolicy wall sits exactly on the tenant boundary, offboarding is one namespace delete (plus the ADR-042 bucket drop), and it is the model ADR-041 (`tenants.namespace UNIQUE`) and ADR-042 already presume.
- **(D) Cluster-per-tenant (vcluster / separate K3S).** The real ECE answer and the strongest wall, but disproportionate for a gated free beta on a single finite 3-node K3S — a vcluster's own control plane per tenant would eat the capacity #84 exists to protect. Can be revisited post-MVP for paid tiers without invalidating C: the tenant→namespace mapping generalizes to tenant→cluster.

**Rationale:** C is the smallest boundary that makes every remaining layer enforceable by the cluster instead of by code discipline. The pattern follows ADR-041/042's one-boundary-per-tenant rule (namespace ↔ Postgres tenant row ↔ snapshot bucket), so every subsystem agrees on where the wall is. Verified against the codebase (2026-08-01): today ALL deployments share `ns()` (`veloxsearch-test` on Tornis prod), there is **no** NetworkPolicy/ResourceQuota/LimitRange anywhere in `src/` or `deploy/`, and the vendored operator (ClusterRole + ClusterRoleBinding, no watch-namespace flag) already reconciles CRs in **any** namespace — so C needs no operator change, only where WE create things.

**The primitives (templates in `deploy/tenant-templates/`, tokens `VELOX_*` replaced at provision time — the `operator_bundle()` precedent):**
- **Namespace** — `velox-t-<slug>` (slug is already URL/label-safe per ADR-041; the `velox-t-` prefix is reserved, and provisioning refuses names colliding with system namespaces). Labeled `veloxsearch.io/tenant: <slug>` + `veloxsearch.io/managed-by: veloxsearch` — the same owner label ADR-041 stamps on CRs, giving one label vocabulary for K8s-side filtering and NetworkPolicy selection.
- **ResourceQuota** — rendered from the tenant's `quotas` row, not hardcoded: `count/opensearchclusters.opensearch.org = max_deployments` (the cluster-side echo of #84's app-layer gate); `requests.storage = max_total_disk_gb`; `persistentvolumeclaims`, `pods`, cpu/memory sums derived from `max_deployments × max_nodes ×` the largest sizing preset (`src/k8s.rs sizing()`: 3Gi mem, 1 CPU req / 2 lim per node) plus dashboards headroom. Worked default for the ADR-041 default row (1 deployment, 3 nodes, 50Gi): pods 8, requests.cpu 4, limits.cpu 7, requests.memory 10Gi, limits.memory 12Gi, requests.storage 50Gi, persistentvolumeclaims 6, count/opensearchclusters 1. When a quota row changes, the control plane re-renders and re-applies the ResourceQuota (SSA, idempotent) — Postgres stays authoritative, the quota object is a rendered cache, same discipline as the ADR-041 owner label.
- **LimitRange** — default `requests`/`limits` for containers that declare none. This is not optional polish: a namespace with a cpu/memory ResourceQuota **rejects** any pod whose containers omit requests, and while operator-built OpenSearch pods carry resources from the CR (Guaranteed QoS per ADR-035), auxiliary pods (dashboards sidecars, future jobs) may not. The LimitRange makes the quota safe to enforce.
- **NetworkPolicy** — default-deny ingress AND egress (empty podSelector), then allow exactly: **(1)** intra-namespace (OpenSearch transport 9300, dashboards→9200); **(2)** ingress 9200/5601 from the control-plane namespace (SSE polling, profile/ISM apply, operator user/securityconfig management — operator and app share that namespace); **(3)** ingress 5601/9200 from the ingress-controller namespace (`<name>.veloxsearch.ai` dashboards); **(4)** ingress 9200 from `velox-agents` (`src/agents.rs` ships to `{deployment}.{ns}.svc:9200`); **(5)** egress DNS (53/udp+tcp → kube-system) and egress 9000 to the MinIO platform namespace (ADR-042 snapshots). Namespace peers select by immutable `kubernetes.io/metadata.name`, not hand-set labels. Tenant→tenant traffic matches no rule and dies at the default deny; so does tenant→control-plane (**egress** deny in the tenant namespace — the "toward the control plane" posture holds even before the control-plane namespace gets its own ingress policy, which is deferred: default-deny there must first account for API-server→operator webhook traffic, an enumeration that belongs to the #87 review).

**Enforcement caveat (stated, not hidden):** NetworkPolicy is enforced by the CNI, not the API server. K3s ships an embedded network-policy controller alongside flannel, so Tornis prod enforces these objects; a **generic install (ADR-027) on a CNI without policy support accepts them and silently enforces nothing.** Therefore the conformance fleet (ADR-026) gains a live probe — a pod in tenant A attempts tenant B's 9200 and must time out — and #87 treats "NetworkPolicy objects exist" as insufficient evidence; only the probe counts (same standard as ADR-042's cross-tenant 403 bucket probe). Kubelet-probe exemption under default-deny is likewise CNI-dependent and is covered by the same conformance probe.

**Migration path (existing deployments):** the current shared namespace is **grandfathered, not migrated**. A `legacy` tenant row maps `namespace = veloxsearch-test` (the mapping supports it — one tenant, one namespace), existing deployments become that tenant's, and **no new tenants are ever admitted into it**. NetworkPolicy inside a shared namespace cannot distinguish tenants, so the legacy namespace keeps app-layer-only isolation until its deployments move; the beta's isolation guarantee applies to namespaced tenants, i.e. all new signups. Per-deployment migration out of legacy is optional and operator-driven: recreate the CR in the tenant namespace and restore data via ADR-042 snapshots (#83), or re-bind the Longhorn volume through a new PV/PVC pair (PVCs are namespace-scoped; the Longhorn volume itself survives). No forced bulk move — the MVP needs new tenants isolated, not old data relocated under deadline.

**Composition:** #84's admission gate stays the **polite** wall (Postgres quota × `capacity.rs deployment_fit` → admit or waitlist, checked before provisioning); the ResourceQuota is the **hard** wall behind it (a bug that skips the gate still cannot exceed the namespace quota — it gets an API rejection instead of a clean waitlist). #80 keeps the request-path wall; this ADR bounds the blast radius when it fails. #87 inherits three verifiable claims: the cross-tenant network probe, the quota rejection, and the legacy-namespace boundary ("no non-legacy tenant in `veloxsearch-test`").

**Follow-up wiring (lands after #92 merges — same surfaces, sequenced to avoid conflict):**
1. `src/bootstrap.rs` (or a new `src/tenant.rs`): `provision_tenant(slug, quota_row)` — `include_str!` the four templates, token-replace, SSA-apply in kind order (Namespace first), idempotent re-run; called from signup and from quota-row updates.
2. `src/k8s.rs`: deployment-scoped `Api`s (`crs()`, Secrets, Ingress — today hardwired to `ns()`) take the tenant namespace resolved from Postgres; the dashboards Ingress is created in the tenant namespace (an Ingress backend must be a same-namespace Service).
3. RBAC widening in `deploy/k8s/veloxsearch.yaml` / `install.yaml` (#92's surface — explicitly deferred): `veloxsearch-runtime` gains create/update on `resourcequotas`, `limitranges`, `networkpolicies`, and its Secrets/Ingress CRUD — today two namespaced Roles in the app namespace — must extend to tenant namespaces (RBAC cannot scope "namespaces matching a label", so either a ClusterRole for those resources or a per-tenant Role stamped at provision time; decide in the wiring MR, per-tenant Role is the least-privilege default).
4. `src/discovery.rs`: tenant namespaces (label `veloxsearch.io/managed-by=veloxsearch`) join the exclusion list so one tenant's discovery view never lists another tenant's workloads (belt to #80's braces).

**Tests (wiring MR):** unit — quota rendering from a `quotas` row matches the worked default, and template token-replacement yields valid manifests for a property-tested slug set; integration (ct1) — `provision_tenant` is idempotent, a pod in tenant A cannot reach tenant B's 9200 (the probe, automated), a CR exceeding `count/opensearchclusters` is rejected by the API server, and a request-less pod schedules under the LimitRange defaults instead of being quota-rejected.

### ADR-044 amendment — how layer 1 is enforced, and the label domain (#80, 2026-08-05)

ADR-044 named app-layer ownership as layer 1 but left *how* it is enforced to
#80. It is enforced by the type system, not by discipline: `src/scope.rs`
introduces a `Scope` (from the signed session, admin or one tenant) and a
`Deployment` capability token with **no constructor outside that module**. Every
deployment-touching function in `k8s.rs`, `recipes.rs`, `metrics.rs`,
`profiles.rs`, `integrations.rs`, `catalog.rs` and `agents.rs` now takes
`&Deployment` instead of `&str`, so a handler that skipped the ownership check
would have nothing to pass and **would not compile**. `api::ROUTES` is the
matching audit table — one declared policy per route, and `api::routes()`
refuses to mount a path that has no entry — so a new route cannot ship
unreviewed. Three decisions this settles:

- **Owner label is `veloxsearch.ai/tenant`, not `veloxsearch.io/tenant`.**
  ADR-041 and ADR-044 both write `.io`; that is an **erratum** in the ADR text.
  The CRs have carried `veloxsearch.ai/size`, `/purpose`, `/monitors` and
  `/auth-kind` since the beginning, and one product must not run two label
  domains. Read `veloxsearch.ai/` wherever those ADRs say `veloxsearch.io/`.
  The value is `tenants.id` (the ADR-041 primary key), not the slug, so a
  future display-name/slug change cannot orphan a CR. Every object a deployment
  owns — CR, admin Secret, dashboards Ingress, auth Secrets — carries it plus
  `veloxsearch.ai/managed-by: veloxsearch`, so offboarding and audit are one
  selector.
- **The legacy admin session is the super-tenant, explicitly.** A v1 token
  (`user:exp:sig`, mintable only by the installation admin credential) resolves
  to `Scope::Admin`: every namespace, no label selector, `require_admin()`
  satisfied. Scoping the operator at the app layer would buy nothing (they hold
  `kubectl` on the cluster) and would break bootstrap, access settings and
  support access to a customer deployment. It is also what makes
  `VELOX_MULTITENANT_AUTH=off` a genuine no-op: with the flag off every session
  is v1, so every query is the single-namespace, no-selector one that shipped
  before #80.
- **"Not yours" is answered exactly like "does not exist" — 404 / `null`, never
  403.** Not by convention but by construction: a tenant's lookup is issued
  against its own namespace with its own label selector, so a foreign name and
  an unknown name traverse the same code path. A 403 would confirm the resource
  exists, which is the fact an attacker probes for. The same rule makes
  admin-only routes answer 404 to a tenant rather than 403.

**Still owed by #81's provisioning MR (named, not hidden):** the tenant
namespace must exist with its Role/RoleBinding before the flag can be turned on
— `veloxsearch-runtime`'s Secrets/Ingress permissions are two namespaced Roles
in the app namespace today (ADR-044 wiring item 3), so with the flag on a
tenant's `create_cluster` fails closed on the admin-Secret write until that
lands. The `OpenSearchCluster` permissions are already a ClusterRole, so the CR
reads/writes and the admin's cross-namespace list work as-is. Two smaller items
ride along: a `kubernetes.io/tls` Secret named in the access config lives in the
app namespace and an Ingress can only reference a same-namespace Secret, so
per-tenant dashboards TLS needs that Secret mirrored at provision time; and
`discovery.rs`'s exclusion list (wiring item 4) is unneeded for now only because
`/discover` is admin-only.

## ADR-045 — External identity providers per deployment (auth provider axis)

**Status:** PROPOSED — **rev. 2** (2026-08-02) supersedes rev. 1 ("Directory-backed auth (AD/LDAP) as a deployment auth profile", 2026-07-31). Spike, not committed (issue #56, backlog). Rev. 1 scoped the feature to AD/LDAP only; the product requirement is *any* external identity provider OpenSearch supports, opt-in per deployment, configured from a screen in the app.
**Date:** 2026-07-31, revised 2026-08-02

**Decision (proposed):** Introduce an **auth provider** as a new axis of a deployment — optional, orthogonal to sizing (ADR-035) and purpose (ADR-017), defaulting to `internal` so every existing deployment is unchanged. A deployment's auth provider has a `kind` and a kind-specific config:

| `kind` | Backs | Cluster-side artifact | Dashboards-side artifact |
|---|---|---|---|
| `internal` (default) | internal users / basic auth | none (operator default securityconfig) | none |
| `ldap` | Active Directory, OpenLDAP, FreeIPA | `authc` LDAP domain + `authz` roles-from-LDAP | none (basicauth login works as-is) |
| `oidc` | Keycloak, Entra ID (Azure AD), Okta, Google, Auth0 | `authc` `openid` domain (JWT validation) | **required** — `auth.type: openid` + discovery URL + client id/secret + redirect base |
| `saml` | ADFS, Keycloak, Okta, OneLogin | `authc` `saml` domain (IdP metadata, exchange key) | **required** — `auth.type: saml` + XSRF allowlist + `session.keepalive: false` |
| `jwt` | machine-to-machine / upstream gateway | `authc` `jwt` domain | none (API-only, no Dashboards login) |
| `proxy` | header injection behind a trusted reverse proxy | `authc` `proxy` domain + `xff` | none |

The mechanism is **two generated artifacts, not one** — this is rev. 1's central omission:
1. **Cluster side** — our own `spec.security.config.securityConfigSecret` on the `OpenSearchCluster` CR: a `config.yml` carrying the provider's `authc` (+ `authz` for LDAP) domain, plus `roles_mapping.yml` mapping directory groups / IdP claims (backend roles) onto OpenSearch roles.
2. **Dashboards side** — `spec.dashboards.additionalConfig` (the operator renders it into `opensearch_dashboards.yml`). `oidc` and `saml` are **browser redirect flows**: without this block the IdP is configured on the data plane and the Dashboards login page still shows a username/password box that the IdP can never satisfy. LDAP needs nothing here precisely because it rides the same basic-auth challenge.

The VeloxSearch panel login (`auth.rs`) is untouched: this governs who logs into each deployment's OpenSearch Dashboards / API, not the wizard.

**Context:** Enterprise buyers expect directory-backed auth and SSO; its absence is an adoption barrier (#56). Today `src/k8s.rs:709-714` builds `spec.security` with TLS + `adminCredentialsSecret` only, never sets `securityConfigSecret`, and `spec.dashboards` (`src/k8s.rs:715-723`) carries no `additionalConfig` — every cluster runs the operator's default securityconfig (internal users / basic auth) and there is no code path for external auth of any kind. The operator does expose everything needed (`.claude/memory/opensearch-operator-capabilities.md`): `security.config.securityConfigSecret`, `dashboards.additionalConfig`, `general.keystore`, `general.additionalVolumes`, and the aux CRDs. Naming collision to avoid: the "profiles" in `profiles.rs` are *purpose* profiles (observability/security/search); rev. 1 called this an "auth profile" — rev. 2 renames it **auth provider** so the two axes never read alike in code or UI.

**What rev. 1 did not cover (the gap that forced this revision):**
- **Only `ldap`.** Keycloak/OAuth2/OIDC was filed as a rejected option (B, "out of scope for #56"); SAML, JWT and proxy were absent. The requirement is a provider *axis* with several kinds, LDAP being one.
- **No Dashboards-side configuration at all.** OIDC and SAML are structurally impossible without `spec.dashboards.additionalConfig`. Rev. 1's design would have shipped a cluster that authenticates against Keycloak on `:9200` and cannot log in on `:5601`.
- **No lockout invariant.** A generated securityconfig that leaves only an external domain locks every human out of a cluster the moment the IdP is unreachable.
- **No public-URL / TLS gate.** OIDC and SAML redirect through the browser: they require a stable HTTPS origin (`https://<name>.veloxsearch.ai`) for `base_redirect_url` / SP `kibana_url` and `cookie.secure`. In `portforward` access mode (ADR-027) there is no such origin.
- **No connection test, no day-2 story.** Auth misconfiguration is discovered as "cluster restarted and nobody can log in". Needs a pre-save probe, a change/rollback path, and an "applying" state.
- **No UI.** Rev. 1's MR3 said "create/settings UI" with no spec, while the screen *is* half the requested feature.

**Options:**
- (A) **`securityConfigSecret` + `dashboards.additionalConfig`, one generator per kind (this).** Native security-plugin mechanism, applied by the operator's `securityadmin` updateJob; full control over authc + authz + roles_mapping *and* the Dashboards login mode. Same code path for every kind — kinds differ only in the rendered YAML.
- (B) LDAP only, OIDC/SAML later. Rejected in rev. 2 — the axis costs the same to build for N kinds as for one (the generator is a `match` on kind), and retro-fitting the Dashboards-side artifact later would rewrite the CR wiring and the screen.
- (C) Aux CRDs (`OpensearchUser`/`OpensearchRole`/`OpensearchUserRoleBinding`) only. These manage internal users/roles and can express **backend-role → role mapping** without replacing the securityconfig, but cannot stand up an external authc domain. **Promoted from "rejected" to a complement**: see the open question on partial securityconfig below.
- (D) Front every deployment's Dashboards with our own reverse-proxy SSO (oauth2-proxy at the ingress) instead of configuring OpenSearch. Rejected — it authenticates the *page*, not the *cluster*: the security plugin still sees an anonymous request, so index-level authz and audit lose the user identity. Also breaks direct API clients.
- (E) Leave auth internal-only. Rejected — that is the barrier #56 exists to remove.

**Rationale:** A is the operator-native path and the only single mechanism that satisfies "external authentication + security-domain mapping + working Dashboards login". Modeling it as a selectable *provider* rather than a bespoke setting keeps it composable with sizing/purpose and matches the thin-slice-with-presets DNA (ADR-015): the screen offers a short list of IdP presets, not a raw YAML editor. Both artifacts are produced by **pure functions** (mirroring `retention_policy()` / `mappings_body()` / `dashboards_ingress_manifest()`), so every kind's securityconfig and Dashboards block is asserted in unit tests without a live cluster.

**Per-kind config surface** (to document in `docs/auth/{ldap,oidc,saml}.md`):
- **`ldap`** — `hosts` (host:port list), `bind_dn` + bind password, `userbase`/`usersearch` (`(sAMAccountName={0})` for AD), `username_attribute`, `rolebase`/`rolesearch`, `userrolename`/`rolename`, `resolve_nested_roles`, TLS (`enable_ssl` / `enable_start_tls`, `pemtrustedcas_content`).
- **`oidc`** — discovery URL (`.../.well-known/openid-configuration`), `client_id`, `client_secret`, `subject_key` (default `preferred_username`), `roles_key` (default `roles`), extra `scope`, optional `pemtrustedcas_content` for a private CA. Dashboards block additionally needs `base_redirect_url` = the deployment's public HTTPS host and `logout_url`.
- **`saml`** — IdP `metadata_url` (or inline metadata), IdP `entity_id`, SP `entity_id`, `kibana_url`, `roles_key`, `exchange_key` (shared secret), optional signature/encryption certs.
- **`jwt`** — `signing_key` (or JWKS URL), `jwt_header`, `jwt_url_parameter`, `subject_key`, `roles_key`.
- **`proxy`** — `user_header`, `roles_header`, `roles_separator`, trusted `internalProxies` regex (`xff` block).
- **Security-domain mapping (all kinds):** the authz side yields backend roles (LDAP groups, or the `roles_key` claim); `roles_mapping.yml` maps them onto OpenSearch roles (`all_access`, `kibana_user`, `readall`, custom per-tenant). Expressed in the app as an ordered list of `directory group / claim value → OpenSearch role` rows.

**Invariants the generator must enforce (non-negotiable):**
1. **Break-glass.** The internal basic-auth domain is *always* emitted alongside the external domain (lower `order`, `challenge: false` when an external browser flow owns the login page), and the `adminCredentialsSecret` admin keeps `all_access`. On Dashboards, `oidc`/`saml` set `opensearch_security.auth.multiple_auth_enabled: true` with `auth.type: ["basicauth","<kind>"]` so the login page keeps a local fallback. A generated config where the IdP is the only path is a bug, asserted against in unit tests.
2. **Complete securityconfig.** Supplying `securityConfigSecret` **replaces** the operator's default securityconfig — the generated Secret must carry the full set (`config.yml`, `internal_users.yml`, `roles.yml`, `roles_mapping.yml`, `action_groups.yml`, `tenants.yml`) or the admin path breaks and the cluster hangs at "Security not initialized". Open question for the spike: whether the operator's update job tolerates a *partial* secret (only `config.yml` + `roles_mapping.yml`), which would let option C's aux CRDs own the role side and shrink our blast radius considerably.
3. **Origin gate.** `oidc` and `saml` require access mode `ingress` with a resolvable HTTPS host (ADR-027/#54). In `portforward` mode the app offers only `internal`/`ldap`/`jwt`/`proxy` and explains why.

**Secrets handling:** the bind password, `client_secret` and `exchange_key` are all credentials.
- The securityconfig files land in a **Secret** → fine, and provisioned through the ADR-034 model (External Secrets), never committed.
- **`spec.dashboards.additionalConfig` is rendered by the operator into a ConfigMap** — putting `opensearch_security.openid.client_secret` there stores an IdP credential in plaintext, readable by anything with configmap-get in the namespace. Candidates were: (a) OSD `${ENV_VAR}` expansion in `opensearch_dashboards.yml` fed by `spec.dashboards.env[].valueFrom.secretKeyRef`; (b) a Secret file mounted over the config path via `spec.dashboards.additionalVolumes`; (c) accept the ConfigMap and compensate with namespace RBAC. **Resolved in MR2 as (a)**: the generator emits `${VELOX_OIDC_CLIENT_SECRET}` and the CR wires that env var from the deployment's `<name>-auth` Secret; both `dashboards.env` (standard `EnvVar`, so `valueFrom.secretKeyRef` is available) and `dashboards.additionalVolumes` are confirmed present in the vendored CRD (`deploy/bootstrap/operator.yaml`). A unit test asserts no credential appears in the ConfigMap-bound map for any kind. ~~Still to confirm on a live cluster~~ — **CONFIRMED in MR4 against Keycloak on a live cluster: OSD 3.x does expand `${…}`.** A full browser login (authorization code → token exchange, which is the only step that uses the client secret) succeeded with the ConfigMap carrying nothing but the literal `${VELOX_OIDC_CLIENT_SECRET}`. Option (a) stands; the fallback to (b) is not needed.

**Three further things the OIDC path needs that rev. 2 did not account for — each of them fatal on its own, all found on a live cluster in MR4 (#56):**
1. **`pemtrustedcas_content` is inert without `enable_ssl: true`.** The security plugin builds the IdP HTTP client through its generic `SettingsBasedSSLConfigurator`, which only assembles a custom trust store when that flag is set. Without it the PEM is silently ignored, the node falls back to the JVM `cacerts`, the JWKS fetch dies with `unable to find valid certification path to requested target`, and every login is refused — while the generated securityconfig reads as perfectly correct.
2. **The IdP CA has a second consumer: Dashboards.** The ADR treated the CA as a cluster-side concern (the nodes validate the JWT against the JWKS). But the security-dashboards plugin is a Node process that fetches the discovery document *itself* at startup, over its own TLS stack, knowing only the system roots — behind a private CA it dies at boot with `UNABLE_TO_VERIFY_LEAF_SIGNATURE` / `Failed when trying to obtain the endpoints from your IdP`. The same PEM therefore has to reach the Dashboards container as a **file** (`spec.dashboards.additionalVolumes`, `restartPods: true`) and be named in `opensearch_security.openid.root_ca`.
3. **No `server.publicBaseUrl`.** OpenSearch Dashboards forked Kibana at 7.10 and its legacy config schema rejects unknown `server.*` keys outright: emitting that (a Kibana 7.11+ setting) kills the container at boot with `child "server" fails because ["publicBaseUrl" is not allowed]`. The redirect origin is carried by `opensearch_security.openid.base_redirect_url` alone.

**And two more the SAML path needed, both found the same way (live cluster, MR4, #56):**
1. **The XSRF allowlist named the wrong path family — SAML login was impossible.** The generator listed the three `/_plugins/_security/saml/*` paths, which is what the current documentation shows. But the security plugin still advertises the SP's AssertionConsumerService under the *legacy* prefix, so the IdP posts the assertion to `<kibana_url>/_opendistro/_security/saml/acs`. A browser POST carries no `osd-xsrf` header — exempting exactly these paths is the entire purpose of the allowlist — so Dashboards answered `400 Request must contain the osd-xsrf header` **after** the user had already authenticated at the IdP. Everything before that last hop passes: the probe resolves the metadata, the securityconfig is valid, Dashboards reaches Ready, the redirect to Keycloak works, the credentials are accepted. No assertion over generated YAML can see it; only a real browser round trip can. The generator now emits **both** families (`auth_provider.rs::SAML_XSRF_PATHS`), since Dashboards registers both and a later plugin release may switch.
2. **`SamlConfig` had no CA field at all** while `OidcConfig` did, though both fetch IdP metadata over https from the OpenSearch nodes. Every SAML IdP behind a corporate PKI — i.e. the normal ADFS deployment — was therefore impossible to configure, and the pre-save probe refused a configuration that would in fact have worked. Added, with the same `enable_ssl: true` companion that consequence 1 of the OIDC list describes; the probe uses it too. Note the asymmetry with OIDC: SAML needs **no** Dashboards-side CA, because the Dashboards SAML plugin never contacts the IdP itself — it delegates to the cluster and only the browser follows the redirect. `dashboards_ca_pem()` stays OIDC-only on purpose.

Unrelated to SAML but surfaced by it: `reqwest::Error` renders as `error sending request for url (…)` and hides the cause (`invalid peer certificate: UnknownIssuer`) one or two links down its `source()` chain, so the probe was reporting a category where UI rule 5 promises the exact upstream error. `auth_probe.rs::cause_chain` now walks the chain; this improves the OIDC and LDAP paths as well.

**Consequences / to settle in the spike (#56):**
- Keeping the existing `adminCredentialsSecret` admin working is a hard constraint (invariant 1).
- The updateJob triggers a rolling restart on apply/drift, and a change to `dashboards.additionalConfig` rolls the Dashboards Deployment; surface both as an "applying" state via `.status` (existing pattern). The ~30s reconcile loop reapplies, so manual edits to the Secret cause restarts.
- **Day-2:** changing or removing a provider is a supported operation, not a re-create. ~~removing it restores the operator default securityconfig~~ — **corrected in MR4 against a live cluster:** it does not. The operator's update job runs `securityadmin.sh -f <file> -t <type>` per file over the secret it mounts, and when we supply no `securityConfigSecret` that job rewrites `internal_users.yml` and *nothing else*. Deleting our Secret and clearing the CR field therefore leaves the last-pushed `config.yml` live in the security index: the app reports `internal` while the directory / IdP keeps authenticating into the cluster. **Once we have replaced a cluster's securityconfig we own it** — reverting means pushing an internal-only securityconfig (`internal_only_security_config_files`) and keeping `securityConfigSecret` pointed at it, never dropping ours. A `POST /api/deployments/:name/auth_provider/test` probe runs **before** any write: `ldap` = TCP+TLS dial, bind as `bind_dn`, sample user search returning the matched DN and its groups; `oidc` = fetch the discovery document, validate `issuer`/`authorization_endpoint`/`jwks_uri`; `saml` = fetch and parse the IdP `EntityDescriptor`. A failed probe never persists.
- **Invariant 1 covers authorization, not just authentication (MR4).** `roles_mapping.yml` must always carry `all_access` ← backend role `admin` + the local admin user, and `kibana_server` ← the Dashboards account. Emitting only the customer's group rows produced a cluster where the admin authenticated with **no roles at all**: `cluster:monitor/main` denied → node readiness probe failing → `RollingRestart` never completing, and Dashboards in `CrashLoopBackOff`. Worse, it is **not self-healing**: the operator's repair job waits on `curl https://<svc>:9200`, the Service only routes to *Ready* pods, and readiness is exactly what was lost — so the cluster deadlocks and needs a manual `securityadmin.sh` run against a pod IP to recover (see `docs/runbooks/`). A break-glass assertion that only checks "the admin can log in" does not catch this; it must assert the role and a call that needs it.
- **Multi-tenancy (ADR-041):** the provider config is per *deployment* and lives in the deployment's K8s Secret, not in Postgres — consistent with "the CR owns deployment shape, Postgres owns identity and org".
- Delivery order: **MR1** = this ADR + pure `src/auth_provider.rs` (kind enum, per-kind config structs, `security_config_yaml()`, `roles_mapping_yaml()`, `dashboards_additional_config()`, validators) + unit tests, no cluster; **MR2** = Secret + CR wiring in `k8s.rs` (both artifacts), `GET`/`PUT /api/deployments/:name/auth_provider`, the `/test` probe, `.status` surfacing; **MR3** = the screen below + i18n + `docs/auth/*.md`; **MR4** = `conformance/` integration against an ephemeral OpenLDAP and an ephemeral Keycloak. Branch from `develop` per the merge gate. MR4 is **delivered for `ldap`, `oidc` and `saml`** (`tests/auth_{ldap,oidc,saml}_check.py`), each driving a real browser round trip against the fixture in `conformance/auth/`; `jwt` and `proxy` stay unit-tested only, neither having a Dashboards login to exercise.

**UI — "Autenticação" screen** (deployment detail tab; also an optional, skippable step in the create wizard). Design rules from ADR-019 (minimalist, i18n pt/en, dark/light) and the primitives in `frontend/ui.jsx`:
1. **Choose the kind first, one screen deep.** A row of radio *cards* (not a `<select>`): `Interna (padrão)` · `LDAP / Active Directory` · `OpenID Connect` · `SAML 2.0`, each with an icon, a one-line "quando usar", and — for the kinds blocked by the origin gate — a disabled state whose reason is stated on the card, not hidden in a toast. Advanced kinds (`jwt`, `proxy`) sit behind "Outros métodos".
2. **Presets before fields.** Picking OIDC reveals chips `Keycloak · Entra ID · Okta · Google · Outro`, which prefill the discovery URL as an editable template (`https://<host>/realms/<realm>/.well-known/openid-configuration`). SAML gets `ADFS · Keycloak · Okta · Outro`. Same idea for LDAP: `Active Directory` prefills `(sAMAccountName={0})`, `OpenLDAP` prefills `(uid={0})`.
3. **Progressive disclosure.** Only required fields are visible; everything else lives in a collapsed **"Opções avançadas"** (custom CA PEM, `subject_key`/`roles_key`, nested-role resolution, extra scopes, StartTLS). The default path for Keycloak is four inputs: discovery URL, client id, client secret, and the group→role table.
4. **Credential fields are write-only.** Password type with the existing `eye`/`eyeOff` toggle, never echoed back by the API, cleared after a successful save — the exact pattern already used for TLS PEM material (`frontend/views_settings.jsx:39-42`). A saved secret renders as `••••••• (definido)` with a "Substituir" action.
5. **"Testar conexão" is a first-class button**, next to Save, wired to the `/test` probe. Result renders inline (not as a toast): green panel listing what was resolved — `bind OK · usuário exemplo: CN=... · 7 grupos encontrados`, or `issuer=https://kc/realms/velox · authorization_endpoint OK · jwks_uri OK`; red panel with the exact upstream error and the one field most likely at fault highlighted. Save stays enabled without a test but asks for confirmation.
6. **Group → role mapping as a small repeatable table.** Two columns (`Grupo do diretório / claim` → `Papel OpenSearch` as a select of `all_access`, `kibana_user`, `readall`, plus custom), add/remove row, one pre-filled row, and an empty-state line explaining that users authenticate but see nothing until at least one mapping exists.
7. **Lockout reassurance is permanent, not a footnote.** A standing info box: *"O administrador local (`<admin user>`) continua funcionando mesmo se o provedor ficar indisponível."*
8. **State the blast radius before writing.** Save opens a confirm dialog naming what happens: the securityconfig is reapplied and the Dashboards restarts, ~1–2 min, sessions dropped. After save, a status chip driven by `.status` — `pendente → aplicando → ativo → erro` — with the failure reason and a one-click **"Reverter para autenticação interna"**.
9. **Accessibility / i18n:** every input has a real `<label for>`, hints via `aria-describedby`, radio cards keyboard-navigable, validation errors rendered under the field (toast is a duplicate, never the only signal), all strings through `STR` in `i18n.jsx`, both themes.

**Tests:** pure assertions over the generated `config.yml` per kind (LDAP authc+authz domain shape and ldaps/StartTLS toggles; `openid` domain discovery URL and `roles_key`; `saml` IdP/SP entity ids and exchange key), over `roles_mapping.yml` (group/claim → role), and over the Dashboards block (`auth.type` array, XSRF allowlist for SAML, `cookie.secure`, redirect base derived from the deployment host). Invariant tests: the internal basic-auth domain and `all_access` admin survive every kind; `oidc`/`saml` are refused in `portforward` mode. Integration on a `conformance/` cluster against ephemeral OpenLDAP + Keycloak validating Dashboards login and group→role resolution (MR4 — unit + manual smoke first, given backlog priority).

## ADR-046 — Platform alerting is a standalone CronJob watchdog, not a PrometheusRule (post-incident 2026-08-04/05)

**Status:** ACCEPTED (2026-08-05). Closes the P1/P2 alerting action items of
`org/incidents/2026-08-04-vm3-longhorn-disk-full.md` and
`org/incidents/2026-08-05-registry-gc-dead-year.md`.

**Context.** Two production incidents in two days, both detected by a human
noticing a red pipeline hours late:

1. vm3's 148G root disk reached 100% (`/var/lib/longhorn/replicas` = 135G).
   All three nodes are etcd voters, so one full disk degraded the whole
   control plane and broke the `main` deploy. No alert existed.
2. The `registry-gc` CronJob had been dead ~1 year in `ImagePullBackOff`
   after the Bitnami catalog retirement. Nothing alerts on a CronJob that
   never runs, so a year of registry garbage accumulated and fed (1).

**The obvious answer was wrong.** The cluster already runs
kube-prometheus-stack (provisioned by the *sidecar* repo, not owned here), so
two `PrometheusRule` objects would have been the smallest diff. Measured
read-only on 2026-08-05, they would also have alerted precisely nobody:

- Alertmanager's only receiver is named `"null"` and the default route points
  at it. All 35 existing PrometheusRules already fire into it and are
  discarded — the stack has never notified anything.
- Prometheus, Alertmanager and Grafana had all been `0/1` for 21h, because
  their Longhorn volumes live **on the node that filled**.
- The single largest consumer of vm3's disk **is** the Prometheus PVC: 39G
  active replica (bloated by two ~21GB Longhorn snapshots) plus a 21G leaked
  replica directory — ~60G of the 135G.
- node-exporter is a DaemonSet, and its vm3 pod was evicted by the very disk
  pressure it exists to report.

The monitoring stack shares fate with the thing it must monitor, is a leading
cause of the failure it must report, and is wired to a null receiver.

**Decision.** Ship platform alerting as a self-contained CronJob
(`deploy/ci/50-platform-watch.yaml`) that depends on the apiserver and kubelet
only:

- Node disk comes from each kubelet's `/stats/summary` via `nodes/proxy`, not
  from node-exporter, metrics-server (which reports no disk at all) or
  Longhorn's node CRs (stale exactly when a node is full, because
  longhorn-manager gets evicted).
- Trend state is a small ConfigMap in etcd — never a Longhorn volume.
- The script is stdlib-only Python, embedded in a ConfigMap, so a threshold
  or logic fix is `kubectl apply` + next run: no image build, no registry
  push, no dependency on CI, which is itself one of the things being watched.

**Alerting thresholds are argued, not round.** Disk WARN 75% / CRIT 85% (the
critical tier deliberately *precedes* kubelet's 90% eviction, because these
nodes are etcd voters), plus a projected-time-to-full trend alert at 14 days
that fires regardless of absolute level — the 08-04 disk spent months filling
at a level nobody looked at. CronJob staleness is scaled per job (2x its own
period + 1h), so a weekly job is flagged in ~15 days rather than ~365.

**Consequences.**

- Observe-and-report only. It never patches, deletes, drains or restarts;
  relief stays operator-gated (`org/runbooks/node-disk-pressure.md`). An
  automation that "fixes" a full disk by deleting Longhorn replicas is the
  incident we are trying not to have.
- Nothing watches the watchdog, so it carries a dead-man's switch: if it has
  sent nothing for 24h it sends an explicit OK. Silence past that window is
  actionable rather than reassuring.
- `startingDeadlineSeconds` is set because >100 missed schedules makes the
  CronJob controller stop scheduling *permanently* — a watchdog that quietly
  dies after a long outage is worse than none.
- **The channel is still an open decision.** There is no watched alert
  channel on this estate (`org/infra.md`, `org/peculiarities.md`); the
  the status page has a documented false-positive *and* false-negative
  history and is not read. The notify Secret is therefore a template
  (`50-platform-watch.secret.yaml.example`) with `optional: true` mounting:
  the checks run and log regardless. The only channel with evidence of a human
  reading it is the "Equipe Tornis" WhatsApp group via the self-hosted
  Evolution API, which is how profana pages today; the watchdog speaks that
  protocol natively and a generic JSON webhook as the alternative.
- This does **not** replace kube-prometheus-stack, and it is not an argument
  against ever using it. If the stack is given a real receiver and its storage
  is moved off the disk it monitors, migrating these checks to PrometheusRules
  is a reasonable follow-up — but a monitor must not share fate with its
  subject, so the standalone path stays.

**Also settled here (Longhorn headroom, P2).** Live settings had been loosened
by hand and undocumented: over-provisioning 200 (upstream 100) and
minimal-available 10 (upstream 25), leaving the fleet scheduled to 99.3% of
its own ceiling. The 200% patch was literally an instruction in
`deploy/ci/README.md`; it has been removed and replaced with the reason not to
re-add it. Safe values are now explicit in `deploy/bootstrap/longhorn.yaml`'s
`longhorn-default-setting` — the one deliberate divergence from that vendored
bundle — so new installs (including customers') are safe by default.
Re-tightening the *existing* tornis cluster is operator-gated and sequenced in
`deploy/bootstrap/longhorn-settings.tornis.yaml.example`: lowering the ceiling
while 205 GiB is scheduled would block the replica rebuilds recovery depends
on, so vm3 must get headroom **first**.

## ADR-047 — Registry-down is a state, not a failure: stale-tolerant catalog cache + a one-integration embedded bootstrap floor (resolves ADR-039's open question)

**Decision:** Resolve the two questions ADR-039 left open in its *"Consequences / to settle in the child issues"* — *"the failure mode when the registry is unreachable"* and *"whether the core embeds a minimal built-in catalog as a bootstrap floor"* — as follows, implemented by the runtime catalog client in `src/catalog.rs` (#75):

1. **The catalog read never fails.** `POST /api/catalog` always answers 200 with `{ integrations, source, age_seconds, stale, error }`. `source` is one of `registry` (fetched now, or cached inside a 15-minute TTL), `cache` (the registry could not be reached, so the **last good catalog is served, marked `stale`**, with the reason verbatim in `error`), or `bootstrap` (the registry has never been reached on this instance). A down, slow, or *unauthorized* registry degrades the Integrations tab to "what we last knew, plus why we can't refresh" — it never 5xx's, and it never touches the deployment screen. Registry reads carry a 10s timeout so a hung endpoint cannot hold a UI request open.
2. **The core embeds a bootstrap catalog of exactly one integration: `kubernetes`.** Not the twelve. `kubernetes` is the baseline monitor ADR-018 turns on for every deployment anyway, so a brand-new, **egress-less** install is not an empty shelf — the cluster can still monitor itself — while Rodrigo's constraint that integrations *"não vir já de antemão"* stays honest: nothing else is pre-loaded. When the registry cannot be reached during an install of a bootstrap id, the in-binary recipe is applied instead (it is byte-equivalent to the registry package, enforced by the `registry_golden` gates); any other id is a clear, actionable error rather than a silent no-op.
3. **A reachable registry always wins.** A registry row for `kubernetes` replaces the embedded entry outright, so the floor can never pin an old version once the network is back.
4. **Transport is configurable, and `file://` is a first-class route.** `VELOX_REGISTRY_URL` (default: the registry repo's raw path on `main`) + `VELOX_REGISTRY_TOKEN` for a private registry. `file:///srv/veloxsearch-registry` reads a local checkout with the **same parser and the same signature check** — this is ADR-039's transport-agnostic promise made real for the air-gapped buyer, at the cost of one `if`.
5. **Trust does not degrade with the network.** The ed25519 keyring is compiled into the binary (`keys/velox-registry-2026.pub`, vendored from the registry, gated by a test that it has not drifted). Verification is offline by construction, so "the registry was unreachable" can never become "we installed something unverified". The one path that installs without a signature check is the bootstrap fallback — and that applies *core bytes that shipped through the core's own review and release gate*, not registry bytes.

**Date:** 2026-08-05 · **Epic:** #70 · **Card:** #75 · **Resolves:** ADR-039's open questions · **Implements:** `spec/signing.md` §3 verification path

**Options considered for the failure mode:**
- **(A) Hard-fail: registry unreachable ⇒ the Integrations tab errors.** Simple and loud, but it makes a third-party network dependency a *product* dependency: an air-gapped buyer — the profile ADR-039 explicitly commits to — sees a permanently broken tab, and a transient GitLab blip becomes a support call. Rejected.
- **(B) Empty catalog on failure.** No error path to write, but it is a lie by omission: "no integrations exist" and "we cannot reach the registry" render identically, and a user with `nginx` already installed watches it vanish from the list. Rejected — the UI cannot tell the operator what to fix.
- **(C) Stale-tolerant cache + embedded floor + an explicit `stale`/`error` on the wire — CHOSEN.** The three states are distinguishable by the client, the shelf keeps everything we ever knew about, and the one integration the product cannot function without is always installable. Costs a cache and a merge rule.

**Options considered for the bootstrap floor:**
- **(A) No floor.** Purest reading of "integrations must not ship pre-loaded", but a first install with no egress can then monitor *nothing* — the wizard's own always-on K3S monitoring (ADR-018) would depend on the internet. Rejected.
- **(B) Embed all twelve.** They are all still compiled in today, so it is free — and exactly the coupling epic #70 exists to remove. It would also make the registry cosmetic until the in-binary catalog is deleted. Rejected.
- **(C) Embed exactly the baseline (`kubernetes`) — CHOSEN.** One entry, matching the one integration ADR-018 already turns on unconditionally. `BOOTSTRAP_IDS` is a one-line const, and a test asserts every id in it is genuinely applicable from the in-binary catalog, so the floor cannot advertise something the core cannot install.

**Rationale:** The registry is infrastructure someone else operates, so its availability must not be load-bearing for a screen the customer uses to run their cluster. Distinguishing *fresh / stale / never-reached* on the wire is what lets the UI be honest at zero cost to the happy path, and it is strictly more information than either failing or emptying. Embedding exactly the baseline keeps the "empty shelf" property that made #70 worth doing while removing the one absurd outcome (a monitoring product that cannot monitor itself without internet access). Keeping the keyring compiled in means degradation is *availability-only*: fewer packages are visible, never less-verified ones.

**Consequences:**
- Installed state grows a sibling annotation, `veloxsearch.ai/integration-versions` (`id=version,…`), next to the existing `veloxsearch.ai/monitors`. A sibling rather than a new encoding *inside* `monitors` so every existing reader of that annotation — status list, wizard, frontend — keeps parsing exactly what it always did. Disabling a monitor drops its version with it, so nothing is ever reported installed at a version that is not.
- `min_core_version` becomes enforced, not decorative: install refuses a package whose floor this core does not meet, and the catalog view marks it incompatible. The registry's twelve packages declare `0.7.0` ("the first core release that ships the fixed apply-engine"), so **#75 ships that release**: the crate and the image tag move 0.6.2 → 0.7.0 in lock-step, and a test asserts this core satisfies every shipped package's floor.
- The bootstrap install path depends on the in-binary recipe catalog, which #70 will eventually delete. When it goes, `BOOTSTRAP_IDS` must either ship the `kubernetes` package's assets in-binary or shrink to nothing — the test tying `BOOTSTRAP_IDS` to `recipes::RECIPES` is what will fail loudly at that point.
- Key custody remains the open operator decision `spec/signing.md` §4 names; this ADR only pins the *verification* side.

**Tests:** every one of the twelve shipped packages verifies against the compiled-in key; a one-byte edit to any asset and any edit to a signed manifest field are rejected as tampered; a missing signature block and the #74 staging placeholder are rejected as unsigned; an unrecognized `key_id` is rejected before any crypto runs; a manifest re-serialized through the YAML parser (key order destroyed) still verifies, proving the digest is over canonicalized content and not file bytes; an unreachable registry with an empty cache yields the bootstrap floor and with a warm cache yields the stale catalog, both flagged; a fresh cache is served without refetching; ids and manifest-supplied asset filenames containing path separators are refused before a URL is built.
## ADR-048 — Deployment version upgrade as a first-class, one-way, pre-flighted operation (operator rolling upgrade)

**Status:** **IMPLEMENTED** (2026-08-06) — branch `feat/upgrade-and-ui-rich`, commits `77369de` (backend, docs, conformance) and `7d09f2b` (UI). All five children delivered: #110 version preservation, #111 `src/upgrade.rs`, #112 `upgrade_cluster` + `POST /api/upgrade_options` / `POST /api/upgrade_cluster`, #113 the UI (version everywhere, modal with blast radius, live progress), #114 conformance (`tests/upgrade_check.py` + the stuck-upgrade runbook). Plus rev. 2 (hourly upstream check, "Upgrade vX" tag, version choice at creation).

**Verified / not verified.** A real rolling upgrade ran end to end on the local single-node k3s (`teste-safq` 3.7.0 → 3.8.0, PVC-backed on `local-path`): three sequential node restarts, `status.version` reaching the target, the Dashboards phase patched automatically, cluster green afterwards with every index intact, and 9 pre-flight refusals rejected against the live cluster with the CR untouched. **Not** yet exercised: the same run on ct1/Longhorn, the accepted-save preservation path (the local deployment predates ADR-043, so its every save is refused by the operator's storageClass webhook — the refused-save case IS covered), and any upgrade of a deployment created through the wizard's new version picker.
**Date:** 2026-08-05 · **Epic:** #109 (children: #110 preserve-version fix, #111 MR1, #112 MR2, #113 MR3, #114 MR4) · **Builds on:** ADR-040 (3.7.0 deploy target), ADR-035 (memory is the tuning knob / day-2 guards), ADR-031+ADR-043 (PVC-backed Longhorn storage), ADR-027 (access modes), ADR-042/#83 (snapshots) · **Composes with:** ADR-045 (securityconfig ownership), ADR-019 (UI rules)

**Decision:** Upgrading a deployment's OpenSearch version becomes an explicit, named day-2 operation — **`POST /api/upgrade_cluster`** driven by an **"Atualizar versão"** button on the deployment detail screen — implemented on the operator's native mechanism: **patch `spec.general.version` and let the operator roll the nodes one by one, waiting for green between each**, then patch `spec.dashboards.version` in a second phase once the data plane is done. The set of offered targets is a **pinned catalog in the binary** (`src/upgrade.rs`), not free text and not a live registry listing; every target passes a **pre-flight** (semver-valid, strictly greater than the running version, at most one major ahead, image tags resolvable, cluster green and not mid-reconcile) **before** anything is written. The version stops being a hardcoded literal: `k8s.rs` gains `DEFAULT_VERSION` for *creation only*, and every other write path **preserves the running version**.

**Context:** Today `src/k8s.rs:728` and `:763` hardcode `"3.7.0"` into `general.version` and `dashboards.version`, and `create_cluster()` is the same function that `POST /api/save_cluster` calls to apply an **existing** deployment's edits (`src/api.rs:795-805`). Two consequences follow, one of them a live bug:
1. There is **no way to upgrade** a deployment from the app — the wizard's whole day-2 surface (nodes, memory, disk, purpose, monitors, auth) has no version axis, and `Status` (`src/k8s.rs:1046`) does not even carry the version, so the UI cannot show what a deployment runs.
2. **A save silently upgrades.** Any deployment created before ADR-040 runs 3.0.0; the moment an operator touches *anything* in the Edit tab — memory, disk, a monitor checkbox — the applied manifest carries `version: "3.7.0"` and the operator starts a rolling major-jump upgrade **that nobody asked for and that cannot be undone** (the upgrade reconciler rejects downgrades). This is the strongest argument that the version must become an explicit, consented field rather than a literal in the CR builder.

**How the operator behaves (verified against the docs and the operator's `pkg/reconcilers/upgrade.go`, 2026-08-05):**
- Trigger is the field itself: change `spec.general.version` → the operator "performs a rolling upgrade, restarting nodes one by one and waiting after each node for the cluster to stabilize and reach a green status."
- Order is **data-only pools → data+master pools → non-data pools**, resuming an in-progress pool before starting the next. Our deployments are one pool of 3 data+master nodes (ADR-016), so it is one pool, three sequential restarts.
- Progress is reported in `status.componentsStatus[]` as `component: "Upgrader"` with `status` in `Pending` / `Upgrading` / `Finished` / `Upgraded` and `description` = the node-pool name; `status.version` carries the version the operator considers running. Both fields are already in the vendored CRD (`deploy/bootstrap/operator.yaml:7065`, `:7093`) — no operator change, no new RBAC.
- It **refuses** a downgrade (`"version requested is downgrade"`) and a jump of more than one major (`"version request is more than 1 major version ahead"`), returning a terminal error that leaves other reconcilers running — i.e. a bad version does **not** block the operator, it just never upgrades, silently, unless we surface it.
- `general.drainDataNodes: true` is the documented safety for `emptyDir` storage. Our deployments are PVC-backed (ADR-031/043), so data survives a pod restart and we **leave it off** — turning it on would evacuate and re-replicate shards three times for no durability gain. Stated here so the omission reads as a decision, not an oversight.

**The one-way door (the property that shapes everything else):** because the operator rejects downgrades, there is **no rollback**. The recovery path for a bad upgrade is restore-from-snapshot into a fresh deployment (ADR-042 / #83), not a version revert. Everything below — the pinned catalog, the pre-flight, the confirm dialog, the snapshot nudge — exists because the operation cannot be taken back.

**Options:**
- **(A) Patch `spec.general.version`, pinned catalog, pre-flight, two-phase with Dashboards — RECOMMENDED (this).** Operator-native, zero new infrastructure, the CR stays the source of truth for deployment shape (ADR-041). Cost is the catalog: shipping a new target version means shipping a release. That cost is the point — a target in the catalog is a target we tested.
- **(B) Free-text version field.** Cheapest to build, worst to own: a typo (`3.7`, `3.70`, a yanked tag) produces an unpullable image on the first restarted node, the pod sits in `ImagePullBackOff`, the pool never goes green, the upgrade never advances — and the field cannot be reverted through the operator. Rejected as a *default*; it survives as a hidden advanced override (see invariant 5).
- **(C) Query the registry at runtime for available tags.** Discovers versions we never tested, breaks in air-gapped installs (the ADR-039 self-contained DNA), and offers no signal about *our* compatibility — a tag existing on Docker Hub says nothing about our recipes, profiles or securityconfig. Rejected as the source of the list; a **tag-existence check** of the already-chosen target is kept as a pre-flight step, degrading to a warning when the registry is unreachable.
- **(D) Blue/green: provision a new deployment at the new version, restore a snapshot, cut the DNS over.** The genuinely reversible answer, and the right one eventually for paid tiers, but it doubles cluster capacity for the duration on a finite 3-node K3S (the resource #84 exists to protect), needs #83 merged first, and changes the deployment's identity/subdomain. Not for the MVP; explicitly not foreclosed by A.
- **(E) Delete + recreate.** Data loss. Rejected.

**Rationale:** A is the mechanism the operator already implements — we are choosing *what to write and when*, not building an upgrade engine. The pinned catalog matches the pre-baked-recipe DNA (ADR-015): the app offers a short list of tested targets, not a raw version box, exactly as the auth screen offers IdP presets rather than a YAML editor. The two-phase node-then-Dashboards write follows OpenSearch's own guidance (the data plane leads, Dashboards follows) and avoids a window where a newer Dashboards talks to older nodes. And the pre-flight is not optional politeness: given no rollback, validation before the write is the *only* place a mistake can still be cheap.

**What gets built:**
- **`src/upgrade.rs` (pure, unit-tested, no cluster):** `DEFAULT_VERSION` (the creation version, today `3.7.0` — the single home of the literal that ADR-040 last moved by hand); `CATALOG`, an ordered list of tested targets with a note per entry (`3.7.0` current, `3.6.0` first 3.x LTS as the documented fallback); `targets_for(current) -> Vec<Target>` filtering the catalog by the operator's own rules (strictly greater, ≤ 1 major ahead) so the UI never offers a target the operator will reject; `validate(current, requested) -> Result<()>` returning the *same* refusals in our words; and `upgrade_state(spec_version, status_version, components_status) -> UpgradeState` — the pure reduction of `componentsStatus[]` + the two version fields into `idle | pending | upgrading{pool, from, to} | finished | failed{reason}`.
- **`src/k8s.rs`:** `Status` gains `version` (running, from `status.version`, falling back to `spec.general.version`), `target_version` (spec, when it differs) and `upgrade` (the state above). `create_cluster()` stops hardcoding: on an existing CR it **reads the current `general.version`/`dashboards.version` and re-applies them verbatim**; only a genuinely new CR gets `DEFAULT_VERSION`. New `upgrade_cluster(name, target)`: pre-flight → patch `general.version` → spawn a background task that waits for the pool to finish and the cluster to be green (`wait_green`, the existing pattern from `save_cluster`) → patch `dashboards.version` → re-assert the purpose profile is intact.
- **`src/api.rs`:** `GET /api/upgrade_options` (per deployment: running version, catalog targets with notes, and the blocking reasons if any) and `POST /api/upgrade_cluster`. Progress reaches the UI over the existing `/api/events` SSE stream, no new transport.

**Invariants (asserted in tests, not conventions):**
1. **No implicit version change, ever.** Every write path other than `upgrade_cluster` preserves the version already on the CR. A test applies an edit to a CR pinned at an older version and asserts the rendered manifest still carries that version — this is the regression test for the live bug above.
2. **Pre-flight before write, always.** Refuse unless: target ∈ catalog (or the advanced override is explicitly used), `target > current`, same or one major ahead, `health == "green"`, `phase` is running, and no `Upgrader` component is already non-terminal. A failed pre-flight never patches the CR.
3. **Both images resolve before either is written.** `opensearchproject/opensearch:<v>` **and** `opensearchproject/opensearch-dashboards:<v>` are checked (anonymous registry manifest HEAD) before the first patch — the two-phase write must not strand a cluster with upgraded nodes and an unpullable Dashboards. Registry unreachable (air-gapped) degrades to an explicit "não foi possível verificar as imagens" warning the operator must confirm, never to a silent pass.
4. **The operator's refusals surface as ours.** A terminal upgrade validation error in the operator is invisible today; `upgrade_state` maps it onto `failed{reason}` and the UI shows the exact upstream string (ADR-045 UI rule 5, same discipline as `auth_probe.rs::cause_chain`). **Correction found while building (2026-08-06):** a refusal is **not** written into `componentsStatus` at all — `upgrade.go` emits a `Warning`/`Upgrade` **Event** and returns `AsTerminal(err)`, leaving `status` untouched. So `upgrade_state` takes the latest such event message as a fourth input (`k8s.rs::last_upgrade_warning`, only read while spec and running version disagree, so a stale event can't paint a healthy deployment red). The `events` verb was already in the runtime ClusterRole — still no RBAC change.
5. **The advanced override is a deliberate act.** A free-text target lives behind "Opções avançadas", is still subject to invariants 2–3, and its confirm dialog states plainly that the version is untested by us and irreversible.
6. **Upgrade does not touch shape.** No node/memory/disk/purpose/auth field is rewritten by an upgrade; the securityconfig we own (ADR-045) is re-asserted, not regenerated.

**UI — "Atualizar versão"** (deployment detail, Overview tab; the Edit tab keeps *no* version field, by invariant 1):
1. **The running version is always visible**, as a chip next to health: `OpenSearch 3.7.0`. When a newer catalog target exists, the chip gains a subdued "atualização disponível" affordance and an **"Atualizar versão"** button — the button is never a bare icon and never hidden in a menu.
2. **A single modal, three facts, one choice.** Target select (catalog entries with their note — `3.7.0 · atual` / `3.6.0 · LTS`), then the blast radius stated before the action, not after: *os nós reiniciam um a um, o cluster segue atendendo, leva ~5–15 min, e **não há rollback**.*
3. **The snapshot line is not fine print.** While #83 is unbuilt: a warning that there is no automatic backup and the recovery path is a manual snapshot. Once #83 lands, the modal grows a "criar snapshot antes de atualizar" checkbox, on by default.
4. **Blocked states explain themselves on the spot** (ADR-045 UI rule 1): cluster not green, upgrade already running, no catalog target ahead — the reason renders in the modal, not in a toast.
5. **Progress is a live state, not a spinner.** `pendente → atualizando (nó 2 de 3) → concluído / erro`, driven by `componentsStatus` over SSE, with the failed reason verbatim and the runbook linked on error.
6. **i18n pt/en through `STR`, both themes, keyboard-navigable modal, error under the control** — the standing ADR-019/ADR-045 rules.

**Consequences / open questions for the build:**
- **Duration and the SSE window:** three sequential node restarts each waiting for green is minutes, not seconds; the state must survive a page reload (it lives on the CR, so it does) and a backend restart (likewise — no in-memory upgrade bookkeeping is allowed).
- **Dashboards phase failure:** if phase 2 fails, nodes are already upgraded and the deployment is in a supported-but-mixed state (a Dashboards one minor behind its cluster works). Surface it as `failed` with a retry that re-runs phase 2 only.
- **The `veloxsearch-test` legacy 3.0.0 cluster (ADR-044's grandfathered namespace)** is the first real customer of this feature and the natural manual smoke test: 3.0.0 → 3.7.0 is one major, allowed.
- **Bumping `DEFAULT_VERSION` is now a two-line release chore** (constant + catalog entry) instead of ADR-040's four-file sweep; `deploy/opensearch-test-cluster.yaml` and `src/recipes.rs`' panel version stay manual and are called out in the release checklist.
- **A new runbook** `docs/runbooks/` for a stuck upgrade: `ImagePullBackOff` on the rolling pod, the operator's terminal validation error, and the manual `securityadmin`-style recovery when a pool is half-upgraded.

**Rev. 2 (2026-08-06) — hourly upstream check + "Upgrade v3.8.0" tag.** Rodrigo asked for the app to notice new OpenSearch releases by itself: a tag on the deployment naming the new version, which starts the upgrade when clicked. This **amends option C's rejection**, and only in one direction: the registry becomes a source of *suggestions*, never of the rules. `src/version_feed.rs` polls Docker Hub every hour (`VELOX_VERSION_CHECK_SECS`, `0` = off for air-gapped installs), keeps the newest **stable** `MAJOR.MINOR.PATCH` in memory (never a `-beta`/`-rc`), and publishes it only if the **Dashboards image carries the same tag** — one image without the other would strand the two-phase upgrade. `Status.suggested_version` is that version filtered per deployment through `upgrade::validate`, so a deployment is only ever told about an upgrade the operator would actually accept. Clicking the tag opens the same modal with the target preselected — the irreversible step still needs its confirmation. A suggested version is accepted by the pre-flight without the "untested" override (the user is clicking what *we* showed them), is labelled `latest · nova — detectada no registry, ainda não testada por nós`, and passes every other gate unchanged. Nothing is persisted: a restart just re-checks. The catalog stays the tested list and still leads for anything at or below `DEFAULT_VERSION`.

**Rev. 2b — the version is also chosen at creation.** The wizard's Propósito step now carries a version select right after the name: the three newest versions the hourly check confirmed (both images published), or the pinned catalog offline, plus a free-text "outra versão". `CreateOverrides.version` is honored **only when the CR does not exist** — `save_cluster` never sends it, so invariant 1 is untouched and a live deployment's version still moves solely through `upgrade_cluster`. Creation validates differently from an upgrade, on purpose: there is no "current" to compare against, so the only refusals are a malformed tag and images that are not published (a create that fails to pull is deletable; an upgrade is not). That check runs with the other day-2 guards, **before** `ensure_admin_secret` — it sat after it at first and a refused create left an orphan credentials Secret behind (found and fixed the same day).

**Live conformance (2026-08-06, local k3s, `teste-safq` 3.7.0 → 3.8.0) — PASSED.** The feed found 3.8.0 upstream, the tag offered it, and the upgrade ran end to end in ~15 minutes: the three nodes rolled **one at a time** (1 → 2 → 3 pods on the new image, the operator waiting for the pool to settle between each), `status.version` reached 3.8.0, and **phase 2 patched `spec.dashboards.version` on its own**. Afterwards: cluster green, 43 primaries / 91 shards active, every index intact (July audit logs still readable), `upgrade_options` reporting "3.8.0 is the newest version we know of". 3.8.0 was then added to `CATALOG` — that is exactly how the list is meant to grow: a target is in it because we upgraded to it. `DEFAULT_VERSION` stays 3.7.0 (what a *new* deployment is created with is ADR-040's call, not this one's). Two findings the design had wrong:
1. **A rolling upgrade is not health-neutral.** The cluster reports **red** while the node holding an unreplicated primary shard is down — the conformance assertion "no red interval" was wrong (it asserted a property the operator's own one-node-at-a-time design cannot provide), and the modal's "o cluster segue atendendo" was optimistic. Both corrected: the test records the health states seen and requires green *at the end* with the data readable, and the blast-radius text now says replicated indices stay available while unreplicated ones do not.
2. **A save can be refused for reasons unrelated to the version.** `teste-safq` predates ADR-043, so its node pool has no `storageClass` and the operator's webhook refuses the `'' → longhorn` change on any save. The #110 invariant still holds there (a refused save writes nothing), but the accepted-save path is not exercisable on such a deployment — the script now says so explicitly instead of passing quietly.

Also corrected while testing: `ApiError::internal` formatted errors with `{e}`, dropping the context chain, so a 500 read `applying OpenSearchCluster CR` with no cause. It now uses `{e:#}` and the webhook's actual refusal reaches the UI.

**Tests:** unit — `targets_for()` never offers a downgrade or a two-major jump (property-tested over a version grid); `validate()` refuses the same set the operator refuses, with our wording; `upgrade_state()` reduces representative `componentsStatus` payloads (empty, `Pending`, `Upgrading` mid-pool, `Finished`, terminal error) correctly; **the preservation regression** — applying an edit to a CR at `3.0.0` re-renders `3.0.0`. Integration (conformance ct1, PVC-backed): provision at the catalog's `n-1` target, call `upgrade_cluster` to the current target, assert three sequential restarts with no red interval, `status.version` reaching the target, Dashboards upgraded in phase 2, and an index written before the upgrade still readable after — plus a negative case (a bogus target) that is refused by the pre-flight with the CR unmodified.

## ADR-049 — Snapshot repository is a generic S3 target the user owns, written into the CR as an orthogonal slice; a default scheduled policy comes with it

**Status:** **IMPLEMENTED** (2026-08-07) — the configuration half of the backup pillar, proven end to end on local k3s against MinIO. On-demand snapshot, snapshot listing and restore are deliberately **not** here (see *Out of scope*).

**Verified / not verified.** **Verified live** (local k3s, MinIO fixture, deployments `teste-uu9d` and `versaotest-eamz` on OpenSearch 3.8.0): the repository registers, `_verify` passes on all three nodes, a **scheduled** snapshot ran to `SUCCESS` across 14 shards and its objects landed in the bucket (36 MiB / 420 objects), the first configuration rolls the nodes and the cluster comes back green, a policy-only edit restarts nothing and round-trips, and all seven pre-flight refusals leave the CR untouched. `tests/snapshot_check.py --configure` reports 17 passing checks. **Not** verified: restore (out of scope by design); behaviour against real AWS S3 — only MinIO was exercised, so the `bucket_default` encryption choice is proven not to *break* AWS but has not been run there; the wizard's create-time path (both deployments were configured day-2, so the "configure at creation, before the pods exist, therefore no restart" claim is reasoned, not observed); and anything on ct1/Longhorn or the production cluster.

**Live conformance (2026-08-07, local k3s + MinIO) — PASSED, after two real defects it found.**
1. **Server-side encryption, the one that would have shipped broken.** Every snapshot failed with MinIO's **501 `Server side encryption specified but KMS is not configured`**. `repository-s3` requests encryption even when nothing asks it to, and MinIO — which implements SSE-S3 through its own KMS — rejects `AES256` exactly as it rejects `aws:kms`. Only the third accepted value, `bucket_default`, sends no directive at all. The renderer now pins it. Nothing in the unit tests could have caught this: the manifest was *valid*, the operator accepted it, the CR looked right, and the failure only exists on the far side of an S3 request. This is the argument for the fixture existing.
2. **A failed snapshot policy does not self-heal.** After the repository was fixed, the SLM policy stayed in `CREATION_START` / `latest_execution: FAILED` with a null `end_time` and never fired again — the SM plugin's runtime metadata is stuck independently of the policy document, so re-saving the same policy changes nothing. Disabling and re-enabling the policy (which deletes and recreates the CR, and with it the SLM policy) clears it, and snapshots started immediately. Recorded in the runbook; a self-heal is not worth building, but a user who fixes their bucket and sees nothing happen needs to be told this.

Also corrected in the harness: `tests/snapshot_check.py` waited for a single `health == green` reading after the restart, which returns *between* two node restarts while the roll still has nodes to go — it then verified against a cluster the operator had not finished with and read `repository_missing` for four minutes. It now requires every node ready and green on three consecutive samples, and gives registration its reconcile window before failing.
**Date:** 2026-08-07 · **Epic:** #83 (backup/restore, the anchor pillar) — this is its first child · **Builds on:** ADR-042 (S3 repo decision), ADR-048 (day-2 operation shape: pure module → pre-flight → write), ADR-045 (per-deployment slice under its own field manager, write-only secrets), ADR-031/043 (PVC-backed Longhorn storage) · **Amends:** ADR-042, partially (MinIO in-cluster stops being a prerequisite) · **Unblocks:** #83's snapshot/restore routes

**Decision:** A deployment's snapshot repository is a **generic S3-compatible target the user supplies** — bucket, endpoint, region, path-style flag, access key, secret key — serving AWS S3, MinIO, Wasabi or anything speaking the S3 API through **one** `type: s3` code path. It is written **through the operator's own CRs**, never through ad-hoc REST calls: the repository as an entry in `spec.general.snapshotRepositories`, its credentials as a per-deployment Secret loaded into the OpenSearch keystore via `spec.general.keystore[].keyMappings`, and the schedule as an `OpensearchSnapshotPolicy` CR in the deployment's namespace. Both the CR slice and the policy CR are **optional everywhere**: a deployment with no snapshot configuration is the default and a fully valid state. The configuration is offered as a **skippable step in the create wizard** and is **editable afterwards** in a Backup tab on the deployment screen — the same field on the same contract, reached from two places.

**Context:** The product has no configurable backup. The only snapshot code is `src/profiles.rs:28-119`: a `SNAPSHOT_REPO = "velox-snapshots"` constant registered best-effort as `{"type":"fs","settings":{"location":…}}` inside `ensure_retention()`, feeding the ISM `hot → snapshot → delete` policy of the Observability profile. That `fs` premise was already struck down by ADR-042 — an `fs` repo needs a `path.repo` reachable by every node of every tenant's cluster, which on a namespace-per-tenant SaaS is a cross-tenant read channel — but nothing replaced it: there is no S3 code, no keystore code, no MinIO, no API surface. So today the ISM policy points at a repository that either does not exist or, where it does, is the wrong shape. The cost of that gap is stated in ADR-048 itself: a version upgrade has **no rollback**, the recovery path is restore-from-snapshot, and the upgrade modal has to admit in writing that no automatic backup exists (`i18n.jsx: upg_snapshot_p`). Backup is also the anchor pillar of the roadmap (#83) — the central trust story of the ECE-class experience we are modelling.

**How the operator behaves (verified against the operator docs and `pkg/reconcilers/`, 2026-08-07):**
- `spec.general.snapshotRepositories[]` is `{name, type, settings}` where **`settings` is `map[string]string`** — every value must be a string. `path_style_access: true` is rejected by the API server; `"true"` is accepted. This is a real, silent-looking failure mode, so the renderer stringifies everything by construction.
- The repository reconciler **only runs once the cluster is `PhaseRunning`**, requeueing every 10s until then. On a fresh deployment the repository is therefore *pending*, not *failed*, for the first minutes — the UI must say so.
- **A repository is never deleted.** `SnapshotRepositoryReconciler.Delete()` is a no-op by design ("only called if the entire cluster is deleted"). Removing an entry from the spec **orphans** the registration inside OpenSearch; it does not deregister it.
- `OpensearchSnapshotPolicy` maps 1:1 onto OpenSearch's SLM structure (`creation.schedule.cron{expression,timezone}`, `deletion.deleteCondition{maxAge,maxCount,minCount}`, `snapshotConfig{repository,indices,includeGlobalState,…}`). **`policyName` is `required` in the CRD schema even though the docs call it optional** — we always set it explicitly rather than relying on the reconciler's `metadata.name` fallback, which the API server never lets us reach.
- **Pre-existing policies are not touched**: a policy the operator did not create is marked `status.state: IGNORED` and is not deleted with the CR. States are `PENDING | CREATED | ERROR | IGNORED`, with `status.reason` carrying the upstream message — that string is what the UI shows, verbatim (ADR-045 UI rule 5).
- Credentials **cannot** go in the repository body: the `repository-s3` plugin reads them from the keystore. The operator populates the keystore **at pod start** and there is no reload hook, so a credential that changes only takes effect after the pods restart.

**The gradient that shapes the UI — not every change costs the same:**
1. Changing **only the policy** (cron, retention, indices) → a patch to one CR, **no restart**, effective immediately.
2. Changing **only the repository body** (bucket, base path, endpoint) → the reconciler re-registers against the live cluster, **no restart**.
3. Changing **the credentials** — first configuration, key rotation, or turning snapshots off → the keystore changes, and **the nodes roll one at a time**. Unreplicated indices are unavailable while the node holding them is down (the correction ADR-048's live conformance forced on us: a rolling restart is *not* health-neutral). This is the only case that gets a confirmation.

The server decides which case a save falls into and tells the UI; the UI never computes it. This is the same discipline as `upgrade_options`' `blocked_reason`.

**Options:**
- **(A) Register the repository and the SLM policy over the OpenSearch REST API**, the way `ensure_retention()` does today. Cheapest, no RBAC change, no restart. But credentials still have to reach the keystore, which is a pod-spec concern the REST API cannot touch — so the one thing that actually needs the CR is the one thing REST cannot do, and we would end up with the configuration split across two mechanisms with two failure modes and no single source of truth. Rejected as the primary path.
- **(B) Operator CRs for everything — RECOMMENDED (this).** Declarative, reconciled, survives a backend restart with no in-memory bookkeeping (the ADR-048 rule), and the CR stays the source of truth for deployment shape (ADR-041). The repository, its credentials and its schedule are one consistent object graph. Costs: one new RBAC rule (`opensearchsnapshotpolicies`), and the credential path implies a rolling restart.
- **(C) Hybrid — repository + keystore by CR, policy by REST `_plugins/_sm`.** Avoids the CRD/RBAC addition for the policy and edits schedules without any operator round-trip. Rejected because it re-introduces the split of (A) for no gain: the policy is the *cheap* half either way, and a policy living outside the CR is a policy nothing reconciles back after a manual change.
- **(D) Ship in-cluster MinIO first as platform infra** (ADR-042's option B in full: shared MinIO, bucket per tenant, scoped key minted by us). The eventual multi-tenant answer, but it front-loads a whole storage component, a bucket-provisioning path and an IAM-policy shape before the product can accept the bucket a customer already has. Deferred, not rejected — see the amendment below.

**Rationale:** B is the mechanism the linked operator documentation describes, and choosing it means we write *what and when*, not a repository engine. Generic-S3-first is what makes the feature useful on day one: ADR-042's own argument for `type: s3` was that in-cluster MinIO and an external bucket "are the same code path differing only by endpoint + credentials" — this ADR simply builds that shared path and lets the user fill in the endpoint. An operator with an S3 bucket is configured in a minute; an air-gapped operator points it at their own MinIO; and when the platform-managed MinIO of ADR-042 arrives it becomes a *provisioner of this same config object*, not a new format. Optionality is not politeness either: making the wizard step skippable keeps the create flow at its current cost for everyone who is evaluating the product, while the deployment-level tab means skipping it is never a one-way door.

**Amendment to ADR-042.** ADR-042 chose "default in-cluster MinIO as shared platform infra, external bucket as a per-deployment override". This ADR **inverts the order of delivery, not the design**: the bring-your-own-bucket path ships first and there is no platform MinIO. Everything ADR-042 decided about the repository contract stands unchanged — `type: s3`, `{bucket, base_path, endpoint, path_style_access}`, no inline credentials, registration at provisioning time rather than lazily in `ensure_retention`. What is deferred is only the *provider* half: the shared MinIO deployment, bucket-per-tenant (`velox-snap-<tenant>`), and the scoped-key mint. Those remain the right answer for the multi-tenant free beta and are not foreclosed — they land as a default that pre-fills this same `SnapshotConfig`. A **MinIO for testing** is shipped now as an explicit fixture (`deploy/dev/minio.yaml`), not as bootstrap infra: it validates the S3 path end to end without pretending to be the product's storage.

**What gets built:**
- **`src/snapshot.rs` (pure, unit-tested, no cluster — the `src/upgrade.rs` shape):** `SnapshotConfig` + `PolicyConfig` (also the API DTO); `validate()`; `repo_entry()` rendering the `snapshotRepositories` entry with every value stringified and **no credentials**; `keystore_entry()` mapping `accessKey → s3.client.default.access_key` and `secretKey → s3.client.default.secret_key`; `policy_cr()` rendering the whole `OpensearchSnapshotPolicy`; and `needs_restart(old, new)` — the pure reduction of the gradient above into the one boolean the UI is handed.
- **`src/k8s.rs`:** a `veloxsearch-snapshot` field manager alongside `veloxsearch-auth`, applying **only** `spec.general.snapshotRepositories` + `spec.general.keystore`; `preflight_snapshot()` refusing before any write; `set_snapshot_config()` (Secret → CR slice → policy CR) and `read_snapshot_config()` returning credentials as a `secret_kept` sentinel; `Status` gains a `snapshot` block (configured, repo state, policy state, last error); `owned_secret_names()` gains the snapshot Secret.
- **`src/api.rs`:** `POST /api/snapshot_config` (read + `will_restart`), `POST /api/save_snapshot_config` (write), `POST /api/verify_snapshot_repo` (a non-writing `_snapshot/velox-snapshots/_verify` probe, the analogue of `test_auth_provider`). `ClusterReq` gains an optional `snapshot` honored **only on create**, applied after the cluster goes green because the reconciler needs `PhaseRunning`.
- **RBAC:** one rule for `opensearchsnapshotpolicies` in `deploy/install.yaml` and `deploy/k8s/veloxsearch.yaml`. The CRD is already in the vendored bundle (`deploy/bootstrap/operator.yaml`) — no operator change, no bootstrap RBAC change.
- **`src/profiles.rs`:** `ensure_retention()` stops registering the `fs` repository and merely references `velox-snapshots`, which now exists for real when the user configured it. The ISM policy is unchanged.

**Invariants (asserted in tests, not conventions):**
1. **No credential ever appears in the repository body.** A pure assertion over `repo_entry()`: `type: s3`, `bucket`/`base_path`/`endpoint`/`path_style_access` present, and no key material anywhere. This is the test ADR-042 already specified and never got.
2. **Saving a deployment never erases its snapshot configuration.** The slice lives under its own field manager, so `create_cluster()`'s full-manifest apply — which is also the `save_cluster` path — cannot prune it. Asserted by rendering the create manifest and requiring the slice to be absent from it.
3. **Pre-flight before write, always.** An invalid configuration creates no Secret, patches no CR and creates no policy. Same rule as ADR-048 invariant 2.
4. **`will_restart` is computed server-side.** The UI renders the answer; it never derives it.
5. **Every `settings` value is a string.** Asserted structurally, because the failure it prevents is an API-server rejection that reads like an unrelated schema error.
6. **Snapshot configuration is optional at every layer.** No config means: no CR slice, no policy CR, no Secret, and a create request identical to today's. The wizard step is skippable and skipping it is asserted in the browser journey.
7. **The Secret is cleaned up on delete** — asserted over `owned_secret_names()`, the exact place ADR-045's leak came from.

**UI:**
1. **A skippable wizard step.** Between "Dados" and "Revisão", a step whose default state is off: a single toggle, and while it is off, one line saying backup can be configured later on the deployment screen. Turned on, it reveals bucket / endpoint / region / path-style / access key / secret key, plus a collapsed "Política padrão" whose defaults (daily at 02:00, keep 7 days, 14 max, 3 min, all indices) are already valid — the user configures a working backup without typing a schedule.
2. **A Backup tab per deployment**, built on the ADR-045 auth-provider shape: its own read, write-only secret fields with the `secret_kept` sentinel, and a non-writing "verificar repositório" probe whose result renders inline.
3. **The restart is stated before it happens, and only when it happens.** A credential change confirms with the blast radius spelled out — nodes roll one at a time, unreplicated indices go unavailable while their node is down, minutes not seconds. A policy-only change saves with no confirmation and says plainly that nothing restarts.
4. **State is a state, not a spinner.** `pendente` (cluster still provisioning) / `configurado` / `erro` with the operator's `reason` verbatim, visible as a chip on the deployment overview next to the version chip.
5. **Errors render under the control, never in a toast**; i18n pt/en through `STR`; both themes. The standing ADR-019/ADR-045 rules.
6. **`upg_snapshot_p` stops lying.** The upgrade modal's "there is no automatic backup" line becomes conditional: it points at the Backup tab when a policy is configured, and keeps its warning when none is.

**Consequences / open questions for the build:**
- **Server-side encryption has to be named explicitly, and the only portable answer is `bucket_default`** (found live against MinIO, 2026-08-07). Leaving `server_side_encryption_type` unset makes `repository-s3` request encryption on its own, and MinIO answers **501 `Server side encryption specified but KMS is not configured`** — every snapshot fails with an error naming a KMS nobody asked for. `AES256` fails identically, because MinIO implements even SSE-S3 through its KMS; only `bucket_default` sends no directive at all. So the repository renderer pins it, and picking SSE-KMS with a named key stays an unexposed advanced case (ADR-015). The wider lesson: an S3 setting we do not send is not the same as a setting the plugin does not apply, and the failure surfaces as a storage error that reads like a user misconfiguration.
- **An orphaned repository is possible by design** (operator caveat 2). Turning snapshots off removes our slice but leaves the registration inside OpenSearch. Acceptable for now — a stale repository registration is inert — but the deregistration call belongs in #83 alongside the snapshot/restore routes, and it is written down here so it is not rediscovered as a bug.
- **A rotated credential is silently ineffective until the pods restart.** That is why credential changes restart rather than merely patching; but a user who edits the Secret out of band gets no such restart. The runbook covers it.
- **`repository-s3` is NOT bundled in the OpenSearch image — verified, not assumed** (2026-08-07, `docker run opensearchproject/opensearch:3.7.0 -- bin/opensearch-plugin list`): the distribution ships `repository-url` as a module and 26 plugins, none of them `repository-s3`. So it goes into `spec.general.pluginsList` **and** `spec.bootstrap.pluginsList`, and **the first snapshot configuration always rolls the nodes** — the plugin list is part of the pod spec. Two consequences follow. First, `pluginsList` must never be set on a deployment that does not use snapshots, or every deployment pays a restart for a feature it does not have. Second, the operator installs the plugin by running `bin/opensearch-plugin install --batch repository-s3`, which **downloads from `artifacts.opensearch.org`** (verified: the install succeeds by name, exits 0, and the plugin appears in the list) — so an air-gapped cluster cannot configure snapshots until the plugin is pre-baked into a custom image. That is a real limit of this design against the ADR-039 offline story, stated here rather than discovered in the field; the fix, when someone needs it, is a VeloxSearch-built image with the plugin baked in, not a change to this contract.
- **The ISM `hot → snapshot → delete` policy and the SLM policy overlap.** Both can write into `velox-snapshots`. The Observability profile's ISM snapshots per-index on age; the SLM policy snapshots the whole cluster on a cron. They coexist without conflict, but retention is then governed by two mechanisms and the interaction deserves a decision in #83 rather than an accident here.
- **Snapshots stored in the same cluster they back up are not disaster recovery** — ADR-042's honest caveat, inherited. The generic endpoint is precisely what lets an operator point at off-cluster storage and get real durability.

**Out of scope:** on-demand snapshot, snapshot listing, restore (#83); platform-managed MinIO with bucket-per-tenant and scoped-key minting (ADR-042 option B, deferred not rejected); object lifecycle tiering, cross-region replication and per-tenant storage-cost metering (already out of MVP scope per ADR-042).

**Tests:** unit — `validate()` refusals (empty bucket, malformed endpoint, `min_count > max_count`, malformed cron); `repo_entry()` carries no credentials and only string values (invariants 1 and 5); `policy_cr()` always sets `policyName` and renders the cron/retention it was given; `needs_restart()` is true exactly for credential transitions and false for policy-only and repository-body edits (invariant 4); the create manifest does not contain the snapshot slice (invariant 2); `owned_secret_names()` includes the snapshot Secret (invariant 7). Integration (`tests/snapshot_check.py`, against the MinIO fixture on local k3s): configure a repository, watch the policy CR reach `CREATED`, `_verify` the repository, take a snapshot and list it; edit the policy alone and assert **no pod restart**; and a negative case — a wrong secret key — landing the repository in `ERROR` with the S3 reason surfaced in the UI and the app still usable. Browser — the wizard's Backup step is skipped by the existing journey and the created deployment is byte-identical to today's (invariant 6).

## ADR-050 — "Ready" becomes a composed predicate the server computes: staged progress, controls locked while nodes roll, and the cluster's own events on screen

**Status:** **IMPLEMENTED** (2026-08-08) — prompted by a live false-green observed the day before, during the ADR-049 conformance run, and proven on local k3s against a real create and a real rolling restart.

**Verified / not verified.** **Verified live** (local k3s, OpenSearch 3.8.0): a full create (`provtest-781z`, 67 samples over ~5.5 min) never reported idle with nodes missing, never regressed its percent and never read 100% unsettled; a real rolling restart (node memory 2Gi → 3Gi on `teste-uu9d`, 95 samples over ~8 min) flipped to `restarting` and returned to `idle` only after the roll finished; and the activity log returned real lines from all three sources (`event`, `pod`, `operator`). **The window this ADR exists for was measured**: during that roll the StatefulSet reported `readyReplicas 3/3` with `updatedReplicas 2` for ~17 seconds — every pod up, the roll unfinished. That is long enough for the old UI to hide the panel and for a user to start editing. **Not** verified: the `volumes`, `security` and `dashboards` rungs were never sampled (the create went `accepted → nodes → idle`; on this single-node cluster PVCs bind and Dashboards start faster than the 5s polling interval), so their ordering is reasoned from the ladder rather than observed; the upgrade `kind`; and anything on ct1/Longhorn or production.

**Live conformance (2026-08-08) — PASSED, with two findings worth keeping.**
1. **The health reading inside the window is incidental; `updatedReplicas` is not.** In the measured roll, health read *yellow* during the all-pods-up-but-unfinished window; on 2026-08-07 the same window read *green*, which is what fooled the snapshot harness. Whether it lands green depends on shard replication timing, so a rule keyed on health is not merely wrong, it is **intermittently** wrong — the worst kind. The conformance assertion was tightened from "never idle with pods missing" to "never idle with `updated < desired`", which tests the clause directly instead of hoping the coincidence reproduces.
2. **Rotating the snapshot credentials cannot be used to force a roll.** It was the obvious trigger for the restart test and it does nothing: re-saving the *same* key material writes an identical Secret, the pod template does not change, and the StatefulSet never rolls — verified by watching `readyReplicas/updatedReplicas` hold at `3/3/3` throughout. Our `needs_restart` still *plans* a restart there, because it cannot know the incoming key equals the stored one without reading back a secret it deliberately never reads. Erring toward "this will restart" is the correct direction (never promise calm and then roll), but it means the ADR-049 modal can warn about a restart that does not happen, and that path cannot be used to cause one on demand. The conformance now rolls the nodes through a node-memory change instead.

**Date:** 2026-08-08 · **Builds on:** ADR-005 (the provisioning/troubleshoot panel this replaces), ADR-048 (day-2 operations report live state read from the CR, never from in-memory bookkeeping), ADR-049 (a credential change rolls the nodes), ADR-031/043 (PVC-backed storage, so volumes are a real provisioning stage) · **Amends:** ADR-005 (the 4-minute timeout is now measured from the server's start-of-activity, not from a component mount) · **Constrained by:** ADR-044 (tenant isolation — the reason container logs are out)

**Decision:** A deployment is "ready" when the server says so, not when `status.health` says `green`. The backend computes a single **`activity`** object per deployment — `{kind, stage, percent, detail, settled, locks_edits}` — from the CR **and** the StatefulSet **and** the PVCs **and** the Dashboards Deployment, and every consumer reads that one answer: the provisioning panel hides on it, the mutating controls lock on it, and `wait_settled` (today's `wait_green`) blocks on it. The UI renders the verdict and never re-derives the rule — the same discipline as `upgrade_options`' `blocked_reason` (ADR-048) and `plan_snapshot_config`'s `will_restart` (ADR-049). Progress is a **weighted stage ladder** with sub-progress inside the current stage, and a **"Detalhes" accordion** shows the deployment's real Kubernetes Events, pod container states and the operator's `componentsStatus` rows — no container logs.

**Context:** the false green, observed rather than theorised. Running `tests/snapshot_check.py` on 2026-08-07, the harness waited for `health == "green"` after a credential change, got it, and then found the snapshot repository reporting `repository_missing` for four minutes. The cluster had reported **green with 2 of 3 nodes ready**: the operator restarts one node at a time, and between two restarts every pod is briefly up, so `health` goes green while the roll still has nodes to go. The test was wrong in exactly the way the product is wrong, because both encode the same belief.

That belief is one line: `frontend/views_deployment.jsx:552` renders the whole provisioning panel on `d.health !== "green"`. Three things follow, all present today:

1. **The panel disappears early.** Green implies nothing about nodes being ready, security being initialized, Dashboards being up, or the StatefulSet having no pending revision.
2. **Nothing locks.** A grep for `d.health`/`d.phase` across the frontend returns 11 hits, every one cosmetic. Editing sizing, toggling an integration, resetting the admin password, switching the identity provider and saving a snapshot repository are all fully live while the cluster is coming up. `EditTab` even warns that saving restarts the nodes — and then lets that happen *during another restart*.
3. **The progress does not inform.** The only bar is `nodes_ready/nodes_desired`, which sits at 0% through the longest and most confusing minutes (PVC provisioning, image pull). `phase` is printed raw. The elapsed clock starts at component mount, so switching tabs resets it. And the "details" are four static sentences telling the user to go run `kubectl`.

**The signal that was missing, and was already being read:** `status.updatedReplicas` on the StatefulSet. `k8s.rs` has fetched it since ADR-048 (for the "nó N de 3" line) but nothing ever used it to answer "is this settled". During a roll `updatedReplicas` climbs 0→3 while `readyReplicas` oscillates 2↔3, so there is always a window where ready equals desired and updated does not. That single comparison distinguishes a settled cluster from a rolling one, with no timers, no debounce and no state to keep.

**Options:**
- **(A) Keep `health == green`, add a debounce** — require green for N consecutive samples. Cheap, and it is what the test harness did as a stopgap. Rejected as the product answer: it is a guess about time standing in for a fact about state, it makes every genuine completion N samples slower, and it still cannot tell a rolling restart from a healthy cluster — a long enough gap between two node restarts satisfies any N.
- **(B) Track provisioning stages in backend memory** — a job record per deployment, like `bootstrap.rs`'s `Job`. Familiar in this codebase, and it would give exact stage timestamps. Rejected because it does not survive a backend restart or a second replica, which ADR-048 already ruled out for upgrades for the same reason; a deployment's state must be readable from the cluster alone.
- **(C) Compose the predicate from what the cluster already reports — RECOMMENDED (this).** The CR carries `initialized`, `health`, `phase`, `componentsStatus`; the StatefulSet carries `ready`/`desired`/`updated`; the PVCs carry `phase`; the Dashboards Deployment carries readiness. All of it is already permitted by the runtime RBAC and most of it is already fetched. Stateless, restart-proof, and the same input set explains *why* it is not ready, which is what the stage ladder and the accordion render.
- **(D) Ask the operator.** `componentsStatus[]` looks like the intended answer, but a live check on 2026-08-08 found a healthy cluster reporting a single row (`RollingRestart: Finished`). It is a useful *input*, not a sufficient one. Folded into C rather than adopted alone.

**Rationale:** C is the only option that is a statement about the world instead of a statement about time. It also fixes the backend's copy of the bug for free: `wait_green` polls the same `health == "green"` and hands off to `profiles::apply`, `recipes::apply` and the ADR-049 repository registration — the very tasks that were racing a rolling cluster. One predicate, one definition, three call sites corrected at once.

**Why container logs are out.** Reading pod logs needs `pods/log` on the runtime ClusterRole, which is cluster-wide and therefore crosses the tenant boundary ADR-044 exists to hold — one grant, and the app can read any pod's stdout in any namespace. And it would not even answer the question: a provisioning pod is typically `Pending` or in `ImagePullBackOff` and has **no log at all**. What explains a stuck provision is the Event stream and `containerStatuses`, and the app already has `get/list/watch` on both. Stated here so it is not relitigated: the accordion is deliberately Events + pod state + operator components, and it is *more* informative than logs for this specific question, not a lesser substitute.

**What gets built:**
- **`src/activity.rs` (pure, unit-tested, no cluster — the `upgrade.rs`/`snapshot.rs` shape):** `ActivityInput` (the gathered signals), `Activity` (the verdict), `evaluate()`, and the weighted ladder `storage → accepted → volumes → nodes → security → dashboards → settling`. `percent` is a function of the furthest stage reached plus sub-progress inside it, so it cannot walk backwards.
- **`src/k8s.rs`:** `Status` gains `activity`. `status_from` reads `.status.initialized` (declared in the CRD since forever, never read), counts bound PVCs via the existing `data_pvcs()`, and checks the Dashboards Deployment — but **short-circuits**: a deployment that is already plainly stable returns `idle` without those extra calls, so steady-state SSE cost is unchanged. `wait_green` becomes `wait_settled`. `preflight_snapshot` (ADR-049) switches to the same predicate.
- **`src/api.rs`:** `activity` on `ClusterStatus`, and `POST /api/deployment_activity_log` — Events for the CR, its pods, its PVCs and the Dashboards Deployment, plus per-pod container state and the full `componentsStatus`, normalized into one time-ordered list, capped, best-effort per source.
- **Frontend:** `ProvisioningPanel` becomes `ActivityPanel`, rendering on `activity.kind !== "idle"` and absorbing `UpgradeProgress` so creation, upgrade and restart share one surface. A `lockReason(d)` helper disables the save actions of every mutating tab with the reason stated in place.

**Invariants (asserted in tests, not conventions):**
1. **A rolling restart is never `settled`.** `health: green`, `ready == desired`, `updated < desired` ⇒ not settled. This is the regression test for the observed bug; if anyone ever simplifies the predicate back to health, it fails.
2. **`percent` is monotonic, capped at 100, and never reaches 100 while `settled` is false.**
3. **The UI never derives the rule.** It renders `activity`; the words "green" and "ready" appear in no client-side condition that gates a control.
4. **Delete is never locked.** A provision that never finishes is exactly when the user most needs to remove it; locking it would leave a zombie only `kubectl` can clear.
5. **Steady state costs what it costs today.** With every deployment settled, an SSE frame makes the same number of API-server calls as before this change.
6. **No provisioning state lives in memory.** Everything is re-derived from the cluster on each read, so a backend restart or a second replica changes nothing.

**UI:**
1. **The panel hides only on `idle`** — and covers creation, upgrade and node-restarting operations with one component, since they are the same question asked at different times.
2. **A stage ladder, not a spinner** — the seven stages in the `StepRow` idiom the bootstrap screen already uses, with the overall percent alongside. The label always names the stage and its detail ("Subindo nós · nó 2 de 3"); the number is secondary to the words.
3. **The clock comes from the server**, so switching tabs no longer restarts it, and the troubleshoot threshold measures real elapsed time.
4. **"Detalhes" is a closed-by-default accordion** that fetches the activity log on open and polls only while open. Messages render verbatim, the same treatment `snapshot.last_error` and `upgrade.reason` already get.
5. **A locked control says why, where it is** (ADR-045 UI rule 1) — never a dead button. Tabs stay navigable: the user can read the configuration, just not save it.
6. **The deployment list shows the stage and percent** instead of the raw health word for a deployment that is not idle.

**Consequences / open questions for the build:**
- **`kind` is derived, not remembered.** `!initialized` ⇒ `creating`; `initialized && upgrade.in_flight()` ⇒ `upgrading`; `initialized && updated < desired` ⇒ `restarting`. Cheap and stateless, but it means a cluster whose security bootstrap genuinely regressed would read as `creating` again. Acceptable — that *is* what is happening to it.
- **The short-circuit is load-bearing.** Without it, the extra PVC list and Deployment GET run for every deployment every 3s. The condition that skips them must stay strictly stronger than the condition that would have produced `idle` anyway, or a deployment could report idle without the checks that define idle.
- **Locking is per deployment, by decision.** Navigation, the list, creating another cluster and every other deployment stay usable; a cluster coming up must not freeze the whole panel.
- **The Events feed is unbounded upstream.** Kubernetes expires events (default 1h), so a long provision can outlive its own early events. The cap is on our side too; the accordion is a live window, not an audit trail.

**Tests:** unit — invariant 1 (the false-green regression), `settled` false with `initialized: false` and with Dashboards down, `percent` monotonic over a synthetic provisioning sequence and never 100 while unsettled, and `kind` classification across the four cases. Conformance (`tests/provision_check.py`, new): create a deployment, sample until `idle`, assert no sample was ever `idle` with `nodes_ready < nodes_desired`, that `percent` never regressed, and that the activity log returns sourced, timestamped lines; then trigger a restart through the ADR-049 credential path and assert `kind` becomes `restarting` and returns to `idle` only after the roll completes. `tests/snapshot_check.py`'s three-consecutive-greens heuristic is replaced by `activity.settled` — the harness stops guessing and consumes the product's own contract. Browser: with a deployment provisioning, the save control is disabled and the delete control is not.

### ADR-050 amendment (2026-08-15, issue #131) — a verdict that stops moving has to say why, and there is only one node pair

**Status:** IMPLEMENTED. **Not verified live** — no cluster was available; see "What this does not prove".

**What happened.** A production deployment rendered `RESTARTING THE NODES — Starting the nodes 0/3 — 25%` for **sixteen hours**. On the same screen at the same moment: the NODES tile read `3/3 — all ready`, the progress counter read `6/3` and later `8/3`, the elapsed clock kept resetting (0:20, then 0:16), and documents climbed 2,005 → 6,581 with ingestion live throughout. The operator was looping `Couldn't proceed with rolling restart for Pod …-nodes-0 because waiting for health to be green` every 11 seconds, because a peer recovery of `.opendistro_security` had been stuck in `init` at 0.0% for 15.9h with 7 replicas unassigned behind it (`reached the limit of outgoing shard recoveries [2]`). Every primary was fine, which is exactly why every tile looked healthy.

**What the original ADR got wrong.** Not the predicate — `settled` was correct for all sixteen hours, and the panel correctly refused to hide. Three things around it:

1. **UI rule 5 was written for locks, not for stalls.** "A locked control says why, where it is" was implemented; "a *blocked deployment* says why, where it is" was not. What the panel had past its threshold was `act_slow_hint` — three guesses at common causes ("an image still downloading, a volume that will not provision, a node with no memory") none of which was this one. Vague text is indistinguishable from no text: it ran for sixteen hours and taught the reader to ignore it.
2. **The 4-minute threshold was a client-side rule measured against a client-side clock.** ADR-050 UI rule 3 says the clock comes from the server; it did not. `useActivitySince` anchored on component mount, keyed by `activity.kind`, so a tab switch restarted it — which is the resetting timer in the screenshots. Invariant 3 ("the UI never derives the rule") was violated by the threshold itself.
3. **"One answer" was never enforced for the node counts.** `Status.nodes_desired` was the StatefulSet's `.status.replicas`; the SPA's tile divided by the CR's `spec.nodePools[0].replicas`; the stage counter divided `updatedReplicas` by the CR's; nothing clamped any of them. Kubernetes has no invariant tying a live pod count to a spec request — and the operator manufactures the divergence *on purpose*, since it refuses to reconcile the node pool while health is not green. So `6/3` was not a glitch: it was a correct count over a correct request, printed as a fraction.

**Decision.** `Activity` gains `stalled`, `since_secs`, `serving`, `nodes_ready`/`nodes_total` and `blocked`:

- **`stalled`** — no progress for `activity::STALL_AFTER_SECS` (the same 4 minutes, now server-side). "Progress" is measured as the age of the newest node pod, falling back to the CR's `creationTimestamp`. A roll advances by replacing a pod, so that age *is* the time since the last real advance, and it is immune to both things that corrupted the old clock: an operator reconcile writes CR status and creates no pod, and a tab switch touches nothing in the cluster. Stateless, per invariant 6.
- **`blocked`** — structured facts, never a sentence: health colour, unassigned shards, the operator's first unfinished `componentsStatus` row, and the oldest active recovery with its stage and age. The SPA composes the wording in both locales; the only strings crossing the wire are the cluster's own vocabulary, the treatment `upgrade.reason` already gets. Each field is independently optional and `unassigned_shards: -1` means "we did not get an answer", never "zero".
- **`serving`** — primaries up and the cluster answering, orthogonal to `settled`. Both were true for sixteen hours and the screen could state neither, so an operator reading `0/3 — 25%` concludes there is an outage and starts intervening on a cluster that is working.
- **`nodes_ready`/`nodes_total`** — the one pair, clamped, and `Status`'s pair is now assigned *from the verdict* so nothing downstream can arrive at a different answer.

**Cost, and how invariant 5 survives.** The two facts Kubernetes cannot know (unassigned shards, the stuck recovery) need OpenSearch: `_cluster/health` and `_recovery?active_only=true`. Fenced three ways — asked only for a deployment the pure Kubernetes verdict has *already* called stalled, memoized 30s per deployment, and each request capped at 4s so a hung cluster cannot hang an SSE frame that is serially building every deployment's status. The settled short-circuit is untouched: a stable deployment makes exactly the calls it made before. What *is* new on the unsettled path is a pod LIST per status read, for the clock; it sits beside the PVC LIST that path already did, and it stops when the deployment settles.

**What this does not prove.** No live cluster was available. The stall path is proven by unit tests against a fixture reconstructed from the incident's own logs and screenshots, and by the shape of the two OpenSearch responses — **not** by observing a real stall. Specifically unverified: that `_recovery?active_only=true` on this operator's OpenSearch build reports `total_time_in_millis` for a recovery stuck in `init` (the incident evidence is `_cat/recovery`'s `15.9h`, a different endpoint); that the diagnosis renders inside the 4s timeout on a cluster under recovery pressure; and the browser gate's new assertions, which only fire when a deployment happens to be busy while it runs.

## ADR-051 — Tenant isolation is provisioned at signup: the ADR-044 primitives, wired (and what still is not proven)

**Decision:** ADR-044 chose namespace-per-tenant and shipped the templates; this ADR records the **wiring** and the amendments the implementation forced. `tenants::signup` now calls `k8s::provision_tenant`, which renders the four vendored templates (`deploy/tenant-templates/`) and server-side-applies them under its own field manager `veloxsearch-tenant`: the **Namespace**, a **ResourceQuota** rendered from the tenant's ADR-041 `quotas` row, a **LimitRange**, and the five-policy **default-deny NetworkPolicy set**. The isolation model is unchanged from ADR-044 and is now real in five independently-owned layers: **(1)** app-layer ownership — the `Scope`/`Deployment` capability tokens of `src/scope.rs`, enforced by the type system (#80) → **(2)** the tenant namespace and its namespaced Secrets/RBAC → **(3)** default-deny NetworkPolicy in both directions with five enumerated holes → **(4)** ResourceQuota + LimitRange against noisy neighbours, backstopping #84's admission gate → **(5)** per-deployment OpenSearch admin credentials (ADR-030) and per-tenant snapshot buckets (ADR-042). **The provisioning is verified by unit test against the rendered manifests only. Live cross-tenant denial is NOT verified by this MR** — see "What this does not prove", which is the most load-bearing paragraph here.

**Date:** 2026-08-11 · **Issue:** #81 · **Epic:** #77 · **Implements:** ADR-044 (model + templates), ADR-041 (`tenants.namespace`, `quotas`), ADR-022 (`include_str!` + token-replace bundles) · **Composes with:** ADR-030, ADR-042, #80 (layer 1), #84 (admission gate), #87 (security review)

**A note on the number:** issue #81's own text calls this work "ADR-046". Every number it reached for was taken by the time it landed: ADR-046 by the platform-alerting watchdog, ADR-047 by the catalog cache, and 048/049/050 by the upgrade/snapshot/readiness branch merged just before this. This is **ADR-051**; read "#81's ADR-046" as this document. No existing ADR text was edited to say so — amendments below, never rewrites, so the concurrent lanes merge trivially.

### What was decided in the wiring, and why

- **Provisioning is best-effort, after the commit, never inside it.** `create_user_and_tenant` commits the account, then `provision_isolation` runs. A Kubernetes round-trip must not hold a Postgres transaction open, and losing an account because the cluster hiccuped is the worse trade — the same reasoning that already makes the verification mail best-effort. The consequence is stated rather than hidden: a tenant can exist in Postgres with no namespace. That state is recoverable (provisioning is idempotent, so re-running fixes it) and it is **observable**: an audit row is written on **both** paths, `tenant.provisioned` or `tenant.provision_failed`, because after a bad signup the operator's first question is "which tenants are running without walls" and silence is not an answer to it.
- **The quota is a rendering of Postgres, never numbers typed into a manifest.** `render_quota_hard` derives `spec.hard` from the `quotas` row × the **largest** sizing preset read from `sizing()` itself — so adding a preset moves the quota, and there is never a second copy of the sizing table. Two constants make the derivation honest: `QUOTA_COUNT_HEADROOM = 2`, because a rolling update runs the new pod (and, on a resize, its PVC) beside the old one and a steady-state quota would deadlock the very update it exists to survive; and the `AUX_*` allowances for the Dashboards pod and the operator's securityconfig job, which are ADR-044's worked-default deltas made explicit instead of arriving as unexplained slack. ADR-044's worked default (1 × 3 nodes × 50Gi → pods 8 · requests.cpu 4 · limits.cpu 7 · requests.memory 10Gi · limits.memory 12Gi · requests.storage 50Gi · pvcs 6 · clusters 1) is pinned by a test, so the day the code stops agreeing with the ADR, the test says so.
- **An unresolved token is a hard refusal, not a manifest.** This is the single most important guard in the change. A `VELOX_*` token left standing renders a NetworkPolicy whose `namespaceSelector` matches a literal string no namespace carries: the object is created, `kubectl get netpol` looks healthy, and the flow is dead — or, worse on the deny side, an object that selects nothing and denies nothing. `render_tenant_bundle` scans each rendered document (comments included — a template's header lists its own tokens) and errors out rather than emitting. The template README carries the matching rule: renaming a token without updating `TENANT_TEMPLATES` breaks the build's tests, not production.
- **Peer namespaces are read from their owners, not re-typed.** `NamespaceLayout` takes `control_plane` from `ns()` and `agents` from `agents::AGENT_NS` — which is why that constant was widened to `pub(crate)`. Every peer is a *selector value*, so a wrong one is a silently broken flow rather than a visible error; a second `"velox-agents"` literal in the policy would keep compiling, keep passing every fixture test, and kill ingest the day `agents.rs` renamed its namespace. `ingress` and `minio` are `VELOX_INGRESS_NAMESPACE` / `VELOX_MINIO_NAMESPACE` (defaults `traefik` / `minio`) because they differ by distribution and by install; the MinIO egress hole is a forward declaration that selects nothing until ADR-042's MinIO exists.
- **A `tenants` row that does not match the ADR-041 mapping is refused before anything is applied.** Empty tenant id (the owner label would select every object), a slug that is not a DNS-1123 label, or a `namespace` that is not `velox-t-<slug>`. The failure this prevents is specific and fatal: a bad row would let us apply a **default-deny NetworkPolicy to `kube-system`**.
- **RBAC widened by exactly three resources.** `veloxsearch-runtime` (ClusterRole) gains get/list/create/update/patch on `resourcequotas`, `limitranges`, `networkpolicies`. Cluster-scoped, though ADR-044 wiring item 3 called a per-tenant Role the least-privilege default, because that Role's binding would have to be created inside a namespace that does not exist until we provision it — a chicken-and-egg the per-tenant form cannot resolve at signup. No `delete`: offboarding removes the namespace and the objects cascade.

### Amendments to earlier ADRs (recorded here, not edited into theirs)

- **ADR-044's label domain erratum is now closed in the templates.** ADR-044 wrote `veloxsearch.io/tenant`; #80's amendment corrected the product to `veloxsearch.ai/`. The tenant templates carry `veloxsearch.ai/managed-by: veloxsearch` and `veloxsearch.ai/tenant: <tenants.id>` — the constants `scope::LABEL_MANAGED_BY` / `scope::LABEL_TENANT` / `scope::MANAGED_BY`, asserted by test against those constants rather than against string copies, plus `veloxsearch.ai/tenant-slug` for human legibility. The owner value is `tenants.id`, not the slug, so a rename cannot orphan an object. A test also fails the build if `veloxsearch.io/` reappears anywhere in the templates: a sweep by the wrong domain finds nothing and reports clean.
- **ADR-044 deferred this wiring "after #92's datastore bring-up merges". It landed first.** The sequencing worry was conflict on shared surfaces; in the event this change touches `src/k8s.rs` only additively (a new section plus its tests), so the deferral cost more than it bought.
- **ADR-044's NetworkPolicy set gains a sixth token, not a sixth flow.** The agents peer was the literal `velox-agents` in the template; it is now `VELOX_AGENTS_NS`, rendered from `agents::AGENT_NS`. Same five flows, one fewer place for them to drift.
- **What ADR-044 wiring item 3 still owes, narrowed.** Per-tenant Secrets/Ingress permissions remain unlanded. They are not needed by this MR — deployments still live in the app namespace — and become blocking the moment a deployment is created *in* a tenant namespace. Item 2 (deployment-scoped `Api`s taking the tenant namespace) and item 4 (`discovery.rs` exclusion list) are likewise untouched here.

### What this does not prove

**Live cross-tenant denial is UNVERIFIED.** Everything above is tested against rendered YAML; nothing in this MR observed a packet. Three distinct gaps, stated separately because they fail for different reasons:

1. **NetworkPolicy is enforced by the CNI, not the API server.** k3s' embedded controller enforces these; a CNI without policy support **accepts the objects and enforces nothing**. The Tornis cluster runs Cilium 1.15.6, which does enforce — but that is an inference from the CNI, not an observation of a blocked connection.
2. **A policy that selects nothing looks exactly like a policy that works.** Every guard above (the unresolved-token refusal, the peer-set test, the port-per-peer test) exists because this failure mode is invisible in `kubectl`. Those guards prove the *rendered document* names the right namespaces and ports. They cannot prove a live namespace carries the `kubernetes.io/metadata.name` label the selector matches, nor that the CNI agent has the policy programmed.
3. **The API-server-side quota behaviour is untested.** That a `count/opensearchclusters.opensearch.org` over the limit is *rejected*, and that a request-less pod schedules under the LimitRange defaults instead of being quota-rejected, are both assertions about the admission chain, not about our YAML.

**The test that would close this** is ADR-044's already-specified conformance probe, and only it counts as evidence (the standard ADR-042 set with its cross-tenant 403 bucket probe): on a live cluster, provision tenants A and B; run a pod in A's namespace; `curl` B's OpenSearch Service on 9200 and require a **timeout**, not a refusal (a connection refused would mean DNS and routing worked and nothing dropped the packet — the wrong evidence); in the same probe, confirm the *allowed* flows still pass, because a default-deny that also breaks ingest is a different outage, not a success; then create one `OpenSearchCluster` past `max_deployments` and require an API-server rejection. Until that probe runs green, #87 must treat "the NetworkPolicy objects exist" as insufficient, and the beta's isolation guarantee rests on layer 1 — `scope.rs` — with layers 2–4 as untested depth.

## ADR-052 — Deferred provisioning is recorded on the CR and retried on a bounded schedule; "not applied" becomes a state the deployment reports (amends ADR-050)

**Status:** **IMPLEMENTED** (2026-08-15) — code + unit tests. **Not verified on a live cluster**; see "What this does not prove".

**Date:** 2026-08-15 · **Amends:** ADR-050 (whose stricter `wait_settled` is what made the timeout reachable) · **Builds on:** ADR-048 (state lives on the CR, never in backend memory), ADR-018 (monitor-at-creation), ADR-028 (purpose profiles), ADR-039 (catalog packages as monitors), ADR-049 (the repository is registered after the cluster settles)

**Decision:** The deferred half of a create or save — the purpose profile and the selected monitors — stops being fire-and-forget. Three things, all of them derived from the cluster so a backend restart or a second replica changes nothing:

1. **What has been applied is recorded on the CR**, in a `veloxsearch.ai/provisioning` annotation. What is *owed* is **not** recorded: it is re-derived from the `veloxsearch.ai/purpose` label and the `veloxsearch.ai/monitors` annotation the create itself wrote. The record carries only the applied set, the attempt count and the last error.
2. **The wait is a bounded widening schedule**, not one shot: 600s, 900s, then 1800s three times (≈1h55m), with a capped exponential delay between attempts for the case where the cluster settles fine and an item keeps failing. The attempt counter is persisted, so a crash-looping backend cannot restart the schedule forever.
3. **The deployment reports the answer.** `Status` and the DTO gain `provisioning: {state, profile_pending, monitors_pending, attempts, last_error, updated_at}` with `state ∈ {complete, pending, failed}`, and `POST /api/retry_provisioning` starts a fresh schedule. The UI renders the verdict and does not re-derive it — the same discipline as `activity` (ADR-050) and `upgrade_options.blocked_reason` (ADR-048).

**Context — the failure, observed twice in two days.** On 2026-08-13 a create's deferred task timed out:

```
2026-08-13T21:29:44 ERROR veloxsearch::api::server: deployment tornis-hj27 did not settle
within 600s (stuck at nodes (25%)); profile + selected monitors were not applied
```

The cause of the slow settle was elsewhere and is somebody else's fix: a hung peer recovery kept the operator's rolling restart from finishing, so the cluster never reached `settled`. What this ADR is about is what happened next. The deployment came up green and healthy. Its CR still carried `veloxsearch.ai/monitors: kubernetes,nginx` and `veloxsearch.ai/purpose: observability` — the record of what the user asked for, written by the create. There were no collection agents and no profile. **In the API, and therefore in the UI, that deployment was indistinguishable from a correctly provisioned one**: `health: green`, `activity: idle`, monitors listed. The only evidence was a log line, on the server, that nobody reads. The recovery the operator eventually found was "click Save on the Edit tab", which is not discoverable and was never documented because nobody designed it.

**ADR-050 is why this became likely, which is why this amends it rather than merely citing it.** That ADR correctly replaced `health == "green"` with a composed `settled` predicate and pointed `wait_green` at it. Waiting for a strictly stronger condition means waiting longer, and its consequences section did not ask what the 600s timeout — inherited unchanged from the weaker predicate — now meant. It meant that a deployment whose roll took eleven minutes silently lost its configuration. The predicate was right; the failure handling behind it was not.

**Options:**
- **(A) Raise the timeout.** One character, no new surface. Rejected: it moves the cliff without removing it, and the failure at the new number is identical and equally silent. A timeout is a guess about time; the bug is that exceeding the guess is unrecoverable and invisible.
- **(B) Retry in the background task alone, unbounded.** Fixes the slow-settle case and nothing else. Rejected on two counts: a task that never exits and never reports is a leak with a pleasant name, and it dies with the pod — a backend restart during a long provision loses the work with no trace, which is the same bug in a shorter window.
- **(C) A job record in backend memory**, the `bootstrap.rs` `Job` shape. Familiar here, and it would give exact per-item timing. Rejected for the reason ADR-048 and ADR-050 already rejected it twice: it does not survive a restart or a second replica, and a deployment's state must be readable from the cluster alone.
- **(D) A record on the CR + a bounded retry + a reported state — RECOMMENDED (this).** The intent is *already* on the CR, written by the create, so the only fact missing is which parts have been carried out. Adding that one fact makes the outstanding work re-derivable by anyone who reads the object, which is what makes the retry stateless, the report free, and the manual retry possible at all.

**Rationale:** the shape of D falls out of an observation that was sitting in the incident the whole time — the CR of `tornis-hj27` said exactly what should have been applied. Nothing had to be *remembered*; only *acknowledged*. Recording the applied set rather than the pending set is what makes the record incapable of disagreeing with the deployment: change the monitors in the edit tab while a retry is pending and the next attempt plans against the new list, because the plan is a function of the CR and not a copy taken at create time.

**Consequences / notes for the build:**
- **Absence of the annotation means complete.** It is both the terminal state (the applier removes the record when it finishes) and the state of every deployment that predates this change. The alternative — absence as unknown — would report the entire existing estate as incomplete the moment this deploys.
- **The record is a cache, never a gate.** Everything it tracks is an idempotent upsert by contract (`profiles::apply`, `recipes::apply`, `catalog::install`), so a record that is wrong costs re-applied work and nothing else. A malformed annotation therefore parses as "nothing applied" rather than as an error: pessimism is the safe direction, and refusing to parse would strand a deployment on a string.
- **The mark is written in the request path, before the task exists.** A backend that dies during the wait then leaves a deployment that reports "not applied" instead of one that looks finished. The remaining window is the milliseconds between the CR apply and the mark patch.
- **`save_cluster` gets the same applier, and gains behaviour it should always have had.** It had the identical hole, which is uncomfortable given that clicking Save was the accidental recovery from the create-path bug — that only worked because save's own timeout did not fire. Save also writes the monitors annotation, so before this a monitor ticked in the edit tab was recorded on the CR and never installed. It is now applied, and because the applied set is kept across a save, a save applies only what actually changed.
- **The snapshot configuration is deliberately NOT in the record.** It carries S3 credentials, which must not go into an annotation, so it lives only in the applier's memory. In-process retries cover it; a backend restart between create and settle loses it and the user re-enters it in the snapshot tab. Closing that would mean persisting the credentials at create time, which ADR-049's pre-flight (it refuses an unsettled cluster) does not currently allow. Stated rather than hidden.
- **Giving up on the timer is not giving up on the deployment.** When the schedule is spent the record stays, the state reads `failed` with the last error, and `/api/retry_provisioning` hands out a fresh schedule. An unbounded retry would be the *less* honest design: it would keep a task alive forever while telling the user nothing.
- **The upgrade path (ADR-048 phase 2) is knowingly left alone.** It has the same wait-then-act shape and a 3600s bound, but not the same defect: an unfinished upgrade is already legible from the CR (`upgrade.in_flight()`, `target_version`, a `dashboards_version` behind the nodes), already in the DTO, and already has an explicit `phase_two_only` retry through the pre-flight. It is bounded but not silent, which is the property that matters here.
- **Concurrent appliers are possible and harmless.** Two retries can overlap; idempotence is what makes that a waste of API calls rather than a corruption. No lock is taken, because a lock would be in-memory state and this ADR exists to not have any.

**Invariants (asserted in unit tests, not conventions):**
1. **The profile is planned before any monitor.** The profile installs the retention ISM policy whose `ism_template` attaches to indices created *afterwards*, and the monitors are what create them. A reordering would put a deployment's logs outside retention, silently.
2. **A deployment with no record is `complete`** — the existing-estate guard.
3. **A record with nothing outstanding is `complete`**, so a failed clearing patch cannot leave a healthy deployment flagged forever.
4. **`pending` and `failed` are distinguishable**, because they ask different things of the user: one means a retry is still coming on its own, the other means nothing further happens unless they act.
5. **Changing the purpose makes the profile pending again**, and deselecting a monitor withdraws it — both because the plan is derived, never stored.
6. **The schedule is bounded and widening**, and its total is asserted, so widening it is a deliberate act rather than a drift.
7. **A corrupt record re-applies rather than errors.**

**What this does not prove.** Everything above is unit-tested against pure functions and compiled against the real call sites; **nothing here was run against a cluster**. Specifically unverified: that the annotation round-trips through a real API server under the field managers in play (the record is patched with a JSON merge patch while `create_cluster` owns `metadata.annotations` through server-side apply — the merge patch is expected to win the key, but that is reasoned, not observed); that a second attempt genuinely catches a cluster which settles after minute ten; that `catalog::install` and `recipes::apply` are idempotent in the way their contracts claim when re-run against a half-installed monitor; and the UI, which is not touched by this change beyond the DTO it can now read. The conformance test that would close this is an extension of `tests/provision_check.py`: create a deployment against an operator held artificially below `settled` past the first budget, assert the deployment reports `provisioning.state: pending` with the right `monitors_pending`, release the operator, and assert the monitors appear and the annotation is removed — then re-run the same create with the backend restarted mid-wait and assert the state is still reported.

## ADR-053 — The OpenSearch Observability Stack as a second, additive collection mode (OTel traces + metrics)

**Decision:** Add a **second collection option** — an OpenTelemetry stack installed per deployment, day-2, from the Integrations tab — **alongside and never replacing** the Fluent Bit recipes. Five components in the existing `velox-agents` namespace: **OTel Collector** (OTLP 4317/4318), **Data Prepper** (spans → Trace Analytics documents), **Cortex** (PromQL metric store), **Alertmanager**, and the **OpenSearch Prometheus exporter**. Manifests are generated in Rust (`src/otel_stack.rs`) with `serde_json::json!`, exactly like `agents.rs` — not vendored Helm output, not an ADR-039 registry package.

**Date:** 2026-08-15 · **Modelled on:** `opensearch-project/observability-stack` (Alpha) · **Extends:** ADR-004 (which originally chose OTel for everything, before Fluent Bit became the fallback-that-stayed), ADR-040 (which moved the deploy target to ≥3.6 *for* Agent Traces)

**The gap this closes.** ADR-040 bumped OpenSearch to 3.7/3.8 specifically to get Agent Traces, but nothing in the product emits OTLP spans — so the capability we upgraded for was dark. More broadly: the recipe path is good at logs and blind to traces, metrics, service maps and RED.

**This installs no UI.** `observabilityDashboards` and `queryWorkbenchDashboards` ship in every non-minimal OpenSearch Dashboards distribution, and our CR (`k8s.rs`) names no image override — so Trace Analytics, Agent Traces, Metric analytics, Event Analytics/PPL, Applications, Notebooks, Alerting and Anomaly Detection are **already present on every deployment we provision**. They are empty because nothing feeds them. This feature installs *data producers* plus two configurations (a Prometheus datasource and an ISM policy). The one thing that genuinely is not an OpenSearch plugin is the **Alertmanager UI**, which has no public route by design.

**Options:**
- **(A) Install their Helm chart as-is.** It deploys its *own* OpenSearch + Dashboards via the community charts, standing up a second cluster beside the operator-managed one — duplication, not integration.
- **(B) Install their chart with the bundled OpenSearch disabled** (`opensearch.enabled=false` + `opensearchServiceName` override — the seam does exist). Rejected on evidence: its init Jobs hardcode `http://{{ .Release.Name }}-opensearch-dashboards:5601`, which is the very subchart that must be disabled, and they `pip install requests pyyaml` at container start, which is dead in an air-gapped install (the ADR-039 Ministério-Público-class buyer). It also pins Data Prepper to `sgguruda62324/opensearch-data-prepper:2.16.0-SNAPSHOT-rc1` — a **personal Docker Hub account holding a release candidate**.
- **(C) Vendor the rendered chart** (the `deploy/bootstrap/*.yaml` + `apply_bundle` pattern). Rejected: `helm template` fixes release name and namespace, and this object set is per-deployment.
- **(D) Generate the manifests in Rust — CHOSEN.** The backend has exactly two write mechanisms (kube-rs `Patch::Apply` and `reqwest` against the OpenSearch/Dashboards REST APIs); this stays inside both, pins images we choose, and makes per-deployment substitution trivial.

**Day-2, not at creation.** The Create wizard gets a pointer only (one line in Review when the purpose is Observability). Reasons, in order of weight: (1) Create *cannot* execute it — Data Prepper authenticates immediately and Cortex/Alertmanager want PVCs, so it would have to route through ADR-018's deferred-apply anyway, making the day-2 executor a prerequisite of both designs; (2) purpose is create-time-final (ADR-030) and upgrades are one-way (ADR-048) — a third irreversible Create-time choice, backed by an upstream Alpha, is the wrong kind of debt; (3) the resource bill is only honest once the deployment is allocated (`/api/cluster_capacity`); (4) `discover()` cannot inform it, because this is a choice about *how* to collect, not *what*.

**Coexistence is proven, not asserted.** Index sets are disjoint — `otel-v1-apm-span*` / `otel-v1-apm-service-map*` / `logs-otel-v1*` versus the recipe indices — and a unit test checks that against `profiles::log_patterns()` so a new recipe cannot silently collide.

**State lives in a dedicated CR annotation** `veloxsearch.ai/otel-stack`, **not** in `monitors`. Two reasons, both load-bearing: `recipes::recipe_index` falls through to the nginx index for an unknown id (so the stack would report nginx's doc count as its own), and `monitors` is **round-tripped by the Edit form** into a server-side apply — a stale browser tab would silently unset a marker whose 17 objects are still running, which is the ADR-048 `save_cluster`-clobbers-version bug (#110) in a new place. Like `set_monitor`, `set_otel_stack` is a merge patch on `metadata` alone and therefore cannot move a version.

**Install order is OpenSearch-first**, which is not cosmetic: an ISM `ism_template` only attaches to indices created *after* the policy exists (the rule `profiles.rs` documents), and Data Prepper creates its indices within seconds of connecting. Applying the workloads first would leave the first day of telemetry outside retention forever.

**One inventory.** `otel_stack::manifests()` is the single object list; install applies it in order, uninstall deletes it in reverse. There is no second, hand-maintained delete list, which is what makes ADR-039's clean-install ⇒ clean-uninstall property hold by construction. Left behind on purpose, and stated in the confirm dialog: the telemetry indices and the data on the deleted PVs — the same contract `recipes::disable` documents for logs.

**Security.** Credentials come only from `k8s::admin_creds`, and the Data Prepper pipeline — which carries the OpenSearch password — is a **Secret, never a ConfigMap** (a wart `agents.rs` has and this module deliberately does not inherit); a unit test asserts the password appears in no other object. The config-hash annotation hashes the password too, so a rotation actually rolls the pods (subPath mounts never update live). **Cortex and Alertmanager have no authentication at all** (`auth_enabled: false`), so the inventory ships a **NetworkPolicy** restricting 9090/9093/9114 to the app namespace and the stack's own pods, while leaving OTLP 4317/4318 open cluster-wide as the ingest surface. Verified live in both directions. TLS is stated rather than inherited: Collector→Data Prepper is plaintext gRPC inside `velox-agents`; Data Prepper→OpenSearch is HTTPS with verification off, because the operator's internal CA is trusted by nothing in this codebase yet (`recipes::http()` already does the same). Pinning that CA is follow-up work. RBAC: the `velox-agents` Role in **both** `deploy/k8s/veloxsearch.yaml` and `deploy/install.yaml` gains `services`, `secrets`, `persistentvolumeclaims` and `networkpolicies` — without it every apply 403s, and the `deploy` skill's rolling update does not re-apply RBAC.

**Status: IMPLEMENTED and verified end-to-end on the local k3s** (single node, k3s v1.36.2, OpenSearch 3.8.0, deployment `testev10-ns41`), 2026-08-15. `tests/otel_stack_check.py --install` is **15/15** through the real API: install → all five components ready → datasource registered → OTLP trace accepted → spans indexed → **log recipes still receiving** → uninstall with zero surviving objects and the recipes untouched.

**Verified live, in detail:**
- All five components reach Running; PVCs bind on `longhorn`.
- A synthetic two-service OTLP trace posted to `:4318` lands as **2 documents in `otel-v1-apm-span-000001`** with correct `serviceName`/`traceGroup`, and **2 documents in `otel-v1-apm-service-map`** carrying the `velox-frontend → velox-payments` edge.
- **295 metric names in Cortex** — 216 `elasticsearch_*` from the exporter, 30 `otelcol_*` — and a **PPL query through the registered datasource** returns `elasticsearch_cluster_health_active_shards = 54` for the cluster.
- The NetworkPolicy **refuses** an unlabelled pod on Cortex 9090 while **allowing** the same pod on Collector 4318.
- Uninstall converges to zero objects and zero Longhorn volumes. It does so *eventually*, not instantly — a pod runs out its termination grace period and its PVC holds `pvc-protection` until the pod releases the volume — so `uninstall` returns as soon as the deletes are issued rather than blocking a request on a 30s grace period, and the conformance check polls for convergence instead of measuring that grace period and calling it a leak.

**Three upstream premises were wrong and are corrected here, each found by running it:**
- `blocks_storage.tsdb.retention_period: 15d` — Go durations have no day unit; Cortex refuses to start. Now `360h`.
- `ruler.storage` — removed; a nested block fails startup with `field storage not found in type ruler.Config`. Now top-level `ruler_storage`.
- `service.telemetry.metrics.address` — removed in Collector 0.15x (`migration.MetricsConfigV030 has invalid keys: address`). Now an explicit OpenTelemetry-Go pull reader.

**Two risks the plan carried were resolved by measurement, not argument:**
- The datasource API needs **no** `plugins.query.datasources.encryption.masterkey`: create/read/delete all succeed on 3.8.0 with no `opensearch.yml` change, which keeps this feature off the `additionalConfig` write path and off a node roll. (A datasource reports a `resultIndex` but does not create it; async query execution would, and we never issue one.)
- The service-map index is **`otel-v1-apm-service-map`**, not v2. Data Prepper has a separate `otel_apm_service_map` index type writing v2 — the upstream chart selects it — but the `observabilityDashboards` plugin bundled with 3.8.0 registers the **v1** patterns, and the live run confirmed the v1 names. Picking v2 would render an empty service map.

**Images pinned:** `opensearchproject/data-prepper:2.16.0` (the official GA of the version upstream pinned as a personal-account RC), `otel/opentelemetry-collector-contrib:0.156.0`, `cortexproject/cortex:v1.18.1`, `prom/alertmanager:v0.27.0`, `prometheuscommunity/elasticsearch-exporter:v1.10.0`. Advertised cost ≈ **0.9 vCPU / 2.1 GiB requests + 22 GiB Longhorn per deployment**, asserted by a unit test to equal the summed manifest requests so the UI cannot understate it.

**Tests:** unit — DNS-safe and ≤63-char names for the longest legal deployment; install-set equals teardown-set; the OpenSearch-side created set equals `os_teardown()`; otel patterns disjoint from `profiles::log_patterns()`; two deployments' configs never reference each other; the password appears only in the Secret; the config hash changes when the password does; scrape jobs omitted when targets are absent (never the upstream `kube-system` literals); advertised cost equals summed requests; PVCs pin Longhorn; every component has a Service and a `Recreate` strategy (RWO volumes cannot be handed between two live pods). Live — `tests/otel_stack_check.py`.

**Not done, and named:** `traceId` in Fluent Bit output for log↔trace correlation; OTLP exposure outside the cluster (it would need an authenticating proxy — the collector ships no auth); Cortex HA/multi-tenancy; pinning the operator CA. **Also not yet verified:** the ADR-044 tenant-egress flow to `velox-agents` 4317/4318, which multi-tenant deployments will need before an application can ship OTLP under the default-deny policy.

### Rev. 2 (2026-08-15) — the screens

The paragraph above titled *"This installs no UI"* was **half wrong, in the half that mattered to a user**. The plugins do ship in the image the operator pulls — that part held — but in a default OpenSearch Dashboards 3.8.0 the surfaces those screens are built from are **disabled**, so the first install produced five green components, indexed spans, a live datasource, and no visible change anywhere in Dashboards. Reported from the product, not from a test.

The upstream chart runs the **same `opensearchproject/opensearch-dashboards:3.8.0` image we do**. The entire difference is its `opensearch_dashboards.yml`. So this revision:

1. **Turns on the Dashboards features**, as `spec.dashboards.additionalConfig` under its own field manager `veloxsearch-otel` (the `AUTH_FIELD_MANAGER`/`SNAPSHOT_FIELD_MANAGER` pattern — `additionalConfig` is a granular map, so this manager owns exactly its own keys and `create_cluster`'s apply cannot strip them). Seven keys, each observed booting on 3.8.0: `data_source.enabled`, `data_source.ssl.verificationMode`, `datasetManagement.enabled`, `explore.enabled`, `explore.discoverTraces.enabled`, `explore.discoverMetrics.enabled`, `observability.alertManager.enabled`. Two upstream keys are deliberately **not** copied: `workspace.enabled` (we keep tenants and the current navigation) and `query_enhancements.ppl.lint.enabled`, **which this build rejects at boot** — an unknown key is a fatal `InvalidConfigurationError` and exit 64, found by crash-looping a pod. A unit test pins both the required keys and the rejected one. ADR-048 holds: the patch touches `spec.dashboards.additionalConfig` only and can never move a version.
2. **Waits for the new config to be live** — and specifically *not* for pod readiness. The operator does hash the rendered `opensearch_dashboards.yml` into the pod template (`checksum/dashboards.yml`) and rolls the Dashboards pod itself, so no restart of ours is needed; it just does it on its own reconcile cadence, during which the OLD pod is perfectly ready. A `readyReplicas >= 1` wait therefore returns instantly and everything downstream gets written against an OSD that still rejects the new saved-object types. The probe asks Dashboards instead: `plugin:explore` appears in `/api/status` only once `explore.enabled` is set. A config that kills the boot leaves the old pod answering without that plugin, so the same probe times out — and install then **reverts the config** rather than leaving a deployment whose UI never returns. Uninstall removes the keys the same way.
3. **Creates the saved objects that were missing.** Three of them are load-bearing and none is optional decoration:
   - **Index-pattern attributes.** `signalType` and `schemaMappings` — which `recipes::ensure_index_pattern` cannot express, so the otel patterns are now written directly. Without them the Observability plugin does not adopt the datasets. Their `fields` list is also warmed via `_fields_for_wildcard`, because a pattern created through the raw saved-objects API lands with none and every panel referencing it then fails with *"Could not locate that index-pattern-field"*.
   - **The `APM-Config-*` correlation.** OSD 3.8.0's APM client looks up a `correlations` saved object by that prefix and reads the traces dataset, the service-map dataset and the Prometheus connection from its three references (confirmed by reading `createReferences`/`createEntities` in the shipped plugin bundle). Absent it, APM services and the service map have nothing to read. A `trace-to-logs` correlation is created alongside it.
   - **Three PromQL boards** — OpenSearch Cluster Health, Observability Pipeline Health, Kubernetes Cluster Health — as `explore` panels plus a `dashboard`, ids namespaced per deployment. `explore` is a real saved-object type in 3.8.0 *only once `explore.enabled` is set*, which is why step 1 precedes this one. The queries are upstream's, **retargeted at our metric names**: Data Prepper derives metric prefixes from pipeline names and ours are `entry-pipeline`/`raw-trace-pipeline`/`service-map-pipeline`/`otel-logs-pipeline`, so upstream's `otel_traces_pipeline_*` series do not exist here. Names taken from a live `/metrics/prometheus`.
4. **Registers the datasource through Dashboards**, not through OpenSearch. `POST {osd}/api/directquery/dataconnections` creates the engine-side entry **and** the OSD `data-connection` saved object; `PUT {os}/_plugins/_query/_datasources` creates only the former, which is invisible to the Metrics UI and unusable as a correlation reference. Idempotency has to reason about **two** objects that each survive without the other, and the first cut got it wrong in a way worth recording: treating the saved object as proof the pair existed meant a re-install short-circuited and never re-created the engine entry, so the panel reported no datasource while the boards rendered fine. The rule is now *both halves or neither* — anything short of both is torn down and registered again — and uninstall deletes the `data-connection` saved object explicitly, by lookup, since its id is a UUID Dashboards mints and cannot live in the static teardown list.
5. **Scrapes Data Prepper and Cortex.** The collector previously scraped only itself and the OpenSearch exporter, so every panel on the Pipeline Health board would have been empty. Data Prepper serves its registry at `/metrics/prometheus`, not the default path.

**Also fixed:** the three OTLP/Alertmanager fields in the Integrations panel rendered blank — `Copyable` takes `text` and the panel passed `value`. And the panel now says where the data shows up, with the deployment's Dashboards link: the first install read as "five green components and no visible change" partly because nothing on the screen pointed at OpenSearch Dashboards.

**Still true from rev. 1:** no OpenSearch *plugin* is installed and no image is overridden. What changed is the honest description — the feature enables shipped-but-disabled Dashboards features and creates the saved objects those features read.

### Rev. 6 (2026-08-17) — the two screens that failed, and which of them was ours

Two errors reported from a live deployment, one per screen. They have unrelated causes and only one is a product defect.

**Traces — `Could not locate that index-pattern-field (id: endTime)`. Ours.** The `velox-otel-spans` saved object had no `fields` attribute at all. OSD caches an index pattern's field list *on the saved object*; `AggConfig` resolves `endTime` against that cache, not against the mapping, so an absent list fails every aggregating screen. `endTime` was present in the index mapping throughout — the index was never the problem.

The warm code existed and was correct; its **timing** was wrong. It ran in the install path, and Data Prepper's sinks create `otel-v1-apm-span-*` only once they connect, which is after. `_fields_for_wildcard` therefore returned nothing on every fresh install, the warm skipped, and the pattern shipped fieldless — deterministically, not intermittently. The code's own comment said the next install would fill it in; nothing made that happen.

A second defect sat in the same function: the pattern was POSTed with `?overwrite=true` and the fields PUT afterwards. On a re-install the POST wiped a good field list before the warm had produced a replacement, so any failure in between left the pattern worse than it was found.

Both fixed together — resolve the fields first, retry until the sink has created the index (`FIELDS_WAIT_SECS`, 600s; it waits on a startup, not on the user sending telemetry), then write the pattern once with the fields included. Failure to resolve now logs at error rather than passing silently. Verified live: the three patterns cache 43 / 16 / 15 fields and `endTime` resolves.

**Services — `Error loading services, verify your configuration setup under APM settings`. Not ours.** The screen lists services from the Prometheus datasource, and Cortex held no `request` / `error` / `fault` / `latency_seconds_*` series. The rev. 5 topology fix was in place and correct; the traffic was the problem.

Every trace this investigation generated came from the conformance check, which derived trace ids from the clock: `f"{now*1000+k:032x}"` yields `0000000000000000000001a01200c338` — **ten leading zero bytes**. `otel_apm_service_map` drops those, silently. A/B on the live stack, one variable at a time, stock config:

| trace id | `recordsIn` | `recordsOut` |
|---|---|---|
| clock-derived (`0000…`) | 30 | 0 |
| constant high prefix `f0…`, same adjacency | 24 | +25 |
| random 128-bit | 24 | 20 |

The middle row is the one that matters: it holds numeric adjacency constant and changes only the leading bytes. Adjacency is not the trigger; leading zero bytes are. A real SDK's id hits that shape with probability 2⁻⁸⁰, so this is a check defect with no production exposure — but it is the fourth time this check has produced a confident "the product is broken" verdict against a working pipeline. The check now uses `os.urandom(16)` and re-reads the clock per trace (a burst stamped with one timestamp pushes later traces out of the window they should land in).

**The sharding hypothesis is refuted, again.** Rev. 5 retracted it as never demonstrated; the clustered-id evidence appeared to resurrect it mid-investigation and it was wrong to do so. Direct test: `workers: 1` on `service-map-pipeline` — which forces `processorsCreated == 1`, so shard 0 spans the whole key range — left `recordsOut` at 0 with clustered ids. The partitioning was never involved. The live hand-edit has been reverted and the shipped config carries no `workers` override, matching upstream.

**Standing correction to rev. 5's framing of the RED metrics as a test-only concern.** They are not: the Services screen is a shipped surface that fails outright without them. The conformance assertion stays a hard failure.

**A third check defect, found while verifying the above, and again mistaken for a product failure.** With the pipeline demonstrably working — `recordsOut` 90, 72 records into the Prometheus sink, the `request` series queryable in Cortex — the check still reported "no service-derived RED metrics reached Prometheus". Its probe pod could not reach Cortex at all: object #17 opens 9090 only to the app namespace and to pods of the same stack, and that allowance is keyed on the pod's IP, which k3s's NetworkPolicy controller programs a few seconds *after* the pod starts. Measured inside one probe pod: attempt 1 refused, attempts 2 onward 200. A one-shot curl at container start loses that race, exits before printing, and the empty stdout parses as "no series". The OTLP posts never hit this because the 4317/4318 rule carries no `from` clause and needs no pod membership. Fixed by labelling the probe and retrying inside the pod. The policy is correct and unchanged.

Full round trip green afterwards on local k3s: **30 passed, 0 failed**, including a fresh install caching 34 / 16 / 15 index-pattern fields — the defect at the top of this revision, verified through the code path rather than by hand.

### Rev. 10 (2026-08-18) — correcting rev. 8: the tenant header never worked at all

Rev. 8 attributed the dead `securitytenant` header to `workspace.enabled` "quietly winning". That attribution is wrong, and the correction matters because it changes what the tenant apparatus has been doing since it was written: nothing.

The measurement that exposed it was run on `versaotest-eamz` — **workspaces off**, `multitenancy_enabled: true` on the cluster, the tenant `velox-versaotest-eamz` present. Writing a saved object with `securitytenant: velox-versaotest-eamz` and reading it back with no header at all returns 200 both ways, and the Global index grows by one. Same result as the deployment *with* workspaces. Workspaces were never the variable.

The cause is in the shipped `securityDashboards` bundle, read out of the running pod:

```js
multitenancy: schema.object({
  enabled: schema.boolean({ defaultValue: false }),
```

`opensearch_security.multitenancy.enabled` defaults to **false** on the Dashboards side, and the saved-object client wrapper that routes objects into a tenant index is registered only under `config.multitenancy.enabled && config.multitenancy.enable_aggregation_view`. The operator-generated `opensearch_dashboards.yml` carries no `opensearch_security.*` key on any deployment we provision. The cluster-side `multitenancy_enabled: true` is the *other* half and cannot act alone.

So `recipes::ensure_tenant` has been creating a tenant nothing ever writes to, and every `securitytenant` header in `recipes.rs` and `integrations.rs` has been ignored — on every deployment, at every version, with or without workspaces. There has never been per-deployment saved-object isolation; the objects have always been in Global.

What this changes, and what it does not:

* **The rev. 8 gate stays.** `tenant_scope` returning `None` under workspaces is still right — it stops sending a header that means nothing and stops creating a tenant to match. The reasoning is simply broader than rev. 8 claimed.
* **`opensearch_security.multitenancy.enabled: false` stays**, but it is documentation rather than repair: it pins the default the docs require instead of relying on it, and it is what keeps a deployment correct if anyone ever turns the cluster half on.
* **The migration machinery will never fire on an existing deployment**, and that is now a prediction rather than a hope: `tenant_saved_objects` returns 0 because no tenant index was ever created. It stays for the case where a customer enabled Dashboards-side multi-tenancy themselves.
* **Turning the next-generation UI on carries no saved-object risk** on any deployment we have provisioned. The confirm dialog still states the scoping change, because it is true going forward.

Open, and deliberately not decided here: whether the tenant apparatus should be removed outright rather than merely bypassed. It is dead code that reads as a working isolation boundary, which is the kind of thing that gets trusted in a security review. Removing it touches ADR-025 and ADR-039 and deserves its own decision.

### Rev. 9 (2026-08-18) — the next-generation UI is its own choice

Rev. 8 keyed saved-object tenancy on "is the observability stack installed". That was the right shape with the wrong key, and a question about the UI exposed it: a user on 3.8 asked why Dashboards still looked old, and the answer was that nothing but the stack ever turns the new interface on.

**What the next-generation UI actually is, and what it is not.** Not `theme:version`. OSD 3.8's label map is `v7: "v7"`, `v8: "Next (preview)"`, `v9: "v9 (preview)"`, and `DEFAULT_THEME_VERSION = 'v8'` — the Next theme is already the default, and a deployment here was measured on `v9`, ahead of it. The new interface is the **navigation**: `workspace.enabled` plus `uiSettings.overrides.home:useNewHomePage`. Upgrading to 3.8 does not enable it; it is opt-in, and nothing in the product offered that opt-in.

**Decision: the new UI is a per-deployment setting, and tenancy keys on workspaces rather than on the stack.** `k8s::next_ui_config()` holds the three keys — `workspace.enabled`, `home:useNewHomePage` and `opensearch_security.multitenancy.enabled: false`, the last belonging here because it is a consequence of workspaces and not of this feature. `recipes::tenant_scope` now asks `k8s::workspaces_enabled`, read from the CR's own Dashboards config rather than from a marker, because the config is what the pod boots with.

The stack-keyed gate was not merely less general — it was wrong for a case that now exists. A deployment that turns the new UI on without ever installing the stack would have had workspaces *and* tenants both active: precisely the configuration upstream forbids, and the one rev. 8 set out to eliminate.

**Two field managers over one map.** `veloxsearch-ui` owns the UI keys, `veloxsearch-otel` the stack's. `additionalConfig` is a granular map, so server-side apply tracks ownership per key and uninstalling the stack cannot take the UI down with it. A unit test asserts the two key sets are disjoint, because a key claimed by both would belong to whichever applied last and the failure would be silent.

**Who asked matters.** The stack turns the UI on as its own requirement (`chosen: false`) — the Observability nav group only renders inside a workspace. The `veloxsearch.ai/next-ui` annotation records a *user's* choice, and uninstall only reverts the UI when it is absent. Without that distinction "on" is ambiguous and uninstall either strands a deployment on an interface nobody chose or takes away one somebody did.

**`defaultWorkspace` is no longer set, deliberately.** It is a global setting, so it sent every user of that Dashboards into our workspace whether or not that was where they were going, and it outlives the workspace it names: a deployment here was left pointing at `QreAgI` after that workspace was deleted, landing users on a dead id with no way back but advanced settings. Users land on the home page and pick, which is also where they can set it themselves. Uninstall still writes `null` to clear the setting on deployments that already carry one, and a test allows exactly that one use and no assignment.

### Rev. 8 (2026-08-18) — tenants give way to workspaces, gated on the stack

Upstream states it as a prerequisite: with the Security plugin installed, `opensearch_security.multitenancy.enabled` must be `false` when `workspace.enabled` is `true`. This shipped with workspaces on and multi-tenancy left at its default — the configuration the documentation names as conflicting.

**The conflict was already resolving itself, silently and against us.** Measured on a live 3.8.0 deployment with the stack installed: an object written with `securitytenant: velox-testev10-ns41` reads back with no header at all, and no `.kibana_<hash>_<tenant>` index is created — the Global index simply gains a document. `securityDashboards@3.8.0` is installed and the cluster reports `multitenancy_enabled: true`, so the header is not being *rejected*; `workspace.enabled` is quietly winning. Every `securitytenant` header in `recipes.rs` and `integrations.rs` has been a no-op on such a deployment.

**Decision: saved-object tenancy is keyed on the stack, not on the OpenSearch version.** A deployment without the stack keeps its tenant on any version; one with the stack uses workspaces and no tenant is created or named. `recipes::tenant_scope` is the single place that decides, and every call site asks it rather than calling `tenant_name` directly.

Version was the obvious gate and is the wrong one. Our config keys are written by `set_dashboards_otel_config`, which runs on stack install and is reverted on uninstall — so a 3.8 deployment *without* the stack still has multi-tenancy on and its tenants working. Gating tenant creation on "3.8" while leaving multi-tenancy on for that case would send a `securitytenant` header naming a tenant we had deliberately stopped creating. The stack gate keeps the two halves of one decision together, and it confines the blast radius to an explicit day-2 action with a confirm dialog rather than to everyone who upgrades.

**Both halves, or the two sides disagree.** `opensearch_security.multitenancy.enabled: false` joins the Dashboards config (now 10 keys), and `set_multitenancy` writes the cluster half through `PUT _plugins/_security/api/tenancy/config` — no securityconfig reload, no node restart, and symmetric: uninstall restores `true`. Verified live: 200, and the call preserves `default_tenant`, `private_tenant_enabled` and the rest of the document.

**Migration is conditional, and a copy.** `tenant_saved_objects` counts documents in tenant-shaped `.kibana` indices — Global's migration generations (`.kibana`, `.kibana_1`, `.kibana_2`, …) are a numeric suffix and are excluded, which a unit test pins in both directions because a false positive nags every install and a false negative strands a customer's dashboards. Zero, the expected case, costs nothing and says nothing. Above zero, install exports the tenant's objects and the deferred task imports them into the Observability workspace.

The ordering is forced and is the part that is easy to get wrong: the export **must** run before `set_multitenancy(false)`, because afterwards the tenant indices are still on disk but nothing resolves them — the export would come back empty and the data would look like it had never existed. The workspace it lands in does not exist until the Dashboards pod has rolled, so the ndjson is carried into the deferred task. Failure to import is non-fatal and loses nothing: the export is a copy, the tenant indices are untouched, and re-enabling multi-tenancy brings the originals back.

Not done here, and named rather than left implicit: including `.kibana*` in the ADR-047 snapshot taken before a stack install, which is the one precaution the upstream multi-tenancy documentation actually offers ("take a snapshot of all tenant indexes"). There is no official migration path from tenants to workspaces; this is ours.

### Rev. 7 (2026-08-17) — read against the processor's own README

Rev. 6 closed with a green check. Reading `data-prepper-plugins/otel-apm-service-map-processor` and upstream's `init-opensearch-dashboards.py` afterwards found three things that green run did not catch, two of them product defects.

**The check's trace payload was not a valid APM topology, and the green result was hollow.** It sent two spans and placed the CLIENT span under the *callee's* resource with no peer attributes. A CLIENT span is emitted by the CALLER, so that shape has no caller-side outbound span at all, and `remoteService` never resolves — which the processor drops (`!"unknown".equals(decoration.getRemoteService())`), as the README's "unknown remote services prevent event emission" says plainly. Only the unconditional SERVER-span branch survived. The tell was in the passing output all along: a check calling itself "two-service" reported `services: ['velox-check-frontend']` — one. Rewritten to the three spans real instrumentation emits (frontend SERVER, frontend CLIENT with `peer.service`/`server.address`, payments SERVER), and the assertion now requires *both* services rather than a non-empty result. Measured: 1 RED series → 3, plus a real `velox-apm-frontend → velox-apm-payments` edge in the service map, which the old payload never produced once.

**The service-map index pattern named a field no document has.** It shipped with `timeFieldName: "hashId"`. `hashId` belongs to the legacy `service_map_stateful` document shape; `otel_apm_service_map` writes `sourceNode` / `targetNode` / `sourceOperation` / `nodeConnectionHash` / `operationConnectionHash` / `timestamp` and no `hashId` at all. Upstream's init script uses `timestamp`, and so did this ADR's own plan — the code diverged from both. Fixed, with a test pinning all three time fields (`time` / `endTime` / `timestamp`) against upstream.

**Uninstall never deleted the service map data.** `delete_indices` issued `DELETE otel-v2-apm-service-map*`, but that name is an *alias*: `_resolve/index/otel-v2-apm-service-map*` returns zero indices and one alias pointing at the physical `otel-v1-apm-service-map`. The wildcard matched no index, the call reported success, and the data survived every uninstall — confirmed by an index whose creation date predated two full uninstall cycles run with `delete_indices: true`. The delete list now also carries `SERVICE_MAP_INDEX`; reads keep using the alias, which is upstream's name and what the index pattern and ISM template are written against.

**One earlier framing corrected.** Rev. 5 called chaining the service map behind `traces-raw-pipeline` "a deliberate departure, taken *for* fidelity rather than against it". The processor's README states it outright: the companion `otel_traces` processor must run upstream. The fix is what the documentation requires; it was upstream's own *example topology* that contradicted its processor's README, not us departing from it.

Also noted, not acted on: `window_duration` defaults to `60s` upstream and this stack sets `10s`.

### Rev. 5 (2026-08-17) — why the RED metrics were silent

Rev. 3 shipped the documented Data Prepper topology and rev. 4 was reported with the RED-metrics path "wired end to end but emitting nothing (170 spans in, 0 out, no error)". That is now diagnosed, and the cause is in the documented topology itself.

`OTelApmServiceMapProcessor.processSpan` in the 2.16.0 GA is:

```java
if (span.getServiceName() != null) { ... }   // no else branch
```

A span with no `serviceName` is dropped — **no log, no metric, no counter**. And `recordsIn` is incremented *before* the drop, so the processor reports healthy throughput while discarding everything.

The documented topology fans `otel-traces-pipeline` out to both `traces-raw-pipeline` and `service-map-pipeline`, which means the service map reads the **raw OTLP stream**. On that stream the flattened `serviceName` field does not exist — the event carries `resource` (with `service.name` nested inside) and nothing else. The field is derived by the `otel_traces` processor, which sits on the *other* branch. Confirmed by adding a throwaway sink that wrote the raw event to a scratch index and reading its keys: `resource`, `kind`, `name`, `parentSpanId`, `spanId`, … and no `serviceName`.

**Fix (a deliberate departure, taken *for* fidelity rather than against it):** chain the service map behind `traces-raw-pipeline` so it receives post-`otel_traces` spans. One line; a unit test now fails if it is ever pointed back at the raw stream. Measured on the live cluster, before → after:

| | before | after |
|---|---|---|
| `otel_apm_service_map` recordsOut | 0 | 168 |
| MapDB `spansDbCount` | 0 (always) | non-zero |
| records reaching the Prometheus sink | 0 | 144 |
| Prometheus series | none | 27 each of `request` / `error` / `fault` / `latency_seconds_*`, labelled `service` + `operation` |
| service map | 0 edges | 12 edges with both operation names, plus 34 nodes |

Upstream's chart pins `sgguruda62324/opensearch-data-prepper:2.16.0-SNAPSHOT-rc1` — a snapshot RC — rather than the GA we pin (ADR-049 rev. 1, option B). That is consistent with the raw stream carrying `serviceName` in their build, and it is why their topology works for them and not here.

**Four things asserted earlier in this investigation were wrong and are retracted:** that the `getIterator(processorsCreated.get(), thisProcessorId)` sharding caused the loss (a hypothesis, never demonstrated); that `getEventType() == "METRIC"` in our route was a bug (144 routed records prove it works); that `group_by_attributes` was implicated (removing it changed nothing — restored); and that a single trace was too little traffic (the traffic was never the problem).

**Three defects were in the conformance check, not the product**, and each produced a confident wrong conclusion: a doc-count assertion with no baseline passed while Data Prepper was in CrashLoopBackOff; a fixed `traceId` made Data Prepper overwrite the same two documents so the count never moved; and OTLP posts issued without checking their status code reported "nothing indexed" three times when the traffic had never arrived. The check now requires a *baseline increase*, generates ids per run, and verifies every post — which is the minimum an assertion of this shape needs to mean anything.

### Rev. 3 (2026-08-16) — the workspace, and published endpoints

Rev. 2 got the screens to exist. It did not get the *shape* the upstream playground has: no **Agent Monitoring** section, no **Application performance** section, a different home page. Reported by comparing against `observability.playground.opensearch.org` — which, its `osd-name: observability-stack-dashboards` header confirms, is the same chart on the same `opensearch-dashboards:3.8.0` image we run.

**Why those sections were missing, read out of the shipped bundle rather than guessed.** `src/plugins/agent_traces/target/public/agentTraces.plugin.js` registers its links with `core.chrome.navGroup.addNavLinksToGroup(DEFAULT_NAV_GROUPS.observability, [{title:"Traces", category: DEFAULT_APP_CATEGORIES.agentMonitoring}, {title:"Spans", …}])`. That nav **group** only renders inside a workspace whose use case is observability. Both categories (`agentMonitoring`, `applicationPerformance`) are present in `src/core/utils/default_app_categories.js` of our image — the apps were installed and simply never listed. Rev. 2 skipped `workspace.enabled` deliberately, to preserve our tenant model; that was the wrong trade, and it is reversed here.

So rev. 3 adds:

1. **`workspace.enabled: true`** and **`uiSettings.overrides.home:useNewHomePage: true`** to the Dashboards config — the second is what makes `/app/workspace_initial` the landing page, the way the playground opens.
2. **A per-deployment Observability workspace**, created with `features: ["use-case-observability"]`, and **every saved object written through `/w/{id}/api/...`**. Not cosmetic: verified live that objects created without the prefix report `total: 0` from inside the workspace — a workspace-enabled Dashboards would have shown an *empty* Observability section, which is worse than none. `_bulk_create` under the prefix assigns the workspace itself and **rejects** an explicit `workspaces` field (400, "definition for this key is missing"), so the prefix is the whole mechanism.
3. **Three UI settings**: `defaultWorkspace` (land in it instead of the picker), `observability:apmEnabled` — the plugin's own description: *"When enabled and the Discover Traces feature is active, APM Services and Topology Map pages are available in the navigation. Otherwise, Trace Analytics pages are shown as fallback."* — and `observability:alertManagerSelectedDatasources`, without which the Alerts page opens against nothing. Uninstall nulls all three and deletes the workspace, which takes its contents with it.

**Accepted trade, stated rather than buried:** with workspaces on and a default workspace set, the Fluent Bit recipes' dashboards — which live in the global space — are one workspace-switch away instead of on the landing page. The observability experience was the explicit ask; the recipes are still reachable through the workspace picker.

**Published endpoints.** The stack was reachable only from inside the cluster, which makes it useless to any application that is not in it. OTLP and Alertmanager now get real hosts (`<deployment>-otlp.<domain>`, `<deployment>-alerts.<domain>`) as Ingresses in `velox-agents`, with the platform's TLS certificate copied into that namespace (an Ingress may only name a Secret in its own namespace).

**Publishing them unauthenticated was not an option, so authentication moved into the components:** the collector's `basicauth` extension guards both OTLP receivers, and Alertmanager's own `--web.config.file` guards its UI. Deliberately **not** an ingress annotation — the check then holds regardless of which ingress controller is in front (or none), and it covers in-cluster traffic too. One machine credential per deployment (`velox` + a generated password), stored in the stack's Secret, **read back on re-apply** so a re-install never invalidates exporters somebody already configured, and revealed through its own endpoint rather than riding the 15-second status poll. **Cortex stays unpublished**: it has no authentication of any kind and its only reader is Dashboards, which already has a route. A unit test asserts all of this — both routes present, both configs carrying their auth, and no Cortex host.

Two configs became credential-bearing (each carries a bcrypt hash), so the collector config moved out of its ConfigMap into the stack Secret and the Alertmanager web config joined it. The inventory is now **16 objects, or 18 with routes**; uninstall deletes the two Ingresses by name regardless of the current access mode, since that is the one case where the inventory at uninstall time may not be the one installed.

**Panel rework.** The endpoints are the product, so they are organised by the question being asked — *send* (OTLP traces/logs/metrics, public and in-cluster, with the credential), *look* (workspace deep link, Dashboards, OpenSearch API), *alerts* — with component state compressed into a single chip row above them. The "the Kubernetes board needs kube-state-metrics" note was removed as asked.

**RBAC:** the `velox-agents` Role gains `ingresses` in both install manifests.

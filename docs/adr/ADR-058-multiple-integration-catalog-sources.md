# ADR-058 — Multiple integration catalog sources, each with its own trust

**Status:** proposed (amends ADR-039's single-registry transport and
`docs/integrations/signing.md` §4.4–4.5 for non-default sources; ADR-047 and
ADR-054 are unchanged for the default source)
**Date:** 2026-09-14

## Context

An install reads integrations from exactly one catalog, and trusts exactly one
signer. Every catalog path — the tab's read (`catalog::view`,
`src/catalog.rs:816`), `install` (`:843`), `uninstall` (`:933`), deferred
provisioning (`src/k8s.rs:3976`) and telemetry gating
(`src/telemetry.rs:318`) — builds `Registry::from_env()` (`src/catalog.rs:246`):
one `VELOX_REGISTRY_URL`, one `VELOX_REGISTRY_TOKEN`, one process-wide
`CACHE` (`:736`). `verify_package` looks the manifest's `key_id` up in one
compiled-in `KEYRING` (`:64-67`, lookup and the UNKNOWN KEY refusal at
`:546-560`).

That rules out private or authenticated catalogs (e.g. an organisation's
internal integrations, partner catalogs) in two ways, and the second is the one
that matters. Pointing `VELOX_REGISTRY_URL` at such a catalog loses the public
one. And even then, every package it serves must verify against
`velox-registry-2026` — so an organisation cannot publish its own dashboards
without the project's maintainers signing them. `signing.md` §4.4 anticipated
this: "if [third-party publishers] ever are, the keyring becomes multi-tenant".

This is **not** the plugin system `docs/ROADMAP.md:94` lists as not planned.
Packages stay data, the engine stays fixed (`src/integrations.rs:420`), the
interpolation set stays closed (`CLOSED_TOKENS`, `src/integrations.rs:30`), and
signature verification stays the security boundary. What changes is how many
signed catalogs one install reads, and whom it trusts for each.

"Data" still deserves a precise reading before trust is widened, because three
facts about today's engine make a signing key more than a content key:

- `agent.conf.tmpl` is rendered with `{os_user}`/`{os_password}` and applied as
  the Fluent Bit config of a collector in `velox-agents`
  (`src/agents.rs:22`, `:253`); a log tailer is a DaemonSet mounting every
  node's `/var/log` (`:286`). Nothing checks what that config's `[OUTPUT]`
  sections point at, or which Fluent Bit plugins it loads — and Fluent Bit
  ships plugins (`exec` input, `lua` filter) that execute commands or inline
  scripts. A trusted key can therefore ship a package that sends node logs and
  the deployment's admin credentials elsewhere, or runs commands in the
  collector. The shipped packages do none of this: they are frozen extractions
  of the in-binary templates (`src/registry_golden.rs:316`), which use only
  `tail`, `kubernetes_events`, `kubernetes`, `modify` and an `opensearch`
  output to `{os_host}` (`src/agents.rs:73-184`).
- Cluster-side names are bare: the ingest pipeline and the agent are named by
  `manifest.id`, the template by `manifest.index`, saved objects by ids inside
  the ndjson (`src/integrations.rs:444`, `:458`, `created_resources` at `:317`).
  Two catalogs can ship the same names.
- Uninstall re-fetches the package from the registry to learn its `teardown`
  set (`src/catalog.rs:938`); without a registry only in-binary recipes can be
  torn down (`:948`). A catalog that goes away would strand exactly the leaks
  ADR-039's clean uninstall removed.

## Decision

### 1. Sources are configuration

- A **source** is `{id, base_url, credential?, keys[]}`. `id` is a DNS label
  (the manifest-id pattern). `base_url` is `https://…` or `file://…` and keeps
  today's layout (`catalog.json`, `integrations/<id>/…`), so any existing
  registry checkout or mirror is already a valid source.
- The **default source** is built in, has id `velox`, and is exactly today's
  registry: `VELOX_REGISTRY_URL` (default `DEFAULT_REGISTRY_URL`),
  `VELOX_REGISTRY_TOKEN`, and the compiled-in `KEYRING`. It cannot be removed,
  and configuration cannot add keys to it or replace its keys — mirrors of the
  public catalog override its URL, as today, rather than being added as a
  second source.
- Additional sources live in one Secret, `veloxsearch-catalog-sources`, in the
  control plane's namespace. A Secret, not the `veloxsearch-env` ConfigMap,
  because it holds credentials (ADR-034) and because key approvals are
  trust-relevant state that deserves Secret-grade RBAC. An admin edits it
  through admin-only routes (`scope.require_admin()`), or declaratively with
  `kubectl apply`; the two are equivalent, since anyone who can write that
  Secret can already rewrite the control plane.
- Credentials are write-only, following the ADR-045 `SECRET_KEPT` contract
  (`src/auth_provider.rs:41`): the API never returns one, and echoing the
  placeholder back means "keep what is stored".
- The admin routes are behind `VELOX_CATALOG_SOURCES`, default `"0"`. With the
  flag off the Secret is not read, and every path behaves exactly as today.

### 2. Trust is per source; keys are added by configuration, with approval

- A package verifies **only** against keys pinned for the source that served
  it. `verify_package` takes the source's keyring instead of reading the
  global `KEYRING`. A package signed by source B's key but served from source A
  is an UNKNOWN KEY reject, even though the key is trusted elsewhere in the
  same install.
- A non-default source's key is supplied **by the admin, in configuration** —
  never fetched from the source, since a compromised source would then choose
  its own trust root. Each key entry is `{key_id, public_key, state}`, where
  `state` is `pending | approved | revoked`.
- Adding a key makes it `pending`. The UI shows its SHA-256 fingerprint (over
  the raw 32-byte key) and says in plain words what approving means: packages
  signed by this key can configure collectors that read node logs and hold the
  deployment's credentials (Context). Approval is a separate admin action, and
  it is recorded with actor and time. Only `approved` keys verify. Replacing
  a key's material resets it to `pending`.
- Key ids used by the compiled-in keyring (`velox-registry-*`) are reserved and
  refused on configured sources, and so is a configured key whose material
  equals a compiled-in key: provenance must never be ambiguous in the UI.
- **Rotation** per source follows the `keys/README.md` overlap procedure — add
  and approve the new key, re-sign, then remove the old one — without a core
  release. **Revocation** is setting `revoked`: it takes effect on the next
  config reload, and the UI marks installed integrations whose recorded
  `key_id` is revoked. For the default source nothing changes: rotation is
  still a release, and release cadence is still its revocation latency
  (`signing.md` §4.3).

### 3. Identity is `source/id`, with no silent shadowing

- Everywhere the core names a package — catalog rows, install records, API
  responses, errors — it names `source/id`. A bare `id` in an API request or in
  existing state means `velox/id`. That keeps `CatalogInstallReq`,
  `CatalogUninstallReq`, the `monitors` annotation and the wizard's monitor ids
  valid unchanged; the request types gain an optional `source`.
- Same-named packages from different sources are **separate rows**. Neither
  replaces the other, and the bootstrap-floor merge in `build_view`
  (`src/catalog.rs:772`) applies to the default source only.
- Because cluster-side names are bare (Context), one deployment holds **at most
  one package per bare id**. Installing `acme/nginx` where `velox/nginx` is
  installed is refused with an error naming both. The install also refuses when
  the new package's `created_resources` overlap with any other installed
  package's recorded teardown set, so two different ids cannot claim the same
  index template or saved object either. Moving an integration between sources
  is an explicit *replace*: uninstall (§4), then install.
- The Integrations tab shows each row's source. `CatalogItem` gains `source`,
  plus `source_state` (§6). An installed integration shows the source it came
  from, including one that has since been removed.
- The existing `CatalogView.source` field (`Registry | Cache | Bootstrap`,
  `src/catalog.rs:124`) describes *freshness*, not provenance. It keeps its
  meaning for the default source, so today's frontend reads it unchanged.
  Per-source freshness moves to a new `sources[]` array.

### 4. Install records make uninstall independent of the source

- A successful install writes an **install record** for `source/id`: version,
  `key_id`, the package digest, and everything uninstall needs — the verified
  `teardown` block, the saved-object id→type map (`uninstall_package`,
  `src/integrations.rs:504`) and the agent name. The records are small (no
  assets) and live in one per-deployment Secret, `<name>-integrations`, next to
  the deployment's admin credential Secret. It is added to `owned_secret_names`
  (`src/k8s.rs:1549`), so it has exactly that protection and is deleted with
  the deployment.
- The `veloxsearch.ai/integration-versions` annotation stays (the tab's
  version display reads it). A sibling `veloxsearch.ai/integration-sources`
  annotation (`id=source`, absent ⇒ `velox`) follows the same
  sibling-not-new-encoding rule the versions annotation set
  (`src/k8s.rs:80-83`).
- **Uninstall** acts on the install record. It does not need the source to be
  reachable, configured, or to still trust the key. The record is trusted
  because of *where* it lives and *when* it was written: only after a
  successful verification, in a Secret whose writers can already read the
  deployment's admin credentials. Uninstall still deletes only inside that
  deployment's OpenSearch, and only the recorded set.
- An installation with no record (every install made before this ADR) falls
  back to today's path: fetch from the default source and verify against the
  compiled-in key, else the in-binary recipe (`src/catalog.rs:938-956`). The
  next successful install or upgrade of that id writes its record.
- **Upgrade** is a re-install from the *same* source and verifies against that
  source's current keys. Once a source is removed its integrations keep
  running, uninstall keeps working, and the tab says "source removed — no
  updates". Removing a source never touches deployments.

### 5. Visibility is a label, not a control

- The manifest gains an optional `visibility: public | restricted` (absent ⇒
  `public`), and `catalog.json` entries carry the same field for listing. The
  UI labels `restricted` packages "requires access to <source>".
- The core **enforces nothing** with it. Access control belongs to the source,
  and the core only forwards that source's credential. `catalog.json` is
  unsigned, so the field must never gate an install, a verification or a
  teardown. The signed manifest copy exists so a package cannot be relabelled
  after signing. Provenance, by contrast, is trustworthy without signatures,
  because the core knows which source it fetched from.
- Compatibility: the core parses manifests with serde, which ignores unknown
  fields (`parse_manifest`, `src/integrations.rs:123`), so older cores install
  a package that carries the field. The field is covered by the digest like any
  other. `manifest.schema.json` gains the optional property, and
  `schema_version` stays `"1.0"`. The registry's validator (which enforces
  `additionalProperties: false`) must accept it before any package uses it.

### 6. Degradation is per source

- Each source has its own cache and its own ADR-047 ladder: fresh → stale
  cache → (default source only) the bootstrap floor. A source's state is one of
  `ok | stale | unreachable | unauthorized | untrusted` (no `approved` key), and
  is reported per source with the verbatim error. It never becomes a failed
  request.
- Sources are fetched concurrently, each under `HTTP_TIMEOUT`
  (`src/catalog.rs:58`), so a slow source adds at most one timeout to the tab,
  not one per source.
- Install from a degraded non-default source is refused with that source's
  state. Integrations already installed from it are unaffected: their data is
  applied and their collectors run, and uninstall works (§4). An expired or
  revoked credential on source A changes nothing for source B.

### 7. Transport rules for user-supplied sources

- `https://` only for configured sources, and no URL userinfo. Redirects are
  not followed. The credential is sent only to the source's own origin, with
  the header shape the source declares (`bearer` or `private-token`). Today's
  default source sends both headers (`src/catalog.rs:276`) and follows reqwest's
  default redirect policy. It keeps that behaviour for compatibility, with one
  exception: a credential must not survive a cross-origin redirect. reqwest
  strips standard auth headers on such a redirect, but a custom
  `PRIVATE-TOKEN` header is not among them; the implementation confirms this
  with a test and fixes it for the default source too.
- **SSRF.** The control plane resolves a configured source's host and refuses
  loopback, link-local (including cloud metadata `169.254.169.254`) and
  unspecified addresses at connect time, so DNS rebinding cannot bypass the
  check. Private RFC 1918 ranges stay allowed, because internal catalogs are the
  point. Error text shown to the UI never includes response bodies.
- **`file://`** sources cannot be created through the API. They are declared in
  the Secret by whoever manages the install manifest, who already controls what
  is mounted into the pod. The transport keeps reading only `catalog.json` and
  `integrations/<valid>/<valid>` under the root (`valid_component`,
  `src/catalog.rs:389`). The air-gapped route — the default source pointed at a
  `file://` checkout (`docs/DEPLOY.md:135`) — is unchanged.
- A pre-save probe, like ADR-045's, fetches `catalog.json` and verifies one
  package against the pending key. That lets an admin see that the fingerprint
  matches what the catalog actually signs before approving it.

### 8. Collector configs are confined, for every source

The engine refuses, before any write, an `agent.conf.tmpl` that:

- loads a Fluent Bit plugin outside an allowlist taken from the in-binary
  templates (inputs `tail` and `kubernetes_events`; filters `kubernetes` and
  `modify`; output `opensearch`), or
- has an `[OUTPUT]` whose `Host` is anything but `{os_host}`.

This applies to the default source too. Every shipped package already
complies, because the golden gates hold them byte-equal to those templates.
Widening the allowlist is a core change with a test, like `CLOSED_TOKENS`. This
is what keeps "packages are data" true once someone other than the maintainers
holds a trusted key.

### 9. Tests and gates

- The `registry_golden` gates keep their scope: the default source's packages
  against the in-binary recipes, from `VELOX_REGISTRY_PATH`. They gain one
  check, that every shipped `agent.conf.tmpl` passes §8.
- Multi-source behaviour is tested in `catalog` without network or CI secrets,
  using the existing `throwaway_key()`/`toy_package` helpers
  (`src/catalog.rs:1190-1218`). Two `file://` temp sources, signed by two
  throwaway keys, cover:
  - side-by-side listing with provenance;
  - the cross-source refusal (B-signed package served from A ⇒ UNKNOWN KEY);
  - the same bare id in both sources listing as two rows, with the second
    install refused;
  - uninstall from an install record after the source is removed;
  - the `pending` key refusal.
- The credential cases (401 ⇒ `unauthorized`, B unaffected; no cross-origin
  redirect carries the token) use a loopback HTTP fixture on tokio's
  `TcpListener`. The address policy (§7) is injectable for that one test, so no
  new dev-dependency is needed.

### 10. Migration

- No action for existing installs. With `VELOX_CATALOG_SOURCES` unset and no
  sources Secret, the process has one source — `velox`, built from the same two
  env vars and the same keyring — and every request, response field and
  annotation it relied on keeps its meaning. Integrations installed before
  this ADR are `velox/<id>` and uninstall through the fallback in §4.
- No step is a breaking change under GOVERNANCE. The package format change
  (§5) is additive and older cores ignore it; the API and annotation changes
  are additive. Each implementation PR still adds a `CHANGELOG.md` entry. The
  release that enables the flag carries a migration note covering: what a
  source is, key approval, the `file://` restriction (§7), and the §8
  confinement. §8 is a *tightening*: a hand-built package with a non-allowlisted
  plugin, served from a private mirror of the default source, would stop
  installing. The note says so explicitly, even though no published package is
  affected.

## Consequences

- An organisation can run its own signed catalog next to the public one, sign
  with its own key, and publish without the project's maintainers in the loop.
  Community publishers no longer need a core release to be trusted.
- The admin who approves a key becomes a trust root for every deployment in
  that install. This is a real supply-chain surface the project did not have:
  an approved-but-compromised key can ship any package the engine accepts. §8
  bounds what such a package can do, and the fingerprint-and-approval step
  makes the decision explicit and auditable. What it cannot do is make the
  decision a good one.
- Revocation for configured sources becomes config-fast. For the default source
  it stays release-fast. Two latencies exist, and the docs must say which
  applies where.
- The catalog module grows from one registry to a set: per-source caches, a
  config reader, install records, an address policy. `Registry::from_env` stops
  being the entry point; the implementation must keep the trust code
  (`verify_package`, `package_digest`) as small and adjacent as it is today.
- One package per bare id per deployment is a real restriction: two catalogs
  cannot both provide an `nginx` to the same deployment. It is the honest
  consequence of bare cluster-side names, and it fails loudly rather than
  overwriting.
- Install records duplicate a little of what the package already says. That
  duplication is the price of uninstall not depending on a network, a
  credential or a key that may be gone.
- Foreclosed: fetching keys from a source; configuration adding keys to the
  default source; visibility as enforcement; silent cross-source replacement;
  `file://` sources created through the API.

## Alternatives considered

- **New keys only through a core release** (`signing.md` §2 option A extended:
  every trusted key compiled in). Lost. It is the stronger trust story — every
  key passes the project's review gate — but it makes the project's maintainers
  the gatekeepers of every organisation's private catalog. Each publisher
  would have to put a public key into this repository and wait for a release,
  and every rotation would become a release for all installs. That is the
  limitation this ADR exists to remove. It stays the rule for the default
  source.
- **Build-time keyring extension** (organisations build their own binary with
  extra keys). Lost: it forks the artifact, and the published image's cosign
  provenance (ADR-054) no longer describes what runs.
- **Keys fetched from the source** (`keys/*.pub` next to `catalog.json`,
  trust-on-first-use). Lost: whoever controls the source controls its trust
  root, which turns a transport compromise into a signing compromise.
- **Sigstore/cosign keyless for third-party publishers** (`signing.md` §4.4's
  suggestion). Not chosen now, for the reason `signing.md` §2 option B gives:
  verification must work with no egress. A source's `keys[]` could later
  accept an identity-based entry without changing §1–§7.
- **One global keyring, any trusted key verifies any source.** Lost: a key
  approved for an internal catalog could then sign a package served from a
  compromised mirror of the public one. Per-source pinning is what makes "which
  source served it" a security property rather than a label.
- **Namespace cluster-side names by source** (e.g. pipeline `acme--nginx`).
  Lost: index names and saved-object ids are author-chosen content inside
  signed assets, and dashboards reference them. Rewriting them means the engine
  editing signed data, and changes the `{recipe_id}` contract.
- **Uninstall re-verifies against retained keys of removed sources.** Lost: it
  keeps revoked or removed keys alive in the trust path just to delete things.
  The install record gives the same guarantee without that.
- **Visibility enforced by the core** (hide or refuse `restricted` packages
  without a credential). Lost: the core cannot know what a credential entitles
  its holder to, and `catalog.json` is unsigned. The source already enforces
  access by refusing the fetch.

## Implementation outline

Each step is one reviewable PR, in order. Nothing before step 5 changes
behaviour.

1. **Keyring as a value.** `verify_package` takes a `&Keyring`, and the
   compiled-in `KEYRING` becomes the default source's keyring. Pure refactor;
   the existing verification tests pass unchanged.
2. **Collector confinement (§8)**, in the engine and applied to all packages,
   plus the `registry_golden` check that shipped packages comply. CHANGELOG
   entry (tightening).
3. **Credential redirect fix (§7)** for the default source, with the loopback
   fixture test.
4. **Source model and per-source cache.** A `Source` type and a `Sources` set
   holding only `velox` from env; a per-source `CatalogCache`; `sources[]` in
   `CatalogView`; `source` on `CatalogItem`. Additive API; the frontend shows
   provenance.
5. **Install records (§4)**: the `<name>-integrations` Secret in
   `owned_secret_names`, the `integration-sources` annotation, uninstall from
   record with today's fallback, and the one-package-per-bare-id and overlap
   refusals.
6. **Configured sources behind `VELOX_CATALOG_SOURCES`**: the Secret reader,
   the address policy, `https`-only and no redirects for configured sources,
   and per-source states. Covered by the two-`file://`-sources tests (§9).
7. **Admin routes and UI**: add, edit and remove sources; write-only
   credentials; key fingerprint, the approval action and its record; revocation;
   the pre-save probe. Every new route goes through DTO + handler + `routes()` +
   `frontend/api.jsx`.
8. **Manifest `visibility` (§5)**: the schema property, `CatalogEntry`/UI
   label, and the registry validator change (in the registry repository).
9. **Docs and release**: `signing.md` §4 (per-source trust, two revocation
   latencies), `keys/README.md`, `docs/SECRETS.md`, `SECURITY.md`,
   `docs/DEPLOY.md` (+ `.pt-BR.md` where they exist), and the migration note.
   Then flip this ADR to accepted.

## Open questions for review

- Should key approval require a *second* admin where more than one exists? The
  proposal records a single admin's approval, because single-admin installs are
  common.
- Is `velox` the right reserved id for the default source, given that its URL
  may point at a mirror?
- §8's allowlist is deliberately the current templates. Should it be widened
  now (e.g. `parser`, `grep`, `nest` filters) for packages the maintainers
  already expect, or only on demand?

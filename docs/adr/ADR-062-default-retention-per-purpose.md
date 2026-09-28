# ADR-062 — Default retention per purpose: an installation default, stamped per deployment, applied once

**Status:** accepted (requested by the operator, 2026-09-25)
**Date:** 2026-09-26

## Context

Every non-search deployment gets the `velox-retention` ISM policy after it
settles (ADR-028, applied through `provisioning::plan()` as `Item::Profile`,
before any monitor — ADR-052). The retention age was hardcoded in
`profiles::apply`: `observability` 30 days, `security` 90 days, `search` none.

Two problems followed from that:

1. **The age could not be chosen.** Not per installation and not per
   deployment. The purpose cards stated "30 days" / "90 days" as fixed facts.
2. **velox clobbered the user.** Every save and every retry re-PUT the policy
   unconditionally. A user who changed retention inside OpenSearch Dashboards
   had that change silently reverted the next time anyone edited the
   deployment's memory.

The operator's requirement: velox holds a default retention for new and
existing deployments. The person can always change it inside the cluster, and
a new deployment uses the default.

## Decision

### 1. The installation default lives in the `veloxsearch-config` ConfigMap

It sits beside the access settings (ADR-027), which is where installation-wide
settings already live. The keys are `retention_observability_days` and
`retention_security_days`, and the out-of-box values are 30 and 90, exactly the
old hardcoded ones. An installation that never touches the knob changes
nothing.

- **Not Postgres.** Postgres is control-plane identity only: users, tenants,
  quotas, audit. The access config is the precedent for operator settings,
  and a ConfigMap can be restored or GitOps-applied like it.
- **A field manager of its own (`veloxsearch-retention`).** `access::set`
  server-side-applies the same ConfigMap under `veloxsearch` with only its own
  keys. Sharing that manager would make each writer prune the other's keys.
- A missing or garbled key reads as the built-in value, never as an error.
- Admin-only writes (`/save_retention_defaults`). The value is readable by any
  signed-in account (`/retention_defaults`) because the create wizard shows it.
  It names no deployment and holds no secret.

`search` has no default and no knob. Its data is kept forever (ADR-028), and
the policy only ever covered the recipe log indices, which a search
deployment does not have.

### 2. Each deployment carries its own value on its CR

Per-deployment configuration lives on the CR (ADR-041). At create, the chosen
value is stamped as `veloxsearch.ai/retention: <n>d`: the wizard's override, or
else the installation default for the purpose. Next to it,
`veloxsearch.ai/retention-source` records where the value came from:
`default` (it inherits the installation default) or `override` (a person chose
it for this deployment). The deferred applier reads that
annotation and never reads the installation default. So:

- **Changing the default never moves an existing deployment.** A deployment
  without the annotation (created before this ADR) resolves to the built-in
  30/90, which is what it already runs. It does not resolve to whatever the
  admin set since.
- **A save preserves the value.** `create_cluster` server-side-applies the
  whole annotations map under one field manager, so the value is carried
  through from the existing CR. Omitting it would prune it, which is the same
  trap as the `additionalConfig` round-trip.
- **A purpose change re-stamps the new purpose's default** as `default`. The
  user picked that purpose, and its default is what they chose.

### 3. The policy is applied once, then it is the user's

This follows the `Item::DashboardsDefault` rule: velox provides the starting
point, and the user owns it afterwards. After every write of the policy, velox
records the document's `_seq_no:_primary_term` on the CR as
`veloxsearch.ai/retention-stamp`. On the next pass, `ensure_retention` does the
following:

| Live policy | Stamp | Action |
|---|---|---|
| absent | – | create → `installed` |
| present, `_seq_no:_primary_term` == stamp | velox's own | rewrite only if the age or the covered patterns differ → `updated` / `unchanged` |
| present, pair differs | someone else wrote it | **leave alone** → `customized` |

- The pair is exact, because every write to the document moves `_seq_no`.
  Nothing depends on normalizing what OpenSearch echoes back (retry defaults,
  `schema_version`, and so on).
- An update is conditional (`if_seq_no`/`if_primary_term`), so a user edit
  that lands between velox's read and its write is refused, not overwritten.
- **No stamp** means the policy was written before this ADR, or a stamp write
  failed. velox then falls back to a stated heuristic: velox's description
  names the age it wrote ("VeloxSearch purpose-profile retention (30d): …").
  A policy whose description is not velox's, or whose delete age disagrees
  with it, is customized. An edit that keeps both (for example, only the
  rollover size) is not detectable here. velox overwrites that policy once and
  then stamps it, which is what every save did before this ADR.
- The edit tab shows the state (`managed` / `customized` / `absent`) from a
  live read (`/retention_status`).

### 4. Existing deployments move only on an explicit action, and only if they inherit

"velox defines the default; the person can always change it" covers two kinds
of change: an edit to the policy inside OpenSearch (§3), and a value chosen for
the deployment at create. Neither kind is undone by the default.

- **Admin: "apply default to existing deployments"**
  (`/apply_default_retention`). It moves only deployments whose source is
  `default`. The action runs sequentially and returns one row per deployment:
  `installed` / `updated` / `unchanged` / `override` (skipped, chosen for this
  deployment) / `customized` (skipped, edited in OpenSearch) / `search`
  (skipped) / `error` (with the cluster's words). Skipped rows report the days
  the deployment keeps, not the default it did not get, and their CR is left
  untouched. There is no background mass rewrite.
- **Per deployment: "restore default"** (`/reset_retention`, tenant-scoped).
  This is the one path that overwrites a customized policy or an override. It
  forces the installation default for the purpose and re-stamps the value with
  source `default`, so the deployment follows the default again from then on.
- **CRs without a source annotation** (created before the field existed). No
  retention value at all (pre-ADR-062) inherits, since it runs the built-in.
  A value equal to a default velox would have stamped (the built-in 30/90, or
  the installation default in force now) inherits. Any other value is an
  override. The CR does not record which default was in force at create, so a
  deployment created on a default the admin has since changed reads as an
  override. That errs on the safe side: it is skipped, not rewritten, and
  "restore default" adopts it.

Saving the default applies nothing by itself.

### 5. When a change takes effect

A changed policy reaches existing indices on OpenSearch's next ISM job run,
which is minutes, not the moment velox writes it. A policy customized inside
OpenSearch governs only the indices OpenSearch itself attaches it to (its
`ism_template` for new indices, or an explicit change-policy); velox no longer
re-attaches indices to a policy it does not own. The Edit tab says so in one
line.

## Consequences

- The purpose cards, the wizard and the review step show the effective days.
  The wizard accepts a per-deployment override (1–3650 days).
- The knob is the retention age per purpose and nothing more. There are no
  new policy states and no per-pattern editor (YAGNI). The lifecycle is still
  `hot → snapshot → delete`.
- `the_profile_is_planned_before_any_monitor` is unchanged. The ISM policy is
  still installed before the indices that it auto-attaches to.
- **Known limit:** anything that rewrites the policy document other than a
  user also reads as "customized". One example would be an OpenSearch upgrade
  that migrates ISM policy documents, if one ever does. The effect is
  conservative, because velox stops touching the policy, and restoring the
  default re-adopts it. This has not been observed on a live cluster.

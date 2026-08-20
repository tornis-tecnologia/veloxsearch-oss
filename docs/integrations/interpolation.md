# Integration interpolation contract

**Status:** frozen by #71 · **Governs:** every asset file in every package · **Source of truth for the closed set:** ADR-039

An integration package is **data with holes**. The only values the core substitutes
into a package's asset files are drawn from a **closed, enumerated set the engine
owns**. A package may reference these tokens and **nothing else**. This is the
property that makes "no code ships from the registry" safe: a package cannot name a
value the core did not author, so it cannot express a shell command, a URL to fetch,
or a code path — only a template hole the engine fills with a value it computed
(ADR-039, "Templating is a closed, enumerated variable set").

## The closed variable set

Exactly these eight tokens. Written `{name}`. Case-sensitive. No others exist.

| Token            | Value the engine substitutes                                   | Grounded in (current core)                                             |
|------------------|----------------------------------------------------------------|-----------------------------------------------------------------------|
| `{deployment}`   | Target OpenSearch deployment name (DNS-safe, ADR-020)          | `deployment` arg threaded through `recipes::apply` / `deploy_agent`   |
| `{ns}`           | The app's Kubernetes namespace                                 | `k8s::ns()`                                                           |
| `{os_host}`      | OpenSearch service host, `{deployment}.{ns}.svc`               | `agents::fluent_bit_conf` `host`; `recipes::os_base`                  |
| `{os_user}`      | Deployment's OpenSearch admin username                         | `k8s::admin_creds(deployment).0`                                      |
| `{os_password}`  | Deployment's OpenSearch admin password                         | `k8s::admin_creds(deployment).1`                                      |
| `{index}`        | The OpenSearch index this package writes to                    | `recipes::recipe_index(recipe)` / manifest `index`                   |
| `{tenant}`       | Dashboards multitenancy tenant, `velox-{deployment}`           | `recipes::tenant_name(deployment)`                                    |
| `{recipe_id}`    | The package id                                                 | manifest `id` / the `recipe` arg                                      |

These reconcile 1:1 with the set named in ADR-039:
`{deployment}`, `{index}`, `{os_user}`, `{os_password}`, `{os_host}`, `{ns}`,
`{tenant}`, `{recipe_id}`.

### Where each token is used today

- **`agent_config`** (`agent.conf.tmpl`) — `{os_host}` (Host), `{os_user}`
  (HTTP_User), `{os_password}` (HTTP_Passwd), `{index}` (Index). This is where
  `fluent_bit_conf` interpolates today.
- **`pipeline` / `index_template`** — need no interpolation in the nginx recipe
  (they are static JSON PUT under the fixed ids `{recipe_id}` / `{index}`). The
  ids themselves are engine-owned, so a package that parameterizes them uses
  `{recipe_id}` / `{index}` rather than hard-coding.
- **`saved_objects`** — the index-pattern title is `{index}*`; the objects are
  written into tenant `{tenant}` via the `securitytenant` header (engine-set, not
  a body hole). `{os_user}`/`{os_password}` are used only as the request's basic
  auth by the engine — a package never embeds credentials in an asset body.

> **Security note.** `{os_user}` / `{os_password}` exist so the *engine* can
> authenticate to the customer's cluster. They are legal tokens because refusing
> them would just push packages to invent their own auth. A package SHOULD only
> place them where credentials legitimately go (e.g. Fluent Bit `HTTP_Passwd`);
> the reviewer of a registry PR treats any other use as a red flag. The core does
> not distinguish — it only enforces that no *foreign* token appears.

## The rule

1. The set above is **closed**. Adding a token is a **core change** (this doc +
   the engine's substitution table + a test), reviewed and released like any code
   — never something a package can introduce.
2. On load, after signature verification, the engine scans every asset file for
   `{...}` tokens. **Any token not in the closed set ⇒ the package is rejected**
   (fail closed, before anything is applied to the cluster).
3. Literal braces in an asset (if ever needed) are escaped `{{` / `}}`. An
   unescaped `{` that does not open a known token is a validation error, not a
   silent pass-through.

## Test obligation (for #72–#74, stated here so it is not forgotten)

- A unit test enumerates the closed set and asserts the engine's substitution
  table has exactly these keys — no more, no fewer.
- A golden test feeds an asset containing a foreign token (`{cluster_admin_ssh}`)
  and asserts the loader **rejects** the package.
- A round-trip test renders each shipped package's `agent_config` and asserts the
  output is byte-equivalent to `fluent_bit_conf`'s current output for that recipe.

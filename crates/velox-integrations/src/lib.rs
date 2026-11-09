// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The integration-package contract (ADR-039), shared by the core and by Velox
//! Mesh (ADR-068).
//!
//! An integration is **data with holes**: a `manifest.yaml` plus a handful of
//! asset files, signed in the registry. This crate is everything both consumers
//! must agree on, and nothing either one does alone:
//!
//! * the parsed [`Manifest`] and the loaded [`Package`];
//! * the **closed interpolation set** ([`CLOSED_TOKENS`]) and the resolved
//!   [`Vars`] for one deployment × package;
//! * [`render`], which substitutes the closed set and **fails closed** on any
//!   token a package tried to invent (`docs/integrations/interpolation.md`);
//! * resource accounting ([`created_resources`] / [`teardown_resources`]), the
//!   set equality that proves a clean install is a clean uninstall.
//!
//! Deliberately pure: no Kubernetes, no HTTP, no crypto. The core
//! (`src/integrations.rs`) performs the OpenSearch writes and the agent
//! deploy; `velox-policy` assembles rendered fragments into a per-policy
//! configuration. Signature verification (`catalog::verify_package`) sits in
//! front of both and is the whole security boundary — a `Package` here is
//! already verified. Extending the token set is a change to this crate, its
//! test and the spec, never something a package or a policy can introduce.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The closed, engine-owned interpolation variable set (`docs/integrations/
/// interpolation.md`, ADR-039). Exactly these eight names may appear as
/// `{name}` in an asset; any other `{lower_case_token}` is a hard error.
pub const CLOSED_TOKENS: [&str; 8] = [
    "deployment",
    "index",
    "os_user",
    "os_password",
    "os_host",
    "ns",
    "tenant",
    "recipe_id",
];

/// What this crate can refuse. Both consumers wrap it in their own error type
/// (`anyhow` in the core, `CompileError` in `velox-policy`); the messages are
/// the ones the tests and the registry reviewers read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A `{name}` outside the closed set — a package tried to invent a hole.
    #[error(
        "foreign interpolation token {{{name}}} is not in the closed variable set (allowed: {allowed})"
    )]
    ForeignToken { name: String, allowed: String },
    #[error("parsing integration manifest.yaml: {0}")]
    Manifest(#[source] serde_yaml::Error),
    #[error("saved-objects.ndjson line {line}: {reason}")]
    SavedObjects { line: usize, reason: String },
    /// A schema 1.1 variable the package or the policy got wrong.
    #[error("variable `{name}`: {reason}")]
    Variable { name: String, reason: String },
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// Manifest (the subset the engine consumes — see manifest.schema.json for the
// full validated contract, enforced before a package reaches here).
// ---------------------------------------------------------------------------

/// The parsed `manifest.yaml`. Only the fields the apply-engine reads are
/// modeled; serde_yaml ignores the rest (title/summary/signature/discovery),
/// which the schema validator has already checked.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Package id — also the ingest-pipeline id PUT and the `{recipe_id}` value.
    pub id: String,
    /// The OpenSearch index the package writes to — the `{index}` value and the
    /// index-template / index-pattern id.
    pub index: String,
    /// Saved-object id of the out-of-the-box dashboard (informational here).
    pub dashboard: String,
    /// Asset filenames by role.
    pub assets: Assets,
    /// The explicit inventory uninstall removes.
    pub teardown: Teardown,
    /// Schema 1.1 parameters; empty for every 1.0 package.
    #[serde(default)]
    pub variables: Vec<Variable>,
}

/// Asset filenames shipped in the package directory. `pipeline`/`index_template`
/// are absent for packages that need neither (e.g. kubernetes: dynamic mapping).
#[derive(Debug, Clone, Deserialize)]
pub struct Assets {
    #[serde(default)]
    pub pipeline: Option<String>,
    #[serde(default)]
    pub index_template: Option<String>,
    pub saved_objects: String,
    pub agent_config: String,
    /// Schema 1.1: OpenTelemetry Collector template for the host engine. Only
    /// Velox Agent runs it; the core digests and verifies it like any asset.
    #[serde(default)]
    pub otel_config: Option<String>,
}

/// How a policy value for a [`Variable`] is validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VarType {
    String,
    Integer,
    Boolean,
}

/// One package parameter (schema 1.1). Referenced in the templates as
/// `${name}`; **not** an interpolation token (`docs/integrations/
/// interpolation.md`, "Package variables are not tokens"). `description` is
/// for the UI and is not modeled here.
#[derive(Debug, Clone, Deserialize)]
pub struct Variable {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: VarType,
    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
    #[serde(default)]
    pub required: bool,
}

/// Everything a package creates in the customer's OpenSearch, so uninstall
/// removes exactly these (ADR-039 "Uninstall becomes real"). Also constructed
/// in-binary by the core's `recipes::recipe_teardown` so the built-in
/// `disable()` reuses the same teardown path.
#[derive(Debug, Clone, Deserialize)]
pub struct Teardown {
    /// Ingest-pipeline id to DELETE. Present iff `assets.pipeline` is.
    #[serde(default)]
    pub ingest_pipeline: Option<String>,
    /// Index-template id to DELETE. Present iff `assets.index_template` is.
    #[serde(default)]
    pub index_template: Option<String>,
    /// Index-pattern saved-object id to DELETE (equals `index` today).
    pub index_pattern: String,
    /// Dashboard + visualization saved-object ids to DELETE.
    pub saved_objects: Vec<String>,
    /// Extra index-pattern ids beyond `index_pattern`. A recipe creates exactly
    /// one; the OTel stack (ADR-053) creates three, and reuses this same
    /// teardown machinery rather than growing a parallel one.
    #[serde(default)]
    pub index_patterns: Vec<String>,
    /// ISM policy ids to detach (`_ism/remove`) and DELETE.
    #[serde(default)]
    pub ism_policies: Vec<String>,
    /// SQL-plugin datasource names to DELETE (`_plugins/_query/_datasources`).
    #[serde(default)]
    pub datasources: Vec<String>,
}

/// A verified, parsed package: the manifest plus the raw bytes of each asset it
/// names. Construction (registry pull / disk load / signature check) happens
/// upstream; the engine only consumes it.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    /// Raw `pipeline.json` (None when the package ships none).
    pub pipeline: Option<String>,
    /// Raw `index-template.json` (None when the package ships none).
    pub index_template: Option<String>,
    /// Raw `saved-objects.ndjson` (index-pattern + dashboard + visualizations).
    pub saved_objects: String,
    /// Raw `agent.conf.tmpl` (Fluent Bit config template).
    pub agent_config: String,
    /// Raw `otel.yaml.tmpl` (schema 1.1; None when the package ships none).
    pub otel_config: Option<String>,
}

/// Parse a `manifest.yaml` into the fields the engine reads.
pub fn parse_manifest(yaml: &str) -> Result<Manifest> {
    serde_yaml::from_str(yaml).map_err(Error::Manifest)
}

impl Package {
    /// Load a package from a directory: `manifest.yaml` plus each asset file it
    /// names. Used by the engine's tests against the nginx fixture and by the
    /// registry/offline loaders.
    pub fn load_from_dir(dir: &Path) -> Result<Package> {
        let read = |name: &str, what: String| -> Result<String> {
            std::fs::read_to_string(dir.join(name)).map_err(|source| Error::Io { what, source })
        };
        let manifest_yaml = read(
            "manifest.yaml",
            format!("reading {}/manifest.yaml", dir.display()),
        )?;
        let manifest = parse_manifest(&manifest_yaml)?;
        let asset = |name: &str| read(name, format!("reading asset '{name}'"));
        let pipeline = match &manifest.assets.pipeline {
            Some(f) => Some(asset(f)?),
            None => None,
        };
        let index_template = match &manifest.assets.index_template {
            Some(f) => Some(asset(f)?),
            None => None,
        };
        let saved_objects = asset(&manifest.assets.saved_objects)?;
        let agent_config = asset(&manifest.assets.agent_config)?;
        let otel_config = match &manifest.assets.otel_config {
            Some(f) => Some(asset(f)?),
            None => None,
        };
        Ok(Package {
            manifest,
            pipeline,
            index_template,
            saved_objects,
            agent_config,
            otel_config,
        })
    }
}

// ---------------------------------------------------------------------------
// Package variables (schema 1.1) — declared holes, `${name}`, not tokens.
// ---------------------------------------------------------------------------

/// Resolve a package's declared variables against the values a policy set
/// (`given`; empty on the core's DaemonSet path), falling back to `default`.
/// Fails closed on: a name the manifest does not declare, a `required`
/// variable with neither value nor default, a value or default of the wrong
/// type, a multi-line value (it would splice a Fluent Bit directive), and a
/// variable named after a closed token (the two syntaxes are one character
/// apart). Returns `name → rendered value`, ready for [`apply_variables`].
pub fn resolve_variables(
    manifest: &Manifest,
    given: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let declared: BTreeMap<&str, &Variable> = manifest
        .variables
        .iter()
        .map(|v| (v.name.as_str(), v))
        .collect();
    if let Some(name) = given.keys().find(|k| !declared.contains_key(k.as_str())) {
        return Err(Error::Variable {
            name: name.clone(),
            reason: "not declared by the package manifest".into(),
        });
    }
    let mut out = BTreeMap::new();
    for v in &manifest.variables {
        if CLOSED_TOKENS.contains(&v.name.as_str()) {
            return Err(Error::Variable {
                name: v.name.clone(),
                reason: "shadows a closed interpolation token".into(),
            });
        }
        let value = match (given.get(&v.name), &v.default) {
            (Some(s), _) => check_type(&v.name, v.kind, s)?,
            (None, Some(d)) => check_type(&v.name, v.kind, &yaml_scalar(&v.name, d)?)?,
            (None, None) if v.required => {
                return Err(Error::Variable {
                    name: v.name.clone(),
                    reason: "required and no value or default".into(),
                })
            }
            (None, None) => continue,
        };
        out.insert(v.name.clone(), value);
    }
    Ok(out)
}

fn yaml_scalar(name: &str, v: &serde_yaml::Value) -> Result<String> {
    match v {
        serde_yaml::Value::String(s) => Ok(s.clone()),
        serde_yaml::Value::Number(n) => Ok(n.to_string()),
        serde_yaml::Value::Bool(b) => Ok(b.to_string()),
        _ => Err(Error::Variable {
            name: name.into(),
            reason: "default is not a scalar".into(),
        }),
    }
}

fn check_type(name: &str, kind: VarType, s: &str) -> Result<String> {
    let bad = |reason: &str| Error::Variable {
        name: name.into(),
        reason: reason.into(),
    };
    if s.chars().any(char::is_control) {
        return Err(bad("value contains a control character or newline"));
    }
    match kind {
        VarType::String => {}
        VarType::Integer => {
            s.parse::<i64>()
                .map_err(|_| bad("value is not an integer"))?;
        }
        VarType::Boolean => {
            if s != "true" && s != "false" {
                return Err(bad("value is not `true` or `false`"));
            }
        }
    }
    Ok(s.to_string())
}

/// Substitute `${name}` for each resolved variable, and only those: any other
/// `${...}` (an environment reference such as `${VELOX_OS_PASSWORD}`) passes
/// through untouched. Run **before** [`render`]; the syntaxes do not overlap
/// because a variable may not shadow a closed token.
pub fn apply_variables(template: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = template.to_string();
    for (name, value) in vars {
        out = out.replace(&format!("${{{name}}}"), value);
    }
    out
}

// ---------------------------------------------------------------------------
// Interpolation — the closed variable set (fail closed on foreign tokens).
// ---------------------------------------------------------------------------

/// The Dashboards multitenancy tenant a deployment's objects live in. One
/// formula, used by the `{tenant}` token here and by the core's recipe path
/// (`recipes::tenant_name`), so the two never disagree.
pub fn tenant_name(deployment: &str) -> String {
    format!("velox-{deployment}")
}

/// Resolved values for the closed variable set, for one deployment × package.
/// Built from primitives so it is unit-testable without a cluster; the core
/// derives the primitives from its `Deployment` handle, the Mesh from a policy
/// target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vars {
    deployment: String,
    index: String,
    os_user: String,
    os_password: String,
    os_host: String,
    ns: String,
    tenant: String,
    recipe_id: String,
}

impl Vars {
    /// Build the variable set. `os_host` and `tenant` are derived exactly as the
    /// core's live plumbing derives them (`{deployment}.{ns}.svc` and
    /// [`tenant_name`]), so a rendered asset matches what the built-in recipes
    /// produce. A collector outside the cluster cannot reach the in-cluster
    /// service name: give it [`Vars::with_os_host`].
    pub fn new(
        deployment: &str,
        namespace: &str,
        index: &str,
        recipe_id: &str,
        os_user: &str,
        os_password: &str,
    ) -> Vars {
        Vars {
            os_host: format!("{deployment}.{namespace}.svc"),
            tenant: tenant_name(deployment),
            deployment: deployment.to_string(),
            ns: namespace.to_string(),
            index: index.to_string(),
            recipe_id: recipe_id.to_string(),
            os_user: os_user.to_string(),
            os_password: os_password.to_string(),
        }
    }

    /// Override the derived `{os_host}` — the published endpoint a host agent
    /// outside the cluster ships to (veloxagent/docs/ARCHITECTURE.md §5.1).
    pub fn with_os_host(mut self, os_host: &str) -> Vars {
        self.os_host = os_host.to_string();
        self
    }

    /// The value a token resolves to, or `None` outside the closed set.
    pub fn get(&self, name: &str) -> Option<&str> {
        Some(match name {
            "deployment" => &self.deployment,
            "index" => &self.index,
            "os_user" => &self.os_user,
            "os_password" => &self.os_password,
            "os_host" => &self.os_host,
            "ns" => &self.ns,
            "tenant" => &self.tenant,
            "recipe_id" => &self.recipe_id,
            _ => return None,
        })
    }
}

/// Substitute the closed variable set into an asset, **failing closed** on any
/// foreign token. A *token* is `{` + a lowercase-initial identifier
/// (`[a-z][A-Za-z0-9_]*`) + `}`:
///   - a known name (CLOSED_TOKENS) → its value;
///   - any other lowercase-initial name → `Err` (a package cannot invent a hole).
///
/// Everything else passes through untouched: grok syntax (`%{COMBINEDAPACHELOG}`,
/// `%{POSINT:pid}` — uppercase / colon, not a token) and JSON structural braces
/// (`{"type":…}` — `{` not followed by a bare identifier). `{{` escapes a literal
/// `{` (so `{{index}` yields the text `{index}` without substitution); a `}` is
/// always literal. Escaping is deliberately asymmetric — a `}}` escape would be
/// impossible to distinguish from two adjacent JSON closing braces (`{}}`), and
/// a lone `}` is never ambiguous because only a full `{name}` is a token. This
/// lets one scanner validate JSON, pipeline and Fluent-Bit assets without
/// choking on their non-template braces.
pub fn render(template: &str, vars: &Vars) -> Result<String> {
    let chars: Vec<char> = template.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '{' if i + 1 < n && chars[i + 1] == '{' => {
                out.push('{');
                i += 2;
            }
            '{' => {
                // Read a candidate token name: identifier chars up to a `}`.
                let start = i + 1;
                let mut j = start;
                while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                if j < n && j > start && chars[j] == '}' {
                    let name: String = chars[start..j].iter().collect();
                    if let Some(v) = vars.get(&name) {
                        out.push_str(v);
                        i = j + 1;
                        continue;
                    }
                    // A lowercase-initial `{name}` that isn't known is a template
                    // hole the package tried to invent — reject (fail closed).
                    // Uppercase-initial (grok `%{DATA}`) is not a token attempt.
                    if name.starts_with(|ch: char| ch.is_ascii_lowercase()) {
                        return Err(Error::ForeignToken {
                            name,
                            allowed: CLOSED_TOKENS.join(", "),
                        });
                    }
                }
                out.push('{');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Resource accounting — proves clean-install ⇒ clean-uninstall as set equality.
// ---------------------------------------------------------------------------

/// A class of OpenSearch object an integration creates. Paired with an id, this
/// keys the "what a package installs" set against "what its teardown removes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKind {
    IngestPipeline,
    IndexTemplate,
    IndexPattern,
    SavedObject,
    /// ISM retention policy (ADR-053; recipes never create one — the purpose
    /// profile owns `velox-retention`, which is not a per-package object).
    IsmPolicy,
    /// SQL-plugin datasource (ADR-053).
    Datasource,
}

impl ResourceKind {
    /// A stable name for the wire and the UI. Spelled out rather than derived
    /// from `Debug`, because a rename of the variant would otherwise change an
    /// API response and a screen's labels silently.
    pub fn name(self) -> &'static str {
        match self {
            ResourceKind::IngestPipeline => "IngestPipeline",
            ResourceKind::IndexTemplate => "IndexTemplate",
            ResourceKind::IndexPattern => "IndexPattern",
            ResourceKind::SavedObject => "SavedObject",
            ResourceKind::IsmPolicy => "IsmPolicy",
            ResourceKind::Datasource => "Datasource",
        }
    }
}

/// `(kind, id)` — one installed / torn-down object.
pub type ResourceKey = (ResourceKind, String);

/// Everything applying `pkg` creates in the customer's OpenSearch, derived from
/// the package itself (assets present + the ids inside `saved-objects.ndjson`).
pub fn created_resources(pkg: &Package) -> Result<BTreeSet<ResourceKey>> {
    let mut set = BTreeSet::new();
    if pkg.pipeline.is_some() {
        set.insert((ResourceKind::IngestPipeline, pkg.manifest.id.clone()));
    }
    if pkg.index_template.is_some() {
        set.insert((ResourceKind::IndexTemplate, pkg.manifest.index.clone()));
    }
    for o in parse_saved_objects(&pkg.saved_objects)? {
        if o.kind == "index-pattern" {
            set.insert((ResourceKind::IndexPattern, o.id));
        } else {
            set.insert((ResourceKind::SavedObject, o.id));
        }
    }
    Ok(set)
}

/// Everything the manifest's `teardown` will remove.
pub fn teardown_resources(t: &Teardown) -> BTreeSet<ResourceKey> {
    let mut set = BTreeSet::new();
    if let Some(id) = &t.ingest_pipeline {
        set.insert((ResourceKind::IngestPipeline, id.clone()));
    }
    if let Some(id) = &t.index_template {
        set.insert((ResourceKind::IndexTemplate, id.clone()));
    }
    set.insert((ResourceKind::IndexPattern, t.index_pattern.clone()));
    for id in &t.index_patterns {
        set.insert((ResourceKind::IndexPattern, id.clone()));
    }
    for id in &t.saved_objects {
        set.insert((ResourceKind::SavedObject, id.clone()));
    }
    for id in &t.ism_policies {
        set.insert((ResourceKind::IsmPolicy, id.clone()));
    }
    for id in &t.datasources {
        set.insert((ResourceKind::Datasource, id.clone()));
    }
    set
}

/// One saved object's type + id (from an ndjson line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedObj {
    pub kind: String,
    pub id: String,
}

/// Parse `saved-objects.ndjson` into `(type, id)` pairs (one JSON object per
/// non-blank line). Used both to account for created objects and to recover the
/// object *type* the Dashboards DELETE API needs at teardown.
pub fn parse_saved_objects(ndjson: &str) -> Result<Vec<SavedObj>> {
    let mut out = Vec::new();
    for (n, line) in ndjson.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line).map_err(|e| Error::SavedObjects {
            line: n + 1,
            reason: format!("invalid JSON: {e}"),
        })?;
        let field = |key: &str| -> Result<String> {
            v.get(key)
                .and_then(|x| x.as_str())
                .map(str::to_string)
                .ok_or_else(|| Error::SavedObjects {
                    line: n + 1,
                    reason: format!("missing \"{key}\""),
                })
        };
        out.push(SavedObj {
            kind: field("type")?,
            id: field("id")?,
        });
    }
    Ok(out)
}

/// Parse `saved-objects.ndjson` into the JSON array `_bulk_create` expects.
pub fn saved_objects_bulk(ndjson: &str) -> Result<Vec<serde_json::Value>> {
    let mut out = Vec::new();
    for (n, line) in ndjson.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line).map_err(|e| Error::SavedObjects {
            line: n + 1,
            reason: format!("invalid JSON: {e}"),
        })?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nginx_vars() -> Vars {
        Vars::new(
            "logs-ab12",
            "veloxsearch-test",
            "nginx-logs",
            "nginx",
            "admin",
            "s3cr3t-pw",
        )
    }

    /// The substitution table has EXACTLY the closed set — no more, no fewer —
    /// and every name resolves (interpolation.md test obligation).
    #[test]
    fn closed_token_table_matches_contract() {
        let expected: BTreeSet<&str> = [
            "deployment",
            "index",
            "os_user",
            "os_password",
            "os_host",
            "ns",
            "tenant",
            "recipe_id",
        ]
        .into_iter()
        .collect();
        let actual: BTreeSet<&str> = CLOSED_TOKENS.into_iter().collect();
        assert_eq!(actual, expected, "CLOSED_TOKENS drifted from the contract");

        let v = nginx_vars();
        for name in CLOSED_TOKENS {
            assert!(v.get(name).is_some(), "no value resolves for {{{name}}}");
        }
        // Derived tokens computed exactly as the live plumbing does.
        assert_eq!(v.get("os_host"), Some("logs-ab12.veloxsearch-test.svc"));
        assert_eq!(v.get("tenant"), Some("velox-logs-ab12"));
        assert_eq!(v.get("cluster_admin_ssh"), None);
    }

    /// Every closed token substitutes to its value.
    #[test]
    fn render_substitutes_closed_tokens() {
        let v = nginx_vars();
        let t = "d={deployment} ns={ns} host={os_host} u={os_user} p={os_password} \
                 i={index} t={tenant} r={recipe_id}";
        let got = render(t, &v).unwrap();
        assert_eq!(
            got,
            "d=logs-ab12 ns=veloxsearch-test host=logs-ab12.veloxsearch-test.svc \
             u=admin p=s3cr3t-pw i=nginx-logs t=velox-logs-ab12 r=nginx"
        );
        assert!(!got.contains('{'), "unrendered token remained: {got}");
    }

    /// A package referencing a token outside the closed set is REJECTED, not
    /// silently passed through (the interpolation.md golden token).
    #[test]
    fn render_rejects_foreign_token() {
        let v = nginx_vars();
        let err = render("HTTP_Passwd {cluster_admin_ssh}", &v).unwrap_err();
        assert!(matches!(&err, Error::ForeignToken { name, .. } if name == "cluster_admin_ssh"));
        let msg = err.to_string();
        assert!(msg.contains("cluster_admin_ssh"), "unhelpful error: {msg}");
        assert!(
            msg.contains("closed variable set"),
            "unhelpful error: {msg}"
        );
        assert!(
            msg.contains("recipe_id"),
            "the allowed set is listed: {msg}"
        );
    }

    /// Grok syntax and JSON structural braces are NOT tokens — they pass through
    /// untouched (so pipeline/template/ndjson assets survive the scan).
    #[test]
    fn render_preserves_grok_and_json_braces() {
        let v = nginx_vars();
        let grok = r#"{ "grok": { "patterns": ["%{COMBINEDAPACHELOG}", "%{POSINT:pid}"] } }"#;
        assert_eq!(render(grok, &v).unwrap(), grok);
        let json = r#"{"type":"visualization","id":"velox-nginx-reqs","attributes":{}}"#;
        assert_eq!(render(json, &v).unwrap(), json);
    }

    /// `{{` escapes a literal `{`, so a brace-word can be emitted verbatim
    /// WITHOUT substitution and WITHOUT tripping the foreign-token guard.
    #[test]
    fn render_escapes_open_brace() {
        let v = nginx_vars();
        assert_eq!(render("a {{index} b", &v).unwrap(), "a {index} b");
        assert_eq!(
            render("literal {{cluster_admin_ssh} here", &v).unwrap(),
            "literal {cluster_admin_ssh} here"
        );
    }

    /// A host agent outside the cluster ships to a published endpoint, not to
    /// the in-cluster service name; everything else stays derived.
    #[test]
    fn os_host_can_be_overridden_for_off_cluster_agents() {
        let v = nginx_vars().with_os_host("logs.example.internal");
        assert_eq!(v.get("os_host"), Some("logs.example.internal"));
        assert_eq!(v.get("tenant"), Some("velox-logs-ab12"));
        assert_eq!(
            render("Host {os_host}", &v).unwrap(),
            "Host logs.example.internal"
        );
    }

    fn package(saved_objects: &str) -> Package {
        Package {
            manifest: Manifest {
                id: "demo".into(),
                index: "demo-logs".into(),
                dashboard: "velox-demo".into(),
                assets: Assets {
                    pipeline: Some("pipeline.json".into()),
                    index_template: None,
                    saved_objects: "saved-objects.ndjson".into(),
                    agent_config: "agent.conf.tmpl".into(),
                    otel_config: None,
                },
                variables: vec![],
                teardown: Teardown {
                    ingest_pipeline: Some("demo".into()),
                    index_template: None,
                    index_pattern: "demo-logs".into(),
                    saved_objects: vec!["velox-demo".into()],
                    index_patterns: vec![],
                    ism_policies: vec![],
                    datasources: vec![],
                },
            },
            pipeline: Some("{}".into()),
            index_template: None,
            saved_objects: saved_objects.into(),
            agent_config: "[OUTPUT]\n    Index {index}\n".into(),
            otel_config: None,
        }
    }

    fn with_variables(yaml_vars: &str) -> Manifest {
        parse_manifest(&format!(
            "id: demo\nindex: demo-logs\ndashboard: d\nassets:\n  saved_objects: s\n  agent_config: a\nteardown:\n  index_pattern: demo-logs\n  saved_objects: [d]\nvariables:\n{yaml_vars}"
        ))
        .unwrap()
    }

    /// Defaults fill what the policy did not set; a policy value wins; the
    /// result substitutes `${{name}}` and nothing else.
    #[test]
    fn variables_resolve_from_policy_then_default_and_apply_as_dollar_braces() {
        let m = with_variables(
            "  - {name: path, type: string, description: {en: p}, default: /var/log/app.log}\n  - {name: port, type: integer, description: {en: p}, default: 8080}\n  - {name: tls, type: boolean, description: {en: t}, default: false}\n",
        );
        let given = BTreeMap::from([("port".to_string(), "9090".to_string())]);
        let r = resolve_variables(&m, &given).unwrap();
        assert_eq!(r["path"], "/var/log/app.log");
        assert_eq!(r["port"], "9090");
        assert_eq!(r["tls"], "false");
        let t = "Path ${path}\n    Port ${port} tls=${tls} pw=${VELOX_OS_PASSWORD} {index}";
        assert_eq!(
            apply_variables(t, &r),
            "Path /var/log/app.log\n    Port 9090 tls=false pw=${VELOX_OS_PASSWORD} {index}"
        );
    }

    /// The five refusals, each naming the variable.
    #[test]
    fn variables_fail_closed() {
        let m = with_variables(
            "  - {name: path, type: string, description: {en: p}, required: true}\n",
        );
        let err = resolve_variables(&m, &BTreeMap::new()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "variable `path`: required and no value or default"
        );

        let given = BTreeMap::from([("other".to_string(), "x".to_string())]);
        let err = resolve_variables(&m, &given).unwrap_err();
        assert!(err
            .to_string()
            .starts_with("variable `other`: not declared"));

        let given = BTreeMap::from([("path".to_string(), "a\nb".to_string())]);
        assert!(resolve_variables(&m, &given)
            .unwrap_err()
            .to_string()
            .contains("control character"));

        let m = with_variables(
            "  - {name: port, type: integer, description: {en: p}, default: eighty}\n",
        );
        assert!(resolve_variables(&m, &BTreeMap::new())
            .unwrap_err()
            .to_string()
            .contains("not an integer"));

        let m =
            with_variables("  - {name: index, type: string, description: {en: p}, default: x}\n");
        assert!(resolve_variables(&m, &BTreeMap::new())
            .unwrap_err()
            .to_string()
            .contains("shadows a closed"));

        // No variables declared, none given: the 1.0 case is a no-op.
        let m = parse_manifest("id: a\nindex: b\ndashboard: c\nassets:\n  saved_objects: s\n  agent_config: a\nteardown:\n  index_pattern: b\n  saved_objects: [c]\n").unwrap();
        assert!(m.variables.is_empty());
        assert!(resolve_variables(&m, &BTreeMap::new()).unwrap().is_empty());
    }

    /// Install set == teardown set for a well-formed package, and the
    /// index-pattern is accounted under its own kind.
    #[test]
    fn created_and_teardown_sets_agree_for_a_consistent_package() {
        let pkg = package(
            "{\"type\":\"index-pattern\",\"id\":\"demo-logs\"}\n\n{\"type\":\"dashboard\",\"id\":\"velox-demo\"}\n",
        );
        let created = created_resources(&pkg).unwrap();
        assert_eq!(created, teardown_resources(&pkg.manifest.teardown));
        assert_eq!(created.len(), 3);
        assert!(created.contains(&(ResourceKind::IndexPattern, "demo-logs".into())));
    }

    /// A teardown that forgets an object shows up as set inequality — the
    /// property the registry golden gates assert per package.
    #[test]
    fn a_forgotten_teardown_entry_is_visible_as_an_orphan() {
        let pkg = package(
            "{\"type\":\"index-pattern\",\"id\":\"demo-logs\"}\n{\"type\":\"dashboard\",\"id\":\"velox-demo\"}\n{\"type\":\"visualization\",\"id\":\"velox-demo-v1\"}\n",
        );
        let created = created_resources(&pkg).unwrap();
        let removed = teardown_resources(&pkg.manifest.teardown);
        let orphaned: Vec<_> = created.difference(&removed).collect();
        assert_eq!(
            orphaned,
            vec![&(ResourceKind::SavedObject, "velox-demo-v1".to_string())]
        );
    }

    /// ndjson errors name the line, and a line without `type`/`id` is refused.
    #[test]
    fn saved_objects_errors_name_the_line() {
        let err = parse_saved_objects("{\"type\":\"dashboard\",\"id\":\"a\"}\n{\"id\":\"b\"}\n")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "saved-objects.ndjson line 2: missing \"type\""
        );
        let err = saved_objects_bulk("{\"ok\":true}\nnot json\n").unwrap_err();
        assert!(err
            .to_string()
            .starts_with("saved-objects.ndjson line 2: invalid JSON"));
    }

    /// A manifest with the optional assets absent parses, and the serde
    /// defaults keep the teardown lists empty rather than failing.
    #[test]
    fn manifest_optional_fields_default() {
        let m = parse_manifest(
            "id: kubernetes\nindex: k8s-logs\ndashboard: velox-k8s\nassets:\n  saved_objects: s.ndjson\n  agent_config: a.tmpl\nteardown:\n  index_pattern: k8s-logs\n  saved_objects: [velox-k8s]\n",
        )
        .unwrap();
        assert_eq!(m.assets.pipeline, None);
        assert!(m.teardown.ism_policies.is_empty());
        let err = parse_manifest("id: [\n").unwrap_err();
        assert!(matches!(err, Error::Manifest(_)));
    }

    /// The wire names are fixed: a variant rename must not change an API
    /// response or a screen's labels without the test saying so.
    #[test]
    fn resource_kind_names_are_stable() {
        assert_eq!(ResourceKind::IngestPipeline.name(), "IngestPipeline");
        assert_eq!(ResourceKind::IndexTemplate.name(), "IndexTemplate");
        assert_eq!(ResourceKind::IndexPattern.name(), "IndexPattern");
        assert_eq!(ResourceKind::SavedObject.name(), "SavedObject");
        assert_eq!(ResourceKind::IsmPolicy.name(), "IsmPolicy");
        assert_eq!(ResourceKind::Datasource.name(), "Datasource");
    }
}

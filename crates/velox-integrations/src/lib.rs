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

// ---------------------------------------------------------------------------
// Artifact naming for the agent path
// ---------------------------------------------------------------------------

/// The prefix every artifact Velox Agent's path creates carries. One glance at
/// an index list says which side wrote it, and one pattern (`veloxagent-*`)
/// describes everything the collector account may touch.
pub const AGENT_ARTIFACT_PREFIX: &str = "veloxagent";

/// The signal words an index name may end in. A closed list on purpose: the
/// alternative is deriving the signal by stripping the package id, which
/// breaks the moment an id and its index do not share a prefix
/// (`k8s-events`/`k8s-events`), and would put an empty segment in the middle
/// of a name.
const SIGNALS: [&str; 5] = ["logs", "metrics", "traces", "events", "audit"];

/// The signal an index name denotes — its last `-` segment when that is a word
/// we know, and `logs` otherwise, because every integration shipped so far is
/// a log source and a wrong guess here is a name nobody can read.
pub fn signal_of(index: &str) -> &'static str {
    let last = index.rsplit('-').next().unwrap_or("");
    SIGNALS
        .iter()
        .copied()
        .find(|s| *s == last)
        .unwrap_or("logs")
}

/// The major of a semver-ish version string, as the `v1` segment of a name.
/// Anything unparseable is `1`: a name is not the place to discover that a
/// manifest's version is malformed (the schema validator is), and `v0` would
/// silently claim a pre-release.
pub fn major_of(version: &str) -> u64 {
    version
        .split(['.', '-', '+'])
        .next()
        .and_then(|m| m.parse::<u64>().ok())
        .filter(|m| *m > 0)
        .unwrap_or(1)
}

/// The name **one** integration's artifacts share on the agent path:
/// `veloxagent-<integration>-<signal>-v<major>`, e.g.
/// `veloxagent-nginx-logs-v1`.
///
/// One name for the index, the index template, the ingest pipeline and the
/// index pattern. They live in different namespaces in OpenSearch, so the
/// single string cannot collide — and sharing it means "which template covers
/// this index" and "which pipeline fed it" are answerable by looking, not by
/// cross-referencing a table.
///
/// The version is in the name because an index template is not editable in
/// place for a mapping change that conflicts with what is already indexed:
/// shipping `nginx` 2.0 with a different mapping writes
/// `veloxagent-nginx-logs-v2` and leaves v1 to retention, instead of a
/// deployment where half the documents disagree with the template.
///
/// Deliberately NOT applied to the in-cluster DaemonSet path, whose indices
/// (`nginx-logs`, …) are named by the packages themselves and already carry
/// years of data and dashboards. The prefix marks the agent's artifacts; it
/// does not rename anyone else's.
pub fn agent_artifact_name(package_id: &str, index: &str, version: &str) -> String {
    format!(
        "{AGENT_ARTIFACT_PREFIX}-{package_id}-{}-v{}",
        signal_of(index),
        major_of(version)
    )
}

/// Every id this package's artifacts take on the agent path, keyed by the id
/// the package itself declares.
///
/// Renaming is not cosmetic here: both paths can be active on the same
/// deployment, and the packages' saved objects carry fixed ids
/// (`velox-nginx-overview`, …). Installing an integration twice under one set
/// of ids means the second install silently repoints the first one's panels at
/// a different index. Separate names are what let a DaemonSet install and a
/// policy coexist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRenaming {
    /// The shared name: index, index template, ingest pipeline, index pattern.
    pub name: String,
    /// Declared id → id on this path. Covers the index and every saved object.
    pub map: BTreeMap<String, String>,
}

/// One saved object's id on the agent path. `velox-nginx-overview` becomes
/// `veloxagent-nginx-logs-v1-overview`: the package's own `velox-<id>-` prefix
/// carries no information once the artifact name is in front, and keeping it
/// would read `veloxagent-nginx-logs-v1-velox-nginx-overview`.
fn agent_saved_object_id(id: &str, package_id: &str, name: &str) -> String {
    let suffix = id
        .strip_prefix(&format!("velox-{package_id}-"))
        .unwrap_or(id);
    format!("{name}-{suffix}")
}

/// Work out every rename for one package at one version.
pub fn agent_renaming(pkg: &Package, version: &str) -> Result<AgentRenaming> {
    let name = agent_artifact_name(&pkg.manifest.id, &pkg.manifest.index, version);
    let mut map = BTreeMap::new();
    map.insert(pkg.manifest.index.clone(), name.clone());
    for o in parse_saved_objects(&pkg.saved_objects)? {
        if o.id == pkg.manifest.index {
            // The index pattern is named after the index; already mapped.
            continue;
        }
        let renamed = agent_saved_object_id(&o.id, &pkg.manifest.id, &name);
        map.insert(o.id, renamed);
    }
    Ok(AgentRenaming { name, map })
}

/// One string, renamed if it names something we are renaming.
///
/// Exact match only, plus the `<id>*` form an index template's
/// `index_patterns` uses — a substring rule would rewrite prose (a pipeline
/// description reads "VeloxSearch nginx access-log pipeline") and field names
/// that merely contain an id.
fn rename_str(s: &str, map: &BTreeMap<String, String>) -> Option<String> {
    if let Some(to) = map.get(s) {
        return Some(to.clone());
    }
    let stem = s.strip_suffix('*')?;
    map.get(stem).map(|to| format!("{to}*"))
}

/// Rewrite every renamed id inside a JSON value, including ids buried in a
/// JSON document that is itself stored as a string — Dashboards keeps a
/// visualization's index reference in
/// `attributes.kibanaSavedObjectMeta.searchSourceJSON`, and a rename that
/// stops at the outer object leaves every panel pointing at the old pattern.
fn rename_in_value(v: &mut serde_json::Value, map: &BTreeMap<String, String>) {
    match v {
        serde_json::Value::String(s) => {
            if let Some(to) = rename_str(s, map) {
                *s = to;
                return;
            }
            // A string that parses as a JSON object or array is treated as a
            // nested document. Scalars are left alone: `"1"` is not JSON here.
            let trimmed = s.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(mut inner) = serde_json::from_str::<serde_json::Value>(s) {
                    rename_in_value(&mut inner, map);
                    if let Ok(text) = serde_json::to_string(&inner) {
                        *s = text;
                    }
                }
            }
        }
        serde_json::Value::Array(items) => {
            for i in items {
                rename_in_value(i, map);
            }
        }
        serde_json::Value::Object(fields) => {
            for (_, val) in fields.iter_mut() {
                rename_in_value(val, map);
            }
        }
        _ => {}
    }
}

/// An index template, renamed for the agent path: its `index_patterns` follow
/// the new index name, and `index.default_pipeline` follows the pipeline —
/// which is renamed too, and whose id is not in the map because the package's
/// pipeline id is a bare word (`nginx`) that would match prose and field
/// values if it were renamed by string equality.
pub fn agent_index_template(body: &str, r: &AgentRenaming) -> Result<String> {
    let mut json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| Error::SavedObjects {
            line: 0,
            reason: format!("index-template.json is not valid JSON: {e}"),
        })?;
    rename_in_value(&mut json, &r.map);
    if let Some(settings) = json
        .get_mut("template")
        .and_then(|t| t.get_mut("settings"))
        .and_then(|s| s.as_object_mut())
    {
        if settings.contains_key("index.default_pipeline") {
            settings.insert(
                "index.default_pipeline".to_string(),
                serde_json::Value::String(r.name.clone()),
            );
        }
    }
    serde_json::to_string(&json).map_err(|e| Error::SavedObjects {
        line: 0,
        reason: e.to_string(),
    })
}

/// A saved-objects document, renamed for the agent path: every object id,
/// every reference between them, and every index reference inside a panel.
pub fn agent_saved_objects(ndjson: &str, r: &AgentRenaming) -> Result<String> {
    let mut out = String::with_capacity(ndjson.len());
    for (n, line) in ndjson.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let mut json: serde_json::Value =
            serde_json::from_str(line).map_err(|e| Error::SavedObjects {
                line: n + 1,
                reason: e.to_string(),
            })?;
        rename_in_value(&mut json, &r.map);
        out.push_str(
            &serde_json::to_string(&json).map_err(|e| Error::SavedObjects {
                line: n + 1,
                reason: e.to_string(),
            })?,
        );
        out.push('\n');
    }
    Ok(out)
}

/// What installing this package on the agent path creates — the same
/// accounting as [`created_resources`], under the renamed ids. The UI and the
/// teardown both read this, so a rename that forgot one artifact shows up as
/// an inventory that does not match the cluster.
pub fn agent_created_resources(pkg: &Package, version: &str) -> Result<BTreeSet<ResourceKey>> {
    let r = agent_renaming(pkg, version)?;
    let mut set = BTreeSet::new();
    if pkg.pipeline.is_some() {
        set.insert((ResourceKind::IngestPipeline, r.name.clone()));
    }
    if pkg.index_template.is_some() {
        set.insert((ResourceKind::IndexTemplate, r.name.clone()));
    }
    for o in parse_saved_objects(&pkg.saved_objects)? {
        let id = r.map.get(&o.id).cloned().unwrap_or(o.id);
        if o.kind == "index-pattern" {
            set.insert((ResourceKind::IndexPattern, id));
        } else {
            set.insert((ResourceKind::SavedObject, id));
        }
    }
    Ok(set)
}

/// The index pattern the collector account is granted, for one artifact name.
/// A trailing `*` so a rolled-over or dated index (`...-v1-000001`) is covered
/// by the same grant.
pub fn agent_artifact_pattern(name: &str) -> String {
    format!("{name}*")
}

/// Everything applying `pkg` creates in the customer's OpenSearch, derived from
/// the package itself (assets present + the ids inside `saved-objects.ndjson`).
/// These are the ids the **DaemonSet** path installs under; the agent path has
/// its own, see [`agent_created_resources`].
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

    /// A package shaped like the real nginx one: an index pattern named after
    /// the index, a dashboard that references a visualization, and a
    /// visualization whose index reference is JSON inside a string.
    fn nginx_package() -> Package {
        Package {
            manifest: parse_manifest(
                "id: nginx\nindex: nginx-logs\ndashboard: velox-nginx-overview\n\
                 assets:\n  agent_config: agent.conf.tmpl\n  pipeline: pipeline.json\n  \
                 index_template: index-template.json\n  saved_objects: saved-objects.ndjson\n\
                 teardown:\n  ingest_pipeline: nginx\n  index_template: nginx-logs\n  \
                 index_pattern: nginx-logs\n  saved_objects:\n    - velox-nginx-overview\n    \
                 - velox-nginx-status\n",
            )
            .expect("fixture manifest"),
            pipeline: Some(
                r#"{"description":"VeloxSearch nginx access-log pipeline","processors":[]}"#
                    .into(),
            ),
            index_template: Some(
                r#"{"index_patterns":["nginx-logs*"],"template":{"settings":{"index.default_pipeline":"nginx"},"mappings":{"properties":{}}}}"#
                    .into(),
            ),
            saved_objects: [
                r#"{"type":"index-pattern","id":"nginx-logs","attributes":{"title":"nginx-logs","timeFieldName":"@timestamp"}}"#,
                r#"{"type":"visualization","id":"velox-nginx-status","attributes":{"title":"Status","kibanaSavedObjectMeta":{"searchSourceJSON":"{\"index\":\"nginx-logs\",\"query\":{\"query\":\"\"}}"}},"references":[{"type":"index-pattern","id":"nginx-logs","name":"kibanaSavedObjectMeta.searchSourceJSON.index"}]}"#,
                r#"{"type":"dashboard","id":"velox-nginx-overview","attributes":{"title":"nginx"},"references":[{"type":"visualization","id":"velox-nginx-status","name":"panel_1"}]}"#,
            ]
            .join("\n"),
            agent_config: "[OUTPUT]\n    Index {index}\n".into(),
            otel_config: None,
        }
    }

    #[test]
    fn the_agent_path_renames_the_index_the_template_and_the_pipeline() {
        let pkg = nginx_package();
        let r = agent_renaming(&pkg, "1.0.0").expect("renaming");
        assert_eq!(r.name, "veloxagent-nginx-logs-v1");

        let tmpl = agent_index_template(pkg.index_template.as_deref().unwrap(), &r).unwrap();
        let json: serde_json::Value = serde_json::from_str(&tmpl).unwrap();
        // The template must match the index the agent actually writes...
        assert_eq!(json["index_patterns"][0], "veloxagent-nginx-logs-v1*");
        // ...and point at the pipeline under its new id, or every document is
        // indexed raw and the dashboards' fields do not exist.
        assert_eq!(
            json["template"]["settings"]["index.default_pipeline"],
            "veloxagent-nginx-logs-v1"
        );
    }

    #[test]
    fn renaming_follows_every_reference_including_the_one_inside_a_string() {
        let pkg = nginx_package();
        let r = agent_renaming(&pkg, "1.0.0").expect("renaming");
        let out = agent_saved_objects(&pkg.saved_objects, &r).unwrap();
        let objs: Vec<serde_json::Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();

        // The index pattern takes the artifact name, title included — the
        // title is what the pattern matches, not decoration.
        assert_eq!(objs[0]["id"], "veloxagent-nginx-logs-v1");
        assert_eq!(objs[0]["attributes"]["title"], "veloxagent-nginx-logs-v1");

        // The package's own `velox-nginx-` prefix is dropped: the artifact
        // name already says which integration and version this is.
        assert_eq!(objs[1]["id"], "veloxagent-nginx-logs-v1-status");
        assert_eq!(objs[2]["id"], "veloxagent-nginx-logs-v1-overview");

        // A reference between objects follows...
        assert_eq!(objs[1]["references"][0]["id"], "veloxagent-nginx-logs-v1");
        assert_eq!(
            objs[2]["references"][0]["id"],
            "veloxagent-nginx-logs-v1-status"
        );
        // ...and so does the index buried in searchSourceJSON, which is a
        // JSON document stored as a string. A rename that stops at the outer
        // object leaves every panel reading the old pattern.
        let inner: serde_json::Value = serde_json::from_str(
            objs[1]["attributes"]["kibanaSavedObjectMeta"]["searchSourceJSON"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(inner["index"], "veloxagent-nginx-logs-v1");
    }

    #[test]
    fn renaming_leaves_prose_and_unrelated_values_alone() {
        let pkg = nginx_package();
        let r = agent_renaming(&pkg, "1.0.0").expect("renaming");
        // `nginx` is the package id and appears in a description. Renaming by
        // substring would rewrite it; renaming by equality must not.
        let out = agent_saved_objects(&pkg.saved_objects, &r).unwrap();
        assert!(out.contains("\"title\":\"nginx\""), "{out}");
        let tmpl = agent_index_template(pkg.index_template.as_deref().unwrap(), &r).unwrap();
        assert!(!tmpl.contains("veloxagent-nginx-logs-v1-logs"), "{tmpl}");
    }

    /// The inventory the UI shows and the teardown uses must be the renamed
    /// set, or it describes artifacts that are not in the cluster.
    #[test]
    fn the_inventory_is_the_renamed_one() {
        let pkg = nginx_package();
        let declared = created_resources(&pkg).unwrap();
        let agent = agent_created_resources(&pkg, "1.0.0").unwrap();
        assert_eq!(declared.len(), agent.len());
        for (_, id) in &agent {
            assert!(
                id.starts_with("veloxagent-nginx-logs-v1"),
                "{id} was not renamed"
            );
        }
        assert!(agent.contains(&(
            ResourceKind::IngestPipeline,
            "veloxagent-nginx-logs-v1".to_string()
        )));
        assert!(agent.contains(&(
            ResourceKind::IndexPattern,
            "veloxagent-nginx-logs-v1".to_string()
        )));
    }

    #[test]
    fn an_agent_artifact_is_named_for_its_integration_signal_and_major() {
        assert_eq!(
            agent_artifact_name("nginx", "nginx-logs", "1.0.0"),
            "veloxagent-nginx-logs-v1"
        );
        assert_eq!(
            agent_artifact_name("nginx", "nginx-metrics", "1.4.2"),
            "veloxagent-nginx-metrics-v1"
        );
        assert_eq!(
            agent_artifact_name("nginx", "nginx-logs", "2.0.0"),
            "veloxagent-nginx-logs-v2"
        );
    }

    /// The signal comes from a closed list, not from stripping the package id:
    /// `k8s-events` has the same id and index, and stripping would leave an
    /// empty segment in the middle of the name.
    #[test]
    fn the_signal_never_comes_out_empty() {
        assert_eq!(
            agent_artifact_name("k8s-events", "k8s-events", "1.0.0"),
            "veloxagent-k8s-events-events-v1"
        );
        assert_eq!(signal_of("k8s-audit-logs"), "logs");
        // An index that ends in nothing we recognise is still a log source,
        // which is what every integration shipped so far is.
        assert_eq!(signal_of("something-else"), "logs");
        assert_eq!(signal_of(""), "logs");
    }

    /// A malformed version must not produce `v0` (which would read as a
    /// pre-release) or an empty segment.
    #[test]
    fn a_broken_version_still_names_something_readable() {
        for v in ["", "x", "0.9.1", "0", "-", "1"] {
            let n = agent_artifact_name("redis", "redis-logs", v);
            assert!(n == "veloxagent-redis-logs-v1", "{v:?} produced {n}");
        }
        assert_eq!(major_of("12.3.4"), 12);
    }

    /// The grant has to cover a rolled-over index, or collection stops the
    /// first time an index rolls.
    #[test]
    fn the_grant_covers_rollover() {
        let name = agent_artifact_name("ssh", "ssh-logs", "1.0.0");
        let pattern = agent_artifact_pattern(&name);
        assert_eq!(pattern, "veloxagent-ssh-logs-v1*");
        assert!(format!("{name}-000001").starts_with(pattern.trim_end_matches('*')));
    }
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

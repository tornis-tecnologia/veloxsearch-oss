// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! Registry **Spec 2.0** packages (ADR-074): the typed contract the Mesh path
//! reads — manifest, streams, fields and bindings, the Fluent Bit plugin
//! catalogue, `packages/index.json`, the activation a policy revision stores —
//! plus the rules the core can evaluate without a cluster (R3–R16, R22 of
//! `docs/spec/README.md §14` in veloxsearch-registry) and the names every
//! artifact of a stream carries.
//!
//! **Strict typed parsing is the contract, not a JSON Schema engine.** Every
//! struct refuses unknown keys, so a package that says something this version
//! does not understand is refused rather than half-applied; the registry's CI
//! validates against the JSON Schemas, and the conformance example in the
//! registry must parse here (the test at the bottom proves it). What a schema
//! cannot say — a bind that points at a plugin key the catalogue has not got,
//! a `pipeline_toggle` whose tag the pipeline lacks — is checked explicitly.
//!
//! Deliberately pure, like the rest of this crate: no hashing, no signature,
//! no I/O except the optional directory loaders the CLI and the tests use.
//! `catalog.rs` in the core verifies the signature and the `files:` inventory
//! **before** bytes reach [`Package::from_files`]; a `Package` here is already
//! trusted data. Compilation to Fluent Bit YAML lives in `velox-policy`; the
//! OpenSearch writes in the core's `integrations.rs`.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The one `spec_version` this module reads.
pub const SPEC_VERSION: &str = "2.0";

/// The minimum Fluent Bit a 2.0 package may declare (R4): YAML configuration
/// with processors exists from 3.2.
pub const MIN_FLUENT_BIT: [u64; 3] = [3, 2, 0];

/// The closed set of context references a package may use in `fixed:` strings
/// (spec §7). Anything else inside `{…}`, and any `${…}`, is refused (R7) —
/// the 2.0 counterpart of [`crate::CLOSED_TOKENS`].
pub const CONTEXT_REFS: [&str; 6] = [
    "stream_tag",
    "artifact",
    "state_dir",
    "integration",
    "stream",
    "package_version",
];

/// Plugins that execute code on the host, or exist only for tests. Never in
/// the catalogue (R16) and refused in a stream even if a catalogue were to
/// list them — the allowlist is enforced on both sides (spec §9).
pub const EXCLUDED_PLUGINS: &[&str] = &[
    "exec",
    "exec_wasi",
    "lua",
    "wasm",
    "tensorflow",
    "fluent_typed_scripting",
    "dummy",
    "random",
    "stdin",
];

/// The artifact-name prefix ADR-070's grant covers; shared with 1.x.
pub const ARTIFACT_PREFIX: &str = crate::AGENT_ARTIFACT_PREFIX;

/// The backing indices of every Spec 2.0 data stream (`.ds-<N>-000001`).
/// A bulk `create` into the data stream `N` lands in them, and the security
/// plugin checks the write against the backing index, so the collector's
/// grant names both (ADR-074 decision 3).
pub const BACKING_INDEX_PATTERN: &str = ".ds-veloxagent-*";

/// What `{state_dir}` compiles to (ADR-074 decision 7): the agent's state
/// directory is the agent's to know, so the compiler emits an environment
/// reference and the agent exports it.
pub const STATE_DIR_REF: &str = "${VELOX_STATE_DIR}";

/// Maximum length of an OpenSearch index name (R22).
const MAX_INDEX_NAME: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum Spec2Error {
    #[error("{file}: {source}")]
    Yaml {
        file: String,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("{file}: {source}")]
    Json {
        file: String,
        #[source]
        source: serde_json::Error,
    },
    /// A rule the schema cannot express, or a typed-parse detail worth a
    /// sentence; `file` is the path inside the package (or the catalogue).
    #[error("{file}: {reason}")]
    Invalid { file: String, reason: String },
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Spec2Error>;

fn invalid(file: &str, reason: impl Into<String>) -> Spec2Error {
    Spec2Error::Invalid {
        file: file.to_string(),
        reason: reason.into(),
    }
}

// ---------------------------------------------------------------------------
// Shared value types
// ---------------------------------------------------------------------------

/// A localised string: `en` is mandatory by schema, `es`/`pt` by R17 on
/// published packages. [`I18n::get`] falls back to `en`.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct I18n {
    pub en: String,
    #[serde(default)]
    pub es: Option<String>,
    #[serde(default)]
    pub pt: Option<String>,
}

impl I18n {
    pub fn get(&self, lang: &str) -> &str {
        match lang {
            "pt" => self.pt.as_deref().unwrap_or(&self.en),
            "es" => self.es.as_deref().unwrap_or(&self.en),
            _ => &self.en,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Release {
    Ga,
    Beta,
    Experimental,
}

#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    Linux,
    Windows,
    Darwin,
}

impl Os {
    pub fn name(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Windows => "windows",
            Os::Darwin => "darwin",
        }
    }
    pub const ALL: [Os; 3] = [Os::Linux, Os::Windows, Os::Darwin];
}

/// What a stream **is** for the operator (category, filters, dashboards).
#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Signal {
    Logs,
    Metrics,
    Traces,
}

/// What the Fluent Bit plugin **emits**. Decides the output (spec §3, guide
/// §9): `logs`/`traces` go to `opensearch`, `metrics` to `opentelemetry`.
#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum EventType {
    Logs,
    Metrics,
    Traces,
}

impl EventType {
    pub fn name(self) -> &'static str {
        match self {
            EventType::Logs => "logs",
            EventType::Metrics => "metrics",
            EventType::Traces => "traces",
        }
    }
    /// Streams of this event type have an OpenSearch backend (`N`); metric
    /// streams create nothing there (R10/R11).
    pub fn has_opensearch_backend(self) -> bool {
        !matches!(self, EventType::Metrics)
    }
}

/// The form-control types of spec §6.1.
#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FieldType {
    String,
    Text,
    Integer,
    Number,
    Bool,
    Select,
    Duration,
    Size,
    Regex,
    Password,
    Yaml,
}

impl FieldType {
    pub fn name(self) -> &'static str {
        match self {
            FieldType::String => "string",
            FieldType::Text => "text",
            FieldType::Integer => "integer",
            FieldType::Number => "number",
            FieldType::Bool => "bool",
            FieldType::Select => "select",
            FieldType::Duration => "duration",
            FieldType::Size => "size",
            FieldType::Regex => "regex",
            FieldType::Password => "password",
            FieldType::Yaml => "yaml",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    #[default]
    General,
    Advanced,
}

/// Where a field's value goes (spec §6.2). Exactly one destination; the
/// untagged variants are told apart by their one mandatory key, and
/// `deny_unknown_fields` on each keeps a stray key from being swallowed by a
/// looser variant.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(untagged)]
pub enum Bind {
    /// A plugin key in the Fluent Bit YAML.
    Key {
        key: String,
        #[serde(default)]
        join: Option<String>,
        #[serde(default)]
        repeat: Option<bool>,
        #[serde(default)]
        omit_if: Option<serde_yaml::Value>,
        #[serde(default)]
        prefix: Option<String>,
    },
    /// A dotted path in the component template `N@package`.
    Template { template: String },
    /// `false` removes the ingest-pipeline processor with this `tag`.
    PipelineToggle { pipeline_toggle: String },
    /// `<tag>.<param>` of an ingest-pipeline processor.
    PipelineParam { pipeline_param: String },
    /// `type: yaml` only: the object's keys are merged into the plugin's.
    Merge { merge: bool },
    /// Overrides one `backend.lifecycle` default.
    Lifecycle { lifecycle: LifecycleKey },
}

#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleKey {
    RolloverMaxSize,
    RolloverMaxAge,
    RolloverMaxDocs,
    DeleteAfter,
}

impl LifecycleKey {
    pub fn name(self) -> &'static str {
        match self {
            LifecycleKey::RolloverMaxSize => "rollover_max_size",
            LifecycleKey::RolloverMaxAge => "rollover_max_age",
            LifecycleKey::RolloverMaxDocs => "rollover_max_docs",
            LifecycleKey::DeleteAfter => "delete_after",
        }
    }
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShowIf {
    pub field: String,
    #[serde(default)]
    pub equals: Option<serde_yaml::Value>,
    #[serde(default, rename = "in")]
    pub one_of: Option<Vec<serde_yaml::Value>>,
}

/// A `select` option: a bare scalar or `{value, title}`.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(untagged)]
pub enum SelectOption {
    Titled {
        value: serde_yaml::Value,
        #[serde(default)]
        title: Option<I18n>,
    },
    Bare(serde_yaml::Value),
}

impl SelectOption {
    pub fn value(&self) -> &serde_yaml::Value {
        match self {
            SelectOption::Titled { value, .. } | SelectOption::Bare(value) => value,
        }
    }
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OptionsFrom {
    Parsers,
    MultilineParsers,
}

/// One configurable parameter rendered as a form control (spec §6).
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
    pub title: I18n,
    #[serde(default)]
    pub description: Option<I18n>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
    #[serde(default)]
    pub group: Group,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub show_if: Option<ShowIf>,
    #[serde(default)]
    pub options: Option<Vec<SelectOption>>,
    #[serde(default)]
    pub options_from: Option<OptionsFrom>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub pattern: Option<String>,
    pub bind: Bind,
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    pub min_mesh_version: String,
    pub min_agent_version: String,
    pub min_fluent_bit_version: String,
    pub os: Vec<Os>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// One row of the `files:` inventory (R2): the fetcher downloads exactly these
/// and the signature digest covers exactly these plus the manifest.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
}

/// The signature block, carried as data; `catalog.rs` verifies it.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub algorithm: String,
    pub key_id: String,
    pub value: String,
}

/// `packages/<id>/<version>/manifest.yaml` (spec §4).
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub spec_version: String,
    pub id: String,
    pub version: String,
    pub release: Release,
    pub title: I18n,
    pub summary: I18n,
    #[serde(default)]
    pub description: Option<I18n>,
    pub categories: Vec<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub owner: Option<Owner>,
    #[serde(default)]
    pub license: Option<String>,
    pub compatibility: Compatibility,
    /// Stream ids in display order; each is `streams/<id>/stream.yaml`.
    pub streams: Vec<String>,
    #[serde(default)]
    pub dashboards: Vec<String>,
    #[serde(default)]
    pub docs: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub changelog: Option<String>,
    pub files: Vec<FileEntry>,
    /// Optional at the type level so an unsigned draft still parses for
    /// `velox sign`; `verify_package` is what refuses a missing block.
    #[serde(default)]
    pub signature: Option<Signature>,
}

impl Manifest {
    pub fn parse(yaml: &str) -> Result<Manifest> {
        let m: Manifest = serde_yaml::from_str(yaml).map_err(|source| Spec2Error::Yaml {
            file: "manifest.yaml".into(),
            source,
        })?;
        m.check()?;
        Ok(m)
    }

    /// The identity and compatibility rules a manifest carries alone.
    fn check(&self) -> Result<()> {
        let f = "manifest.yaml";
        if self.spec_version != SPEC_VERSION {
            return Err(invalid(
                f,
                format!(
                    "spec_version {:?} is not {SPEC_VERSION:?}",
                    self.spec_version
                ),
            ));
        }
        if !is_package_id(&self.id) {
            return Err(invalid(f, format!("id {:?} is not a package id", self.id)));
        }
        let version = semver(&self.version)
            .ok_or_else(|| invalid(f, format!("version {:?} is not semver", self.version)))?;
        let _ = version;
        for (what, v) in [
            ("min_mesh_version", &self.compatibility.min_mesh_version),
            ("min_agent_version", &self.compatibility.min_agent_version),
            (
                "min_fluent_bit_version",
                &self.compatibility.min_fluent_bit_version,
            ),
        ] {
            if semver(v).is_none() {
                return Err(invalid(
                    f,
                    format!("compatibility.{what} {v:?} is not semver"),
                ));
            }
        }
        if !version_ge(&self.compatibility.min_fluent_bit_version, "3.2.0") {
            return Err(invalid(
                f,
                format!(
                    "compatibility.min_fluent_bit_version {:?} is below 3.2.0 (R4)",
                    self.compatibility.min_fluent_bit_version
                ),
            ));
        }
        if self.compatibility.os.is_empty() {
            return Err(invalid(f, "compatibility.os is empty"));
        }
        if self.streams.is_empty() {
            return Err(invalid(f, "streams is empty"));
        }
        let mut seen = BTreeSet::new();
        for s in &self.streams {
            if !is_stream_id(s) {
                return Err(invalid(f, format!("stream id {s:?} is not valid")));
            }
            if !seen.insert(s.as_str()) {
                return Err(invalid(f, format!("stream id {s:?} is listed twice")));
            }
        }
        if self.files.is_empty() {
            return Err(invalid(f, "files inventory is empty (R2)"));
        }
        let mut paths = BTreeSet::new();
        for e in &self.files {
            if !is_relpath(&e.path) {
                return Err(invalid(
                    f,
                    format!("files: {:?} is not a safe relative path", e.path),
                ));
            }
            if e.path == "manifest.yaml" {
                return Err(invalid(f, "files: must not list manifest.yaml itself"));
            }
            if !is_sha256_hex(&e.sha256) {
                return Err(invalid(
                    f,
                    format!("files: {:?} has a malformed sha256", e.path),
                ));
            }
            if !paths.insert(e.path.as_str()) {
                return Err(invalid(f, format!("files: {:?} is listed twice", e.path)));
            }
        }
        for p in self
            .dashboards
            .iter()
            .chain(self.icon.iter())
            .chain(self.changelog.iter())
            .chain(self.docs.iter().flat_map(|d| d.values()))
        {
            if !paths.contains(p.as_str()) {
                return Err(invalid(
                    f,
                    format!("{p:?} is referenced but not in the files inventory"),
                ));
            }
        }
        Ok(())
    }

    /// The major of the package version — the `v<major>` of every artifact.
    pub fn major(&self) -> u64 {
        semver(&self.version).map(|v| v[0]).unwrap_or(0)
    }

    /// The inventory as `path → sha256`.
    pub fn inventory(&self) -> BTreeMap<&str, &str> {
        self.files
            .iter()
            .map(|e| (e.path.as_str(), e.sha256.as_str()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------------

/// `fixed:` values are scalars or lists of scalars; kept as YAML and checked
/// by [`check_fixed`], because `serde_yaml` untagged enums over numbers are a
/// trap not worth walking into for a two-shape value.
pub type Fixed = BTreeMap<String, serde_yaml::Value>;

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub plugin: String,
    #[serde(default)]
    pub fixed: Fixed,
    #[serde(default)]
    pub fields: Vec<Field>,
}

/// A Fluent Bit parser as the manual spells it. Only `name` and `title` are
/// modelled; the rest is copied into `parsers:` by the compiler, and the
/// registry's schema closes the key set.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
pub struct Parser {
    pub name: String,
    #[serde(default)]
    pub title: Option<I18n>,
    pub format: String,
    #[serde(flatten)]
    pub rest: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
pub struct MultilineParser {
    pub name: String,
    #[serde(default)]
    pub title: Option<I18n>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub rules: Option<Vec<MultilineRule>>,
    #[serde(flatten)]
    pub rest: BTreeMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MultilineRule {
    pub state: String,
    pub regex: String,
    pub next_state: String,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ParserField {
    #[serde(default)]
    pub title: Option<I18n>,
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub group: Option<Group>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProcessorItem {
    pub id: String,
    pub plugin: String,
    pub title: I18n,
    #[serde(default)]
    pub description: Option<I18n>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub fixed: Fixed,
    #[serde(default)]
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Processors {
    #[serde(default)]
    pub defaults: Vec<ProcessorItem>,
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Lifecycle {
    #[serde(default)]
    pub rollover_max_size: Option<String>,
    #[serde(default)]
    pub rollover_max_age: Option<String>,
    #[serde(default)]
    pub rollover_max_docs: Option<u64>,
    #[serde(default)]
    pub delete_after: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Backend {
    #[serde(default)]
    pub index_template: Option<String>,
    #[serde(default)]
    pub ingest_pipeline: Option<String>,
    #[serde(default = "default_true")]
    pub custom_hooks: bool,
    #[serde(default)]
    pub lifecycle: Option<Lifecycle>,
    #[serde(default)]
    pub fields: Vec<Field>,
}

fn default_true() -> bool {
    true
}

/// `streams/<id>/stream.yaml` (spec §5).
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Stream {
    pub id: String,
    pub title: I18n,
    #[serde(default)]
    pub description: Option<I18n>,
    pub signal: Signal,
    pub event_type: EventType,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Empty = every OS the package supports.
    #[serde(default)]
    pub os: Vec<Os>,
    pub input: Input,
    #[serde(default)]
    pub parsers: Vec<Parser>,
    #[serde(default)]
    pub multiline_parsers: Vec<MultilineParser>,
    #[serde(default)]
    pub parser_field: Option<ParserField>,
    #[serde(default)]
    pub processors: Processors,
    #[serde(default)]
    pub backend: Option<Backend>,
    #[serde(default)]
    pub since: Option<String>,
}

impl Stream {
    pub fn parse(file: &str, yaml: &str) -> Result<Stream> {
        serde_yaml::from_str(yaml).map_err(|source| Spec2Error::Yaml {
            file: file.to_string(),
            source,
        })
    }

    /// The OSes this stream runs on: its own list, else the package's.
    pub fn os_or<'a>(&'a self, package_os: &'a [Os]) -> &'a [Os] {
        if self.os.is_empty() {
            package_os
        } else {
            &self.os
        }
    }

    /// Every field of the stream with the scope it lives in, for scope-wide
    /// checks and for form generation.
    pub fn fields(&self) -> impl Iterator<Item = (FieldScope<'_>, &Field)> {
        let input = self.input.fields.iter().map(|f| (FieldScope::Input, f));
        let procs = self.processors.defaults.iter().flat_map(|p| {
            p.fields
                .iter()
                .map(move |f| (FieldScope::Processor(&p.id), f))
        });
        let backend = self
            .backend
            .iter()
            .flat_map(|b| b.fields.iter().map(|f| (FieldScope::Backend, f)));
        input.chain(procs).chain(backend)
    }
}

/// Where a field lives — `name` is unique per scope, not per stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldScope<'a> {
    Input,
    Processor(&'a str),
    Backend,
}

impl std::fmt::Display for FieldScope<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FieldScope::Input => write!(f, "input"),
            FieldScope::Processor(id) => write!(f, "processors.{id}"),
            FieldScope::Backend => write!(f, "backend"),
        }
    }
}

// ---------------------------------------------------------------------------
// Package = manifest + streams + the verified files
// ---------------------------------------------------------------------------

/// A verified Spec 2.0 package: the manifest, its streams in display order
/// and the inventory's bytes by relative path.
#[derive(Debug, Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub streams: Vec<Stream>,
    pub files: BTreeMap<String, Vec<u8>>,
}

impl Package {
    /// Build from the manifest text and the inventory's bytes (keys are the
    /// `files[].path` relpaths). Runs every rule this crate can judge (R3, R4,
    /// R7, R8, R10, R11, R13–R16, R22 and the field-scope uniqueness of §6.1).
    /// Catalogue-dependent rules are [`Package::check_catalog`].
    pub fn from_files(manifest_yaml: &str, files: BTreeMap<String, Vec<u8>>) -> Result<Package> {
        let manifest = Manifest::parse(manifest_yaml)?;
        // Every inventory entry must be present — the fetcher guarantees it,
        // the offline loader proves it here.
        for e in &manifest.files {
            if !files.contains_key(&e.path) {
                return Err(invalid(
                    "manifest.yaml",
                    format!(
                        "files: {:?} is in the inventory but was not provided",
                        e.path
                    ),
                ));
            }
        }
        // R3: listed streams ↔ stream directories, ids equal.
        let mut streams = Vec::with_capacity(manifest.streams.len());
        for id in &manifest.streams {
            let path = format!("streams/{id}/stream.yaml");
            let text = text_of(&files, &path)?;
            let stream = Stream::parse(&path, &text)?;
            if stream.id != *id {
                return Err(invalid(
                    &path,
                    format!(
                        "declares id {:?} but the directory is {id:?} (R3)",
                        stream.id
                    ),
                ));
            }
            streams.push(stream);
        }
        for path in files.keys() {
            if let Some(rest) = path.strip_prefix("streams/") {
                if let Some(dir) = rest.split('/').next() {
                    if !manifest.streams.iter().any(|s| s == dir) {
                        return Err(invalid(
                            path,
                            format!(
                                "stream directory {dir:?} is not listed in manifest.streams (R3)"
                            ),
                        ));
                    }
                }
            }
        }
        let pkg = Package {
            manifest,
            streams,
            files,
        };
        for stream in &pkg.streams {
            pkg.check_stream(stream)?;
        }
        Ok(pkg)
    }

    /// Read `packages/<id>/<version>/` off disk — the offline route and the
    /// tests'. Files not in the inventory are ignored, as the spec says.
    pub fn load_from_dir(dir: &Path) -> Result<Package> {
        let manifest_path = dir.join("manifest.yaml");
        let manifest_yaml =
            std::fs::read_to_string(&manifest_path).map_err(|source| Spec2Error::Io {
                what: format!("reading {}", manifest_path.display()),
                source,
            })?;
        let manifest = Manifest::parse(&manifest_yaml)?;
        let mut files = BTreeMap::new();
        for e in &manifest.files {
            let p = dir.join(&e.path);
            let bytes = std::fs::read(&p).map_err(|source| Spec2Error::Io {
                what: format!("reading {}", p.display()),
                source,
            })?;
            files.insert(e.path.clone(), bytes);
        }
        Package::from_files(&manifest_yaml, files)
    }

    pub fn stream(&self, id: &str) -> Option<&Stream> {
        self.streams.iter().find(|s| s.id == id)
    }

    /// A file of the inventory as UTF-8 text.
    pub fn text(&self, relpath: &str) -> Result<String> {
        text_of(&self.files, relpath)
    }

    /// `streams/<id>/<asset>` — the backend assets live beside the stream.
    pub fn stream_asset(&self, stream: &Stream, asset: &str) -> Result<String> {
        self.text(&format!("streams/{}/{asset}", stream.id))
    }

    /// The artifact name `N` of a stream (ADR-074 decision 3).
    pub fn artifact_name(&self, stream: &Stream) -> String {
        artifact_name(&stream.id, &self.manifest.id, self.manifest.major())
    }

    /// The Fluent Bit tag of a stream.
    pub fn stream_tag(&self, stream: &Stream) -> String {
        stream_tag(&self.manifest.id, &stream.id)
    }

    /// The per-stream rules (spec §5, §6, §8).
    fn check_stream(&self, s: &Stream) -> Result<()> {
        let file = format!("streams/{}/stream.yaml", s.id);
        let f = file.as_str();
        // R4: the stream's OS is within the package's.
        for os in &s.os {
            if !self.manifest.compatibility.os.contains(os) {
                return Err(invalid(
                    f,
                    format!("os {} is not in compatibility.os (R4)", os.name()),
                ));
            }
        }
        if let Some(since) = &s.since {
            if semver(since).is_none() {
                return Err(invalid(f, format!("since {since:?} is not semver")));
            }
        }
        // R16 / spec §9: nothing that executes code.
        check_allowed_plugin(f, &s.input.plugin)?;
        for p in &s.processors.defaults {
            check_allowed_plugin(f, &p.plugin)?;
        }
        for p in &s.processors.allow {
            check_allowed_plugin(f, p)?;
        }
        // R13: processor ids unique.
        let mut ids = BTreeSet::new();
        for p in &s.processors.defaults {
            if !is_local_id(&p.id) {
                return Err(invalid(
                    f,
                    format!("processor id {:?} is not a local id", p.id),
                ));
            }
            if !ids.insert(p.id.as_str()) {
                return Err(invalid(
                    f,
                    format!("processor id {:?} is not unique (R13)", p.id),
                ));
            }
        }
        // R14: parsers coherent.
        let mut parser_names = BTreeSet::new();
        for p in &s.parsers {
            if !parser_names.insert(p.name.as_str()) {
                return Err(invalid(
                    f,
                    format!("parser {:?} is not unique (R14)", p.name),
                ));
            }
        }
        let mut ml_names = BTreeSet::new();
        for p in &s.multiline_parsers {
            if !ml_names.insert(p.name.as_str()) {
                return Err(invalid(
                    f,
                    format!("multiline parser {:?} is not unique (R14)", p.name),
                ));
            }
            if p.kind == "regex" {
                let rules = p.rules.as_ref().filter(|r| !r.is_empty()).ok_or_else(|| {
                    invalid(f, format!("multiline parser {:?} has no rules", p.name))
                })?;
                if rules[0].state != "start_state" {
                    return Err(invalid(
                        f,
                        format!(
                            "multiline parser {:?} must begin in start_state (R14)",
                            p.name
                        ),
                    ));
                }
            }
        }
        if let Some(pf) = &s.parser_field {
            if let Some(d) = &pf.default {
                if d != "none" && !parser_names.contains(d.as_str()) {
                    return Err(invalid(
                        f,
                        format!("parser_field.default {d:?} is neither none nor a parser (R14)"),
                    ));
                }
            }
        }
        // R7: fixed values are scalars/lists with closed context references.
        check_fixed(f, "input.fixed", &s.input.fixed)?;
        for p in &s.processors.defaults {
            check_fixed(f, &format!("processors.{}.fixed", p.id), &p.fixed)?;
        }
        // R10 / R11: backend presence by event type.
        match (&s.backend, s.event_type.has_opensearch_backend()) {
            (None, true) => {
                return Err(invalid(
                    f,
                    format!(
                        "a {} stream needs backend.index_template (R11)",
                        s.event_type.name()
                    ),
                ))
            }
            (Some(b), true) => {
                if b.index_template.is_none() {
                    return Err(invalid(f, "backend.index_template is required (R11)"));
                }
            }
            (Some(b), false) => {
                if b.index_template.is_some()
                    || b.ingest_pipeline.is_some()
                    || b.lifecycle.is_some()
                    || !b.fields.is_empty()
                {
                    return Err(invalid(
                        f,
                        "a metrics stream creates nothing in OpenSearch: no backend assets, lifecycle or fields (R10)",
                    ));
                }
            }
            (None, false) => {}
        }
        // Backend assets exist in the inventory; lifecycle values parse.
        let mut pipeline_tags: BTreeSet<String> = BTreeSet::new();
        if let Some(b) = &s.backend {
            for asset in [&b.index_template, &b.ingest_pipeline]
                .into_iter()
                .flatten()
            {
                if !is_asset_file(asset) {
                    return Err(invalid(
                        f,
                        format!("backend asset {asset:?} is not a plain file name"),
                    ));
                }
                let path = format!("streams/{}/{asset}", s.id);
                if !self.files.contains_key(&path) {
                    return Err(invalid(
                        f,
                        format!("backend asset {path:?} is not in the files inventory"),
                    ));
                }
            }
            if let Some(it) = &b.index_template {
                let text = self.stream_asset(s, it)?;
                let v: serde_json::Value =
                    serde_json::from_str(&text).map_err(|source| Spec2Error::Json {
                        file: format!("streams/{}/{it}", s.id),
                        source,
                    })?;
                let tpl = v.get("template").and_then(|t| t.as_object());
                if tpl.is_none() {
                    return Err(invalid(
                        &format!("streams/{}/{it}", s.id),
                        "a component template body needs a top-level \"template\" object (§8.2)",
                    ));
                }
                for forbidden in ["index_patterns", "data_stream", "composed_of"] {
                    if v.get(forbidden).is_some() {
                        return Err(invalid(
                            &format!("streams/{}/{it}", s.id),
                            format!("must not declare {forbidden:?}; the core sets it (§8.2)"),
                        ));
                    }
                }
            }
            if let Some(ip) = &b.ingest_pipeline {
                let text = self.stream_asset(s, ip)?;
                let v: serde_json::Value =
                    serde_json::from_str(&text).map_err(|source| Spec2Error::Json {
                        file: format!("streams/{}/{ip}", s.id),
                        source,
                    })?;
                pipeline_tags = pipeline_processor_tags(&v);
            }
            if let Some(lc) = &b.lifecycle {
                if let Some(v) = &lc.rollover_max_size {
                    if !is_size(v) {
                        return Err(invalid(
                            f,
                            format!("lifecycle.rollover_max_size {v:?} is not a size"),
                        ));
                    }
                }
                for (k, v) in [
                    ("rollover_max_age", &lc.rollover_max_age),
                    ("delete_after", &lc.delete_after),
                ] {
                    if let Some(v) = v {
                        if !is_duration(v) {
                            return Err(invalid(
                                f,
                                format!("lifecycle.{k} {v:?} is not a duration"),
                            ));
                        }
                    }
                }
            }
        }
        // §6.1 field rules per scope, R8, R15.
        let mut scope_names: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
        for (scope, field) in s.fields() {
            let where_ = format!("{scope}.{}", field.name);
            if !is_local_id(&field.name) {
                return Err(invalid(
                    f,
                    format!("{where_}: field name is not a local id"),
                ));
            }
            if !scope_names
                .entry(scope.to_string())
                .or_default()
                .insert(field.name.as_str())
            {
                return Err(invalid(
                    f,
                    format!("{where_}: field name is not unique in its scope"),
                ));
            }
            check_field(f, &where_, field, scope, s, &pipeline_tags)?;
        }
        // show_if points at a field of the same scope (R8).
        for (scope, field) in s.fields() {
            if let Some(si) = &field.show_if {
                let names = scope_names.get(&scope.to_string());
                if !names.is_some_and(|n| n.contains(si.field.as_str())) || si.field == field.name {
                    return Err(invalid(
                        f,
                        format!(
                            "{scope}.{}: show_if.field {:?} is not another field of the same scope (R8)",
                            field.name, si.field
                        ),
                    ));
                }
                if si.equals.is_none() == si.one_of.is_none() {
                    return Err(invalid(
                        f,
                        format!(
                            "{scope}.{}: show_if needs exactly one of equals / in",
                            field.name
                        ),
                    ));
                }
            }
        }
        // R22: the artifact name is a valid, bounded index name.
        if s.event_type.has_opensearch_backend() {
            let n = self.artifact_name(s);
            if n.len() > MAX_INDEX_NAME || !is_index_name(&n) {
                return Err(invalid(
                    f,
                    format!("artifact name {n:?} is not a valid index name (R22)"),
                ));
            }
        }
        Ok(())
    }

    /// The catalogue-dependent rules: R5, R6, R9, R12 — the input and every
    /// processor exist in the allowlist, accept the stream's event type and
    /// OS, and every `key` binding points at a real, operator-settable
    /// parameter of the right shape.
    pub fn check_catalog(&self, cat: &PluginCatalog) -> Result<()> {
        for s in &self.streams {
            let file = format!("streams/{}/stream.yaml", s.id);
            let f = file.as_str();
            let input = cat.input(&s.input.plugin).ok_or_else(|| {
                invalid(
                    f,
                    format!(
                        "input plugin {:?} is not in the catalogue (R5)",
                        s.input.plugin
                    ),
                )
            })?;
            for os in s.os_or(&self.manifest.compatibility.os) {
                if !input.os.is_empty() && !input.os.contains(os) {
                    return Err(invalid(
                        f,
                        format!(
                            "input {:?} does not run on {} (R5)",
                            s.input.plugin,
                            os.name()
                        ),
                    ));
                }
            }
            if !input.event_types.contains(&s.event_type) {
                return Err(invalid(
                    f,
                    format!(
                        "input {:?} does not emit {} (R9); it emits {}",
                        s.input.plugin,
                        s.event_type.name(),
                        input
                            .event_types
                            .iter()
                            .map(|e| e.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ));
            }
            check_bindings_against(f, "input", input, &s.input.fixed, &s.input.fields)?;
            for p in s.processors.defaults.iter() {
                let plugin = cat.filter_or_processor(&p.plugin).ok_or_else(|| {
                    invalid(
                        f,
                        format!(
                            "processors.{}: plugin {:?} is not in the catalogue (R12)",
                            p.id, p.plugin
                        ),
                    )
                })?;
                if !plugin.event_types.contains(&s.event_type) {
                    return Err(invalid(
                        f,
                        format!(
                            "processors.{}: {:?} does not accept {} (R12)",
                            p.id,
                            p.plugin,
                            s.event_type.name()
                        ),
                    ));
                }
                check_bindings_against(
                    f,
                    &format!("processors.{}", p.id),
                    plugin,
                    &p.fixed,
                    &p.fields,
                )?;
            }
            for name in &s.processors.allow {
                let plugin = cat.filter_or_processor(name).ok_or_else(|| {
                    invalid(
                        f,
                        format!("processors.allow: {name:?} is not in the catalogue (R12)"),
                    )
                })?;
                if !plugin.event_types.contains(&s.event_type) {
                    return Err(invalid(
                        f,
                        format!(
                            "processors.allow: {name:?} does not accept {} (R12)",
                            s.event_type.name()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn text_of(files: &BTreeMap<String, Vec<u8>>, relpath: &str) -> Result<String> {
    let raw = files
        .get(relpath)
        .ok_or_else(|| invalid(relpath, "file is not in the package"))?;
    String::from_utf8(raw.clone()).map_err(|_| invalid(relpath, "file is not UTF-8"))
}

fn check_allowed_plugin(file: &str, name: &str) -> Result<()> {
    if !is_plugin_name(name) {
        return Err(invalid(file, format!("{name:?} is not a plugin name")));
    }
    if EXCLUDED_PLUGINS.contains(&name) {
        return Err(invalid(
            file,
            format!("plugin {name:?} executes code on the host or is test-only; refused (spec §9)"),
        ));
    }
    Ok(())
}

/// R7 on one `fixed:` block: scalars or non-empty lists of scalars, strings
/// resolvable against the closed reference set, no control characters.
fn check_fixed(file: &str, where_: &str, fixed: &Fixed) -> Result<()> {
    for (k, v) in fixed {
        if !is_plugin_key(k) {
            return Err(invalid(
                file,
                format!("{where_}: {k:?} is not a plugin key"),
            ));
        }
        let scalars: Vec<&serde_yaml::Value> = match v {
            serde_yaml::Value::Sequence(items) if !items.is_empty() => items.iter().collect(),
            serde_yaml::Value::Sequence(_) => {
                return Err(invalid(file, format!("{where_}.{k}: empty list")))
            }
            other => vec![other],
        };
        for s in scalars {
            match s {
                serde_yaml::Value::String(text) => {
                    check_context_refs(text)
                        .map_err(|reason| invalid(file, format!("{where_}.{k}: {reason}")))?;
                }
                serde_yaml::Value::Bool(_) | serde_yaml::Value::Number(_) => {}
                _ => {
                    return Err(invalid(
                        file,
                        format!("{where_}.{k}: values must be scalars or lists of scalars"),
                    ))
                }
            }
        }
    }
    Ok(())
}

/// A string is acceptable in `fixed:` when every `{…}` names a closed
/// reference, it carries no `${…}` and no control character.
pub fn check_context_refs(text: &str) -> std::result::Result<(), String> {
    if has_control_chars(text) {
        return Err("control character in value".into());
    }
    if text.contains("${") {
        return Err(
            "`${…}` is not allowed in a package; environment references are the core's (R7)".into(),
        );
    }
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(format!("unbalanced `{{` in {text:?}"));
        };
        let name = &after[..end];
        if !CONTEXT_REFS.contains(&name) {
            return Err(format!(
                "`{{{name}}}` is not a context reference (allowed: {}) (R7)",
                CONTEXT_REFS.join(", ")
            ));
        }
        rest = &after[end + 1..];
    }
    if rest.contains('}') {
        return Err(format!("unbalanced `}}` in {text:?}"));
    }
    Ok(())
}

/// Per-field rules (§6.1, R8, R15) that need no catalogue.
fn check_field(
    file: &str,
    where_: &str,
    field: &Field,
    scope: FieldScope<'_>,
    s: &Stream,
    pipeline_tags: &BTreeSet<String>,
) -> Result<()> {
    let bad = |reason: String| invalid(file, format!("{where_}: {reason}"));
    if let Some(since) = &field.since {
        if semver(since).is_none() {
            return Err(bad(format!("since {since:?} is not semver")));
        }
    }
    match field.kind {
        FieldType::Select => {
            if field.options.is_none() && field.options_from.is_none() {
                return Err(bad("a select needs options or options_from".into()));
            }
            if let Some(OptionsFrom::Parsers) = field.options_from {
                if s.parsers.is_empty() {
                    return Err(bad(
                        "options_from: parsers but the stream has no parsers".into()
                    ));
                }
            }
            if let Some(OptionsFrom::MultilineParsers) = field.options_from {
                if s.multiline_parsers.is_empty() {
                    return Err(bad(
                        "options_from: multiline_parsers but the stream has none".into(),
                    ));
                }
            }
        }
        FieldType::Password => {
            if field.default.is_some() {
                return Err(bad("a password field has no default (§6.1)".into()));
            }
            if field.multi {
                return Err(bad("a password field is not multi".into()));
            }
        }
        FieldType::Yaml => {
            if field.group != Group::Advanced {
                return Err(bad("a yaml field must be in group advanced (§6.1)".into()));
            }
            if !matches!(field.bind, Bind::Merge { merge: true }) {
                return Err(bad(
                    "a yaml field binds with {merge: true} only (§6.2)".into()
                ));
            }
        }
        _ => {}
    }
    if matches!(field.bind, Bind::Merge { .. }) && field.kind != FieldType::Yaml {
        return Err(bad("{merge: true} is only for type yaml (§6.2)".into()));
    }
    // R8: the default has the field's type; a select default is an option.
    if let Some(d) = &field.default {
        let items: Vec<&serde_yaml::Value> = match d {
            serde_yaml::Value::Sequence(items) => {
                if !field.multi {
                    return Err(bad(
                        "default is a list but the field is not multi (R8)".into()
                    ));
                }
                items.iter().collect()
            }
            other => vec![other],
        };
        for item in items {
            check_scalar_type(field.kind, item).map_err(|r| bad(format!("default: {r} (R8)")))?;
            if field.kind == FieldType::Select {
                if let Some(opts) = &field.options {
                    if !opts.iter().any(|o| o.value() == item) {
                        return Err(bad("default is not one of options (R8)".into()));
                    }
                }
            }
        }
    }
    if field.min.is_some() || field.max.is_some() {
        if !matches!(field.kind, FieldType::Integer | FieldType::Number) {
            return Err(bad("min/max only on integer or number".into()));
        }
        if let (Some(lo), Some(hi)) = (field.min, field.max) {
            if lo > hi {
                return Err(bad("min is greater than max".into()));
            }
        }
    }
    // R15 + §6.2 placement.
    match &field.bind {
        Bind::Key { join, repeat, .. } => {
            if field.multi && join.is_none() && !repeat.unwrap_or(false) {
                return Err(bad(
                    "a multi field bound to a key needs join or repeat (R6)".into(),
                ));
            }
            if !field.multi && (join.is_some() || repeat.unwrap_or(false)) {
                return Err(bad("join/repeat only make sense on a multi field".into()));
            }
            if matches!(scope, FieldScope::Backend) {
                return Err(bad("a backend field cannot bind to a plugin key".into()));
            }
        }
        Bind::Merge { merge } => {
            if !merge {
                return Err(bad("merge must be true".into()));
            }
        }
        Bind::Template { template } => {
            if !matches!(scope, FieldScope::Backend) {
                return Err(bad("template bindings belong to backend fields".into()));
            }
            if s.backend
                .as_ref()
                .is_none_or(|b| b.index_template.is_none())
            {
                return Err(bad(
                    "template binding on a stream without index_template (R15)".into(),
                ));
            }
            if !template.starts_with("settings.") && !template.starts_with("mappings.") {
                return Err(bad(format!(
                    "template path {template:?} must start with settings. or mappings."
                )));
            }
        }
        Bind::PipelineToggle { pipeline_toggle } => {
            if field.kind != FieldType::Bool {
                return Err(bad("pipeline_toggle only on a bool field (R15)".into()));
            }
            if !pipeline_tags.contains(pipeline_toggle) {
                return Err(bad(format!(
                    "pipeline_toggle {pipeline_toggle:?} is not a processor tag of the ingest pipeline (R15)"
                )));
            }
        }
        Bind::PipelineParam { pipeline_param } => {
            let tag = pipeline_param.split('.').next().unwrap_or("");
            if !pipeline_tags.contains(tag) {
                return Err(bad(format!(
                    "pipeline_param {pipeline_param:?}: tag {tag:?} is not a processor tag of the ingest pipeline (R15)"
                )));
            }
        }
        Bind::Lifecycle { .. } => {
            if s.backend.as_ref().is_none_or(|b| b.lifecycle.is_none()) {
                return Err(bad(
                    "lifecycle binding on a stream without backend.lifecycle (R15)".into(),
                ));
            }
        }
    }
    Ok(())
}

/// Does `v` have the type `kind` asks for (R8 for defaults, §5 for values)?
pub fn check_scalar_type(
    kind: FieldType,
    v: &serde_yaml::Value,
) -> std::result::Result<(), String> {
    use serde_yaml::Value as Y;
    let want = |what: &str| Err(format!("expected {what}, got {}", yaml_kind(v)));
    match kind {
        FieldType::String | FieldType::Regex | FieldType::Password | FieldType::Text => match v {
            Y::String(s) => {
                if kind != FieldType::Text && has_control_chars(s) {
                    return Err("control character in value".into());
                }
                Ok(())
            }
            _ => want("a string"),
        },
        FieldType::Select => match v {
            Y::String(_) | Y::Number(_) | Y::Bool(_) => Ok(()),
            _ => want("a scalar"),
        },
        FieldType::Integer => match v {
            Y::Number(n) if n.is_i64() || n.is_u64() => Ok(()),
            _ => want("an integer"),
        },
        FieldType::Number => match v {
            Y::Number(_) => Ok(()),
            _ => want("a number"),
        },
        FieldType::Bool => match v {
            Y::Bool(_) => Ok(()),
            _ => want("a bool"),
        },
        FieldType::Duration => match v {
            Y::String(s) if is_duration(s) => Ok(()),
            _ => want("a duration like 30s, 5m, 1d"),
        },
        FieldType::Size => match v {
            Y::String(s) if is_size(s) => Ok(()),
            _ => want("a size like 5MB, 10gb"),
        },
        FieldType::Yaml => match v {
            Y::Mapping(_) => Ok(()),
            _ => want("an object"),
        },
    }
}

fn yaml_kind(v: &serde_yaml::Value) -> &'static str {
    match v {
        serde_yaml::Value::Null => "null",
        serde_yaml::Value::Bool(_) => "a bool",
        serde_yaml::Value::Number(_) => "a number",
        serde_yaml::Value::String(_) => "a string",
        serde_yaml::Value::Sequence(_) => "a list",
        serde_yaml::Value::Mapping(_) => "an object",
        serde_yaml::Value::Tagged(_) => "a tagged value",
    }
}

/// The `tag` of every processor in an ingest-pipeline body.
fn pipeline_processor_tags(pipeline: &serde_json::Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Some(processors) = pipeline.get("processors").and_then(|p| p.as_array()) else {
        return out;
    };
    for p in processors {
        if let Some(obj) = p.as_object() {
            for (_kind, body) in obj {
                if let Some(tag) = body.get("tag").and_then(|t| t.as_str()) {
                    out.insert(tag.to_string());
                }
            }
        }
    }
    out
}

/// R6 against one plugin: fixed keys and field bindings name real parameters,
/// `operator_settable: false` parameters are set only by `fixed`, `secret`
/// parameters only by `password` fields, multi-shape agrees, and a key is not
/// both fixed and bound.
fn check_bindings_against(
    file: &str,
    where_: &str,
    plugin: &Plugin,
    fixed: &Fixed,
    fields: &[Field],
) -> Result<()> {
    for k in fixed.keys() {
        if plugin.param(k).is_none() {
            return Err(invalid(
                file,
                format!(
                    "{where_}.fixed.{k}: not a parameter of {:?} (R6)",
                    plugin.name
                ),
            ));
        }
    }
    for field in fields {
        let Bind::Key {
            key, join, repeat, ..
        } = &field.bind
        else {
            continue;
        };
        let w = format!("{where_}.{}", field.name);
        let param = plugin.param(key).ok_or_else(|| {
            invalid(
                file,
                format!(
                    "{w}: bind key {key:?} is not a parameter of {:?} (R6)",
                    plugin.name
                ),
            )
        })?;
        if !param.operator_settable {
            return Err(invalid(
                file,
                format!("{w}: {key:?} is not operator-settable; only fixed may set it (R6)"),
            ));
        }
        if param.secret && field.kind != FieldType::Password {
            return Err(invalid(
                file,
                format!(
                    "{w}: {key:?} is a secret parameter; only a password field may bind it (R6)"
                ),
            ));
        }
        if fixed.contains_key(key) {
            return Err(invalid(
                file,
                format!("{w}: {key:?} is also set in fixed (R6)"),
            ));
        }
        if field.multi {
            match param.multi {
                Multi::No => {
                    return Err(invalid(
                        file,
                        format!("{w}: {key:?} takes one value but the field is multi (R6)"),
                    ))
                }
                Multi::Join => {
                    if join.is_none() {
                        return Err(invalid(
                            file,
                            format!("{w}: {key:?} takes a joined string; bind needs join (R6)"),
                        ));
                    }
                }
                Multi::Repeat => {
                    if !repeat.unwrap_or(false) {
                        return Err(invalid(
                            file,
                            format!("{w}: {key:?} is a repeated key; bind needs repeat: true (R6)"),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fluent Bit plugin catalogue (spec §9)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Input,
    Filter,
    Processor,
    ParserFormat,
    MultilineParser,
}

#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Multi {
    #[default]
    No,
    Join,
    Repeat,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Param {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: FieldType,
    pub description: I18n,
    #[serde(default)]
    pub default: Option<serde_yaml::Value>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub multi: Multi,
    #[serde(default)]
    pub join: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<serde_yaml::Value>>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub advanced: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(default = "default_true")]
    pub operator_settable: bool,
}

/// One allow-listed plugin, transcribed from its manual page.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Plugin {
    pub kind: PluginKind,
    pub name: String,
    pub title: I18n,
    #[serde(default)]
    pub description: Option<I18n>,
    pub docs_url: String,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub os: Vec<Os>,
    #[serde(default)]
    pub event_types: Vec<EventType>,
    #[serde(default)]
    pub as_processor: Option<bool>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub verified: Option<bool>,
    pub params: Vec<Param>,
}

impl Plugin {
    pub fn parse(file: &str, yaml: &str) -> Result<Plugin> {
        let p: Plugin = serde_yaml::from_str(yaml).map_err(|source| Spec2Error::Yaml {
            file: file.to_string(),
            source,
        })?;
        if EXCLUDED_PLUGINS.contains(&p.name.as_str()) {
            return Err(invalid(
                file,
                format!("plugin {:?} is excluded from the catalogue (R16)", p.name),
            ));
        }
        if !p.docs_url.starts_with("https://docs.fluentbit.io/") {
            return Err(invalid(file, "docs_url is not the Fluent Bit manual (R16)"));
        }
        Ok(p)
    }

    /// The parameter a key names; a `*` in a catalogue key matches any run of
    /// characters (`rdkafka.*`, `collector.*.scrape_interval`), as the
    /// reference compiler's `fnmatch` does.
    pub fn param(&self, key: &str) -> Option<&Param> {
        self.params.iter().find(|p| p.key == key).or_else(|| {
            self.params
                .iter()
                .find(|p| p.key.contains('*') && glob_match(&p.key, key))
        })
    }

    /// Filters that may be attached to an input as processors (guide §7).
    pub fn runs_as_processor(&self) -> bool {
        match self.kind {
            PluginKind::Processor => true,
            PluginKind::Filter => self.as_processor.unwrap_or(false),
            _ => false,
        }
    }
}

/// The catalogue of one Fluent Bit documentation version
/// (`fluent-bit/<version>/`), by kind and name.
#[derive(Debug, Clone, Default)]
pub struct PluginCatalog {
    pub inputs: BTreeMap<String, Plugin>,
    pub filters: BTreeMap<String, Plugin>,
    pub processors: BTreeMap<String, Plugin>,
    pub parser_formats: BTreeMap<String, Plugin>,
    pub multiline_parsers: BTreeMap<String, Plugin>,
}

impl PluginCatalog {
    pub fn insert(&mut self, p: Plugin) {
        let map = match p.kind {
            PluginKind::Input => &mut self.inputs,
            PluginKind::Filter => &mut self.filters,
            PluginKind::Processor => &mut self.processors,
            PluginKind::ParserFormat => &mut self.parser_formats,
            PluginKind::MultilineParser => &mut self.multiline_parsers,
        };
        map.insert(p.name.clone(), p);
    }

    pub fn input(&self, name: &str) -> Option<&Plugin> {
        self.inputs.get(name)
    }

    /// A processor-chain plugin: a processor, or a filter (which the compiler
    /// attaches as a processor when `as_processor`, else as a global filter).
    pub fn filter_or_processor(&self, name: &str) -> Option<&Plugin> {
        self.processors.get(name).or_else(|| self.filters.get(name))
    }

    pub fn len(&self) -> usize {
        self.inputs.len()
            + self.filters.len()
            + self.processors.len()
            + self.parser_formats.len()
            + self.multiline_parsers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Load `fluent-bit/<version>/{inputs,filters,processors,parsers,multiline}/*.yaml`.
    pub fn load_dir(dir: &Path) -> Result<PluginCatalog> {
        let mut cat = PluginCatalog::default();
        for sub in ["inputs", "filters", "processors", "parsers", "multiline"] {
            let d = dir.join(sub);
            let entries = match std::fs::read_dir(&d) {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(Spec2Error::Io {
                        what: format!("reading {}", d.display()),
                        source,
                    })
                }
            };
            let mut paths: Vec<_> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
                .collect();
            paths.sort();
            for p in paths {
                let text = std::fs::read_to_string(&p).map_err(|source| Spec2Error::Io {
                    what: format!("reading {}", p.display()),
                    source,
                })?;
                let file = format!(
                    "{sub}/{}",
                    p.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                );
                let plugin = Plugin::parse(&file, &text)?;
                let stem = p.file_stem().and_then(|n| n.to_str()).unwrap_or("");
                if plugin.name != stem {
                    return Err(invalid(
                        &file,
                        format!("name {:?} differs from the file name", plugin.name),
                    ));
                }
                cat.insert(plugin);
            }
        }
        Ok(cat)
    }
}

/// The catalogue compiled into this binary (ADR-074 decision 2a): the
/// allowlist a package's plugins and an operator's added processors are
/// checked against. Vendored from the registry by
/// `.github/scripts/vendor-plugin-catalog.sh`, parsed once; a vendored file
/// that does not parse is a build defect, caught by the tests, so this
/// panics rather than returning a half catalogue.
pub fn builtin_catalog() -> &'static PluginCatalog {
    static CAT: std::sync::OnceLock<PluginCatalog> = std::sync::OnceLock::new();
    CAT.get_or_init(|| {
        let mut cat = PluginCatalog::default();
        for (path, yaml) in crate::spec2_catalog::FILES {
            let plugin = Plugin::parse(path, yaml)
                .unwrap_or_else(|e| panic!("vendored plugin catalogue: {e}"));
            cat.insert(plugin);
        }
        cat
    })
}

/// `*` matches any run of characters; everything else is literal.
fn glob_match(pattern: &str, text: &str) -> bool {
    fn rec(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => (0..=t.len()).any(|i| rec(&p[1..], &t[i..])),
            Some(c) => t.first() == Some(c) && rec(&p[1..], &t[1..]),
        }
    }
    rec(pattern.as_bytes(), text.as_bytes())
}

// ---------------------------------------------------------------------------
// packages/index.json (spec §10)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IndexVersion {
    pub version: String,
    pub release: Release,
    /// `<id>/<version>/`, relative to `packages/`.
    pub path: String,
    pub manifest_sha256: String,
    pub min_mesh_version: String,
    pub min_agent_version: String,
    pub min_fluent_bit_version: String,
    pub os: Vec<Os>,
    pub signals: Vec<Signal>,
    pub event_types: Vec<EventType>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IndexPackage {
    pub id: String,
    pub title: I18n,
    pub summary: I18n,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub icon: Option<String>,
    pub latest: String,
    pub versions: Vec<IndexVersion>,
}

/// The generated catalogue. A showcase: the signed manifest of each version
/// is the truth, and `manifest_sha256` is checked against it on fetch.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub spec_version: String,
    pub generated_at: String,
    pub packages: Vec<IndexPackage>,
}

impl Index {
    pub fn parse(json: &str) -> Result<Index> {
        let idx: Index = serde_json::from_str(json).map_err(|source| Spec2Error::Json {
            file: "packages/index.json".into(),
            source,
        })?;
        if idx.spec_version != SPEC_VERSION {
            return Err(invalid(
                "packages/index.json",
                format!(
                    "spec_version {:?} is not {SPEC_VERSION:?}",
                    idx.spec_version
                ),
            ));
        }
        for p in &idx.packages {
            if !is_package_id(&p.id) {
                return Err(invalid(
                    "packages/index.json",
                    format!("{:?} is not a package id", p.id),
                ));
            }
            for v in &p.versions {
                let expected = format!("{}/{}/", p.id, v.version);
                if v.path != expected {
                    return Err(invalid(
                        "packages/index.json",
                        format!(
                            "{}@{}: path {:?} is not {expected:?}",
                            p.id, v.version, v.path
                        ),
                    ));
                }
                if semver(&v.version).is_none() || !is_sha256_hex(&v.manifest_sha256) {
                    return Err(invalid(
                        "packages/index.json",
                        format!(
                            "{}@{}: malformed version or manifest_sha256",
                            p.id, v.version
                        ),
                    ));
                }
            }
        }
        Ok(idx)
    }

    pub fn package(&self, id: &str) -> Option<&IndexPackage> {
        self.packages.iter().find(|p| p.id == id)
    }
}

/// What the consumer can offer (spec §10, ADR-074 decision 9): the core's
/// version, the oldest agent's version and Fluent Bit on the policy (`None`
/// when the policy has no agent yet — then only the core gates), and whether
/// non-GA releases are wanted.
#[derive(Debug, Clone, Default)]
pub struct Requirements<'a> {
    pub mesh_version: &'a str,
    pub agent_version: Option<&'a str>,
    pub fluent_bit_version: Option<&'a str>,
    pub include_prerelease: bool,
}

impl IndexPackage {
    /// The highest version whose floors the requirements satisfy.
    pub fn best_version(&self, req: &Requirements<'_>) -> Option<&IndexVersion> {
        self.versions
            .iter()
            .filter(|v| v.satisfied_by(req))
            .max_by_key(|v| semver(&v.version).unwrap_or([0, 0, 0]))
    }
}

impl IndexVersion {
    pub fn satisfied_by(&self, req: &Requirements<'_>) -> bool {
        if !req.include_prerelease && self.release != Release::Ga {
            return false;
        }
        if !version_ge(req.mesh_version, &self.min_mesh_version) {
            return false;
        }
        if let Some(a) = req.agent_version {
            if !version_ge(a, &self.min_agent_version) {
                return false;
            }
        }
        if let Some(fb) = req.fluent_bit_version {
            if !version_ge(fb, &self.min_fluent_bit_version) {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Changelog
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Change {
    #[serde(rename = "type")]
    pub kind: String,
    pub description: I18n,
    #[serde(default)]
    pub link: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChangelogEntry {
    pub version: String,
    pub changes: Vec<Change>,
}

pub fn parse_changelog(yaml: &str) -> Result<Vec<ChangelogEntry>> {
    serde_yaml::from_str(yaml).map_err(|source| Spec2Error::Yaml {
        file: "changelog.yaml".into(),
        source,
    })
}

// ---------------------------------------------------------------------------
// Activation (spec §13) — what a policy revision stores per package
// ---------------------------------------------------------------------------

/// One processor of an activated chain: a package default by `id`, or one the
/// operator added by `plugin`.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(untagged)]
pub enum ProcessorActivation {
    Default {
        id: String,
        /// Absent = the package's own `enabled` (spec §13: a default item
        /// named by id alone runs as the package ships it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(default)]
        fields: BTreeMap<String, serde_json::Value>,
    },
    Added {
        plugin: String,
        params: BTreeMap<String, serde_json::Value>,
    },
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct StreamActivation {
    pub enabled: bool,
    #[serde(default)]
    pub input: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parser: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processors: Vec<ProcessorActivation>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub backend: BTreeMap<String, serde_json::Value>,
}

/// `{package_id, package_version, spec_version, streams}`. A stream that is
/// absent is off; an empty activation means the package's `enabled: true`
/// streams with default values.
#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub package_id: String,
    pub package_version: String,
    pub spec_version: String,
    #[serde(default)]
    pub streams: BTreeMap<String, StreamActivation>,
}

impl Activation {
    /// The `{"$secret": id}` reference a stored password value carries.
    pub fn secret_id(v: &serde_json::Value) -> Option<&str> {
        v.as_object()
            .filter(|o| o.len() == 1)
            .and_then(|o| o.get("$secret"))
            .and_then(|s| s.as_str())
    }

    /// Every secret id the activation references, for the Mesh to resolve.
    pub fn secret_ids(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for s in self.streams.values() {
            for v in s.input.values().chain(s.backend.values()) {
                if let Some(id) = Activation::secret_id(v) {
                    out.insert(id.to_string());
                }
            }
            for p in &s.processors {
                let values = match p {
                    ProcessorActivation::Default { fields, .. } => fields,
                    ProcessorActivation::Added { params, .. } => params,
                };
                for v in values.values() {
                    if let Some(id) = Activation::secret_id(v) {
                        out.insert(id.to_string());
                    }
                }
            }
        }
        out
    }

    /// The environment variable a secret compiles to: `${VELOX_SECRET_<ID>}`,
    /// the id upper-cased with `-` as `_` (guide §6, `render.py`).
    pub fn secret_env(id: &str) -> String {
        format!("VELOX_SECRET_{}", id.to_ascii_uppercase().replace('-', "_"))
    }
}

// ---------------------------------------------------------------------------
// Names (ADR-074 decision 3) and context references (decision 7)
// ---------------------------------------------------------------------------

/// `veloxagent-<stream>.<integration>-v<major>`: the data stream, the component
/// templates (`@package`, `@custom`), the ingest pipeline, the ISM policy, the
/// index template and the index pattern of one stream.
pub fn artifact_name(stream: &str, integration: &str, major: u64) -> String {
    format!("{ARTIFACT_PREFIX}-{stream}.{integration}-v{major}")
}

/// The version-agnostic family readers key on.
pub fn artifact_family(stream: &str, integration: &str) -> String {
    format!("{ARTIFACT_PREFIX}-{stream}.{integration}-v*")
}

/// `veloxagent-<integration>-v<major>-<id>`: a package's saved object, which
/// may span streams.
pub fn saved_object_id(integration: &str, major: u64, local_id: &str) -> String {
    format!("{ARTIFACT_PREFIX}-{integration}-v{major}-{local_id}")
}

/// The Fluent Bit tag of a stream.
pub fn stream_tag(integration: &str, stream: &str) -> String {
    format!("velox.{integration}.{stream}")
}

/// A package parser's name in the compiled configuration — prefixed so two
/// integrations in one policy never collide.
pub fn parser_name(integration: &str, stream: &str, name: &str) -> String {
    format!("{integration}.{stream}.{name}")
}

/// The resolved context of one stream, for `fixed:` strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub stream_tag: String,
    pub artifact: String,
    /// What `{state_dir}` resolves to — [`STATE_DIR_REF`] in the core's
    /// compiler, a literal path in the reference compiler's examples.
    pub state_dir: String,
    pub integration: String,
    pub stream: String,
    pub package_version: String,
}

impl Context {
    pub fn for_stream(pkg: &Package, stream: &Stream) -> Context {
        Context {
            stream_tag: pkg.stream_tag(stream),
            artifact: pkg.artifact_name(stream),
            state_dir: STATE_DIR_REF.to_string(),
            integration: pkg.manifest.id.clone(),
            stream: stream.id.clone(),
            package_version: pkg.manifest.version.clone(),
        }
    }

    fn get(&self, name: &str) -> Option<&str> {
        Some(match name {
            "stream_tag" => &self.stream_tag,
            "artifact" => &self.artifact,
            "state_dir" => &self.state_dir,
            "integration" => &self.integration,
            "stream" => &self.stream,
            "package_version" => &self.package_version,
            _ => return None,
        })
    }
}

/// Substitute the closed references in a `fixed:` string. Fails closed on
/// anything else, exactly as [`check_context_refs`] does at parse time.
pub fn resolve_refs(text: &str, ctx: &Context) -> std::result::Result<String, String> {
    check_context_refs(text)?;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find('}').expect("checked balanced");
        let name = &after[..end];
        out.push_str(ctx.get(name).expect("checked in set"));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Lexical helpers (the schema's patterns, spelled out)
// ---------------------------------------------------------------------------

/// `MAJOR.MINOR.PATCH`, ignoring pre-release and build metadata.
pub fn semver(s: &str) -> Option<[u64; 3]> {
    let core = s.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut out = [0u64; 3];
    for slot in &mut out {
        let p = parts.next()?;
        if p.is_empty()
            || (p.len() > 1 && p.starts_with('0'))
            || !p.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        *slot = p.parse().ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    Some(out)
}

/// `left >= right` on the numeric core; unparsable sides compare as 0.0.0.
pub fn version_ge(left: &str, right: &str) -> bool {
    semver(left).unwrap_or([0, 0, 0]) >= semver(right).unwrap_or([0, 0, 0])
}

pub fn is_package_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 48
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

pub fn is_stream_id(s: &str) -> bool {
    let mut it = s.bytes();
    matches!(it.next(), Some(b) if b.is_ascii_lowercase())
        && s.len() <= 32
        && it.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

pub fn is_local_id(s: &str) -> bool {
    let mut it = s.bytes();
    matches!(it.next(), Some(b) if b.is_ascii_lowercase())
        && s.len() <= 48
        && it.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub fn is_plugin_name(s: &str) -> bool {
    let mut it = s.bytes();
    matches!(it.next(), Some(b) if b.is_ascii_lowercase())
        && it.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn is_plugin_key(s: &str) -> bool {
    let mut it = s.bytes();
    matches!(it.next(), Some(b) if b.is_ascii_alphabetic())
        && it.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

/// A file name inside a stream directory: no separators, no traversal.
pub fn is_asset_file(s: &str) -> bool {
    let mut it = s.bytes();
    matches!(it.next(), Some(b) if b.is_ascii_alphanumeric())
        && it.all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// A path relative to the package root: every segment starts alphanumeric, so
/// `..`, absolute paths and hidden files cannot be spelled.
pub fn is_relpath(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && s.split('/').all(is_asset_file)
}

pub fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `^[0-9]+(ms|s|m|h|d)$`
pub fn is_duration(s: &str) -> bool {
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    digits > 0 && matches!(&s[digits..], "ms" | "s" | "m" | "h" | "d")
}

/// `^[0-9]+([kKmMgGtT][bB]?|[bB])?$`
pub fn is_size(s: &str) -> bool {
    let digits = s.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    let unit = &s[digits..];
    unit.is_empty()
        || matches!(
            unit.to_ascii_lowercase().as_str(),
            "k" | "kb" | "m" | "mb" | "g" | "gb" | "t" | "tb" | "b"
        )
}

/// An OpenSearch index name: lower-case, no `\ / * ? " < > | , #`, no space,
/// not starting with `- _ +`, not `.` or `..`.
pub fn is_index_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && !s.starts_with(['-', '_', '+'])
        && s.chars().all(|c| {
            !c.is_uppercase()
                && !c.is_whitespace()
                && !matches!(
                    c,
                    '\\' | '/' | '*' | '?' | '"' | '<' | '>' | '|' | ',' | '#' | ':'
                )
        })
}

pub fn has_control_chars(s: &str) -> bool {
    s.chars().any(|c| c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry checkout root (`VELOX_REGISTRY_PATH`), or a loud skip — the
    /// same convention as the core's `registry_golden` gates.
    fn registry_root() -> Option<std::path::PathBuf> {
        match std::env::var("VELOX_REGISTRY_PATH") {
            Ok(p) if !p.trim().is_empty() => Some(std::path::PathBuf::from(p)),
            _ => {
                eprintln!("SKIPPED spec2 registry gate: VELOX_REGISTRY_PATH is not set");
                None
            }
        }
    }

    const STREAM_MIN: &str = r#"
id: log
title: {en: Log files}
signal: logs
event_type: logs
input:
  plugin: tail
  fixed:
    db: "{state_dir}/tail-{stream_tag}.db"
  fields:
    - name: paths
      type: string
      multi: true
      required: true
      default: [/var/log/*.log]
      title: {en: Paths}
      bind: {key: path, join: ","}
    - name: multiline
      type: select
      options: [none, docker, cri]
      default: none
      title: {en: Multiline}
      bind: {key: multiline.parser, omit_if: none}
processors:
  defaults:
    - id: exclude
      plugin: grep
      enabled: false
      title: {en: Exclusion rules}
      fields:
        - name: rules
          type: string
          multi: true
          title: {en: Rules}
          bind: {key: exclude, repeat: true}
  allow: [grep, modify]
backend:
  index_template: index-template.json
  ingest_pipeline: ingest-pipeline.json
  lifecycle: {rollover_max_size: 10gb, delete_after: 30d}
  fields:
    - name: replicas
      type: integer
      default: 1
      min: 0
      title: {en: Replicas}
      bind: {template: settings.index.number_of_replicas}
    - name: keep_original
      type: bool
      default: true
      title: {en: Keep original}
      bind: {pipeline_toggle: remove_original}
"#;

    const MANIFEST_MIN: &str = r#"
spec_version: "2.0"
id: custom-logs
version: 1.2.3
release: ga
title: {en: Custom logs}
summary: {en: Any log file.}
categories: [custom]
compatibility:
  min_mesh_version: 0.15.0
  min_agent_version: 0.2.0
  min_fluent_bit_version: 3.2.0
  os: [linux, windows, darwin]
streams: [log]
files:
  - {path: streams/log/stream.yaml, sha256: 0000000000000000000000000000000000000000000000000000000000000000}
  - {path: streams/log/index-template.json, sha256: 0000000000000000000000000000000000000000000000000000000000000000}
  - {path: streams/log/ingest-pipeline.json, sha256: 0000000000000000000000000000000000000000000000000000000000000000}
signature:
  algorithm: ed25519
  key_id: velox-registry-2026-10
  value: AAAA
"#;

    fn files(stream: &str) -> BTreeMap<String, Vec<u8>> {
        let mut f = BTreeMap::new();
        f.insert(
            "streams/log/stream.yaml".to_string(),
            stream.as_bytes().to_vec(),
        );
        f.insert(
            "streams/log/index-template.json".to_string(),
            br#"{"template":{"settings":{"index.number_of_shards":1},"mappings":{"properties":{}}}}"#.to_vec(),
        );
        f.insert(
            "streams/log/ingest-pipeline.json".to_string(),
            br#"{"processors":[{"remove":{"tag":"remove_original","field":"log"}}]}"#.to_vec(),
        );
        f
    }

    #[test]
    fn a_minimal_package_parses_and_names_its_artifacts() {
        let pkg = Package::from_files(MANIFEST_MIN, files(STREAM_MIN)).expect("parses");
        let s = pkg.stream("log").unwrap();
        assert_eq!(pkg.artifact_name(s), "veloxagent-log.custom-logs-v1");
        assert_eq!(pkg.stream_tag(s), "velox.custom-logs.log");
        assert_eq!(pkg.manifest.major(), 1);
        let ctx = Context::for_stream(&pkg, s);
        assert_eq!(
            resolve_refs("{state_dir}/tail-{stream_tag}.db", &ctx).unwrap(),
            "${VELOX_STATE_DIR}/tail-velox.custom-logs.log.db"
        );
        assert_eq!(
            saved_object_id("custom-logs", 1, "overview"),
            "veloxagent-custom-logs-v1-overview"
        );
        assert_eq!(
            artifact_family("log", "custom-logs"),
            "veloxagent-log.custom-logs-v*"
        );
        assert_eq!(
            parser_name("custom-logs", "log", "json"),
            "custom-logs.log.json"
        );
    }

    /// Strict parsing: an unknown key anywhere is a refusal, not a shrug.
    #[test]
    fn unknown_keys_are_refused() {
        let m = MANIFEST_MIN.replace("release: ga", "release: ga\noutput: {host: x}");
        let err = Package::from_files(&m, files(STREAM_MIN))
            .unwrap_err()
            .to_string();
        assert!(err.contains("manifest.yaml"), "{err}");
        let s = STREAM_MIN.replace("plugin: tail", "plugin: tail\n  tag: mine");
        let err = Package::from_files(MANIFEST_MIN, files(&s))
            .unwrap_err()
            .to_string();
        assert!(err.contains("stream.yaml") && err.contains("tag"), "{err}");
    }

    #[test]
    fn the_rules_the_schema_cannot_say_are_enforced() {
        // R7: a foreign reference and `${…}` in fixed.
        let s = STREAM_MIN.replace("{state_dir}/tail-{stream_tag}.db", "{home}/x.db");
        let e = Package::from_files(MANIFEST_MIN, files(&s))
            .unwrap_err()
            .to_string();
        assert!(e.contains("{home}") && e.contains("R7"), "{e}");
        let s = STREAM_MIN.replace("{state_dir}/tail-{stream_tag}.db", "${HOME}/x.db");
        let e = Package::from_files(MANIFEST_MIN, files(&s))
            .unwrap_err()
            .to_string();
        assert!(e.contains("R7"), "{e}");
        // R15: a pipeline_toggle whose tag the pipeline does not carry.
        let s = STREAM_MIN.replace("pipeline_toggle: remove_original", "pipeline_toggle: nope");
        let e = Package::from_files(MANIFEST_MIN, files(&s))
            .unwrap_err()
            .to_string();
        assert!(e.contains("nope") && e.contains("R15"), "{e}");
        // R8: a select default outside its options.
        let s = STREAM_MIN.replace("default: none\n", "default: java\n");
        let e = Package::from_files(MANIFEST_MIN, files(&s))
            .unwrap_err()
            .to_string();
        assert!(e.contains("R8"), "{e}");
        // R13: duplicate processor ids.
        let s = STREAM_MIN.replace(
            "  allow: [grep, modify]",
            "    - id: exclude\n      plugin: grep\n      title: {en: Again}\n  allow: [grep]",
        );
        let e = Package::from_files(MANIFEST_MIN, files(&s))
            .unwrap_err()
            .to_string();
        assert!(e.contains("R13"), "{e}");
        // Spec §9: a code-executing plugin.
        let s = STREAM_MIN.replace("plugin: tail", "plugin: exec");
        let e = Package::from_files(MANIFEST_MIN, files(&s))
            .unwrap_err()
            .to_string();
        assert!(e.contains("exec") && e.contains("refused"), "{e}");
        // R11: a logs stream without a backend.
        let cut = STREAM_MIN.find("backend:").unwrap();
        let e = Package::from_files(MANIFEST_MIN, files(&STREAM_MIN[..cut]))
            .unwrap_err()
            .to_string();
        assert!(e.contains("R11"), "{e}");
        // R4: Fluent Bit floor below 3.2.
        let m = MANIFEST_MIN.replace(
            "min_fluent_bit_version: 3.2.0",
            "min_fluent_bit_version: 3.1.9",
        );
        let e = Package::from_files(&m, files(STREAM_MIN))
            .unwrap_err()
            .to_string();
        assert!(e.contains("R4"), "{e}");
        // R3: a stream directory the manifest does not list.
        let mut f = files(STREAM_MIN);
        f.insert("streams/other/stream.yaml".into(), b"id: other".to_vec());
        let m = MANIFEST_MIN.replace(
            "files:\n",
            "files:\n  - {path: streams/other/stream.yaml, sha256: 0000000000000000000000000000000000000000000000000000000000000000}\n",
        );
        let e = Package::from_files(&m, f).unwrap_err().to_string();
        assert!(e.contains("R3"), "{e}");
    }

    #[test]
    fn the_catalogue_rules_hold() {
        let tail = Plugin::parse(
            "inputs/tail.yaml",
            r#"
kind: input
name: tail
title: {en: Tail}
docs_url: https://docs.fluentbit.io/manual/data-pipeline/inputs/tail
os: [linux, windows, darwin]
event_types: [logs]
params:
  - {key: path, type: string, multi: join, join: ",", description: {en: Files}}
  - {key: db, type: string, operator_settable: false, description: {en: Offsets}}
  - {key: multiline.parser, type: string, multi: join, join: ",", description: {en: Multiline}}
"#,
        )
        .unwrap();
        let grep = Plugin::parse(
            "filters/grep.yaml",
            r#"
kind: filter
name: grep
title: {en: Grep}
docs_url: https://docs.fluentbit.io/manual/data-pipeline/filters/grep
event_types: [logs]
as_processor: true
params:
  - {key: exclude, type: regex, multi: repeat, description: {en: Drop}}
  - {key: regex, type: regex, multi: repeat, description: {en: Keep}}
"#,
        )
        .unwrap();
        let modify = Plugin::parse(
            "filters/modify.yaml",
            r#"
kind: filter
name: modify
title: {en: Modify}
docs_url: https://docs.fluentbit.io/manual/data-pipeline/filters/modify
event_types: [logs]
as_processor: true
params:
  - {key: add, type: string, multi: repeat, description: {en: Add}}
  - {key: rdkafka.*, type: string, description: {en: Any rdkafka key}}
"#,
        )
        .unwrap();
        assert!(modify.param("rdkafka.sasl.username").is_some());
        assert!(modify.param("other").is_none());
        let mut cat = PluginCatalog::default();
        cat.insert(tail);
        cat.insert(grep);
        cat.insert(modify);
        let pkg = Package::from_files(MANIFEST_MIN, files(STREAM_MIN)).unwrap();
        pkg.check_catalog(&cat)
            .expect("the minimal stream binds only real keys");

        // R6: a field bound to a non-operator-settable key.
        let s = STREAM_MIN.replace(
            "bind: {key: path, join: \",\"}",
            "bind: {key: db, join: \",\"}",
        );
        let pkg = Package::from_files(MANIFEST_MIN, files(&s)).unwrap();
        let e = pkg.check_catalog(&cat).unwrap_err().to_string();
        assert!(e.contains("operator-settable"), "{e}");
        // R9: an input that does not emit the declared event type.
        let s = STREAM_MIN
            .replace("event_type: logs", "event_type: metrics")
            .replace("backend:", "backend_x:");
        let s = s.replace("backend_x:", "not_backend:");
        // (metrics streams carry no backend; drop it entirely)
        let cut = s.find("not_backend:").unwrap();
        let pkg = Package::from_files(MANIFEST_MIN, files(&s[..cut])).unwrap();
        let e = pkg.check_catalog(&cat).unwrap_err().to_string();
        assert!(e.contains("R9"), "{e}");
        // R12: an allowed processor that is not catalogued.
        let s = STREAM_MIN.replace("allow: [grep, modify]", "allow: [grep, lua]");
        let e = Package::from_files(MANIFEST_MIN, files(&s))
            .unwrap_err()
            .to_string();
        assert!(e.contains("lua"), "{e}");
        let s = STREAM_MIN.replace("allow: [grep, modify]", "allow: [grep, nest]");
        let pkg = Package::from_files(MANIFEST_MIN, files(&s)).unwrap();
        let e = pkg.check_catalog(&cat).unwrap_err().to_string();
        assert!(e.contains("nest") && e.contains("R12"), "{e}");
    }

    #[test]
    fn index_versions_are_gated_by_the_floors() {
        let idx = Index::parse(
            r#"{"spec_version":"2.0","generated_at":"2026-11-12T00:00:00Z","packages":[
              {"id":"nginx","title":{"en":"Nginx"},"summary":{"en":"x"},"categories":["web"],"latest":"1.1.0","versions":[
                {"version":"1.0.0","release":"ga","path":"nginx/1.0.0/","manifest_sha256":"0000000000000000000000000000000000000000000000000000000000000000",
                 "min_mesh_version":"0.15.0","min_agent_version":"0.2.0","min_fluent_bit_version":"3.2.0","os":["linux"],"signals":["logs"],"event_types":["logs"]},
                {"version":"1.1.0","release":"ga","path":"nginx/1.1.0/","manifest_sha256":"0000000000000000000000000000000000000000000000000000000000000000",
                 "min_mesh_version":"0.15.0","min_agent_version":"0.2.0","min_fluent_bit_version":"4.0.0","os":["linux"],"signals":["logs"],"event_types":["logs"]},
                {"version":"2.0.0","release":"beta","path":"nginx/2.0.0/","manifest_sha256":"0000000000000000000000000000000000000000000000000000000000000000",
                 "min_mesh_version":"0.15.0","min_agent_version":"0.2.0","min_fluent_bit_version":"3.2.0","os":["linux"],"signals":["logs"],"event_types":["logs"]}
              ]}]}"#,
        )
        .unwrap();
        let p = idx.package("nginx").unwrap();
        let req = Requirements {
            mesh_version: "0.20.0",
            agent_version: Some("0.2.0"),
            fluent_bit_version: Some("3.2.5"),
            include_prerelease: false,
        };
        assert_eq!(
            p.best_version(&req).unwrap().version,
            "1.0.0",
            "4.0.0 floor and beta excluded"
        );
        let req = Requirements {
            fluent_bit_version: Some("4.1.0"),
            ..req.clone()
        };
        assert_eq!(p.best_version(&req).unwrap().version, "1.1.0");
        let req = Requirements {
            include_prerelease: true,
            ..req.clone()
        };
        assert_eq!(p.best_version(&req).unwrap().version, "2.0.0");
        let req = Requirements {
            mesh_version: "0.1.0",
            ..req.clone()
        };
        assert!(p.best_version(&req).is_none());
        // No agent yet: only the core gates.
        let req = Requirements {
            mesh_version: "0.20.0",
            agent_version: None,
            fluent_bit_version: None,
            include_prerelease: false,
        };
        assert_eq!(p.best_version(&req).unwrap().version, "1.1.0");
    }

    #[test]
    fn activations_round_trip_and_name_their_secrets() {
        let a: Activation = serde_json::from_str(
            r#"{"package_id":"custom-logs","package_version":"1.0.0","spec_version":"2.0","streams":{"log":{
              "enabled":true,"input":{"paths":["/srv/app/*.log"],"token":{"$secret":"api-token"}},"parser":"json",
              "processors":[{"id":"exclude","enabled":true,"fields":{"rules":["log ^DEBUG"]}},{"plugin":"modify","params":{"add":["env prod"]}}],
              "backend":{"replicas":1}}}}"#,
        )
        .unwrap();
        assert_eq!(
            a.secret_ids().into_iter().collect::<Vec<_>>(),
            vec!["api-token".to_string()]
        );
        assert_eq!(
            Activation::secret_env("api-token"),
            "VELOX_SECRET_API_TOKEN"
        );
        let s = &a.streams["log"];
        assert!(
            matches!(s.processors[0], ProcessorActivation::Default { ref id, enabled: Some(true), .. } if id == "exclude")
        );
        assert!(
            matches!(s.processors[1], ProcessorActivation::Added { ref plugin, .. } if plugin == "modify")
        );
        let back = serde_json::to_string(&a).unwrap();
        let again: Activation = serde_json::from_str(&back).unwrap();
        assert_eq!(a, again);
    }

    #[test]
    fn lexical_helpers_match_the_schema_patterns() {
        assert_eq!(semver("1.2.3"), Some([1, 2, 3]));
        assert_eq!(semver("1.2.3-rc.1+build"), Some([1, 2, 3]));
        assert_eq!(semver("01.2.3"), None);
        assert_eq!(semver("1.2"), None);
        assert!(
            version_ge("3.2.0", "3.2.0")
                && version_ge("4.0.1", "3.2.0")
                && !version_ge("3.1.9", "3.2.0")
        );
        assert!(
            is_duration("30s") && is_duration("5m") && is_duration("1d") && is_duration("250ms")
        );
        assert!(!is_duration("5 m") && !is_duration("m") && !is_duration("5w"));
        assert!(
            is_size("5MB") && is_size("10gb") && is_size("512") && is_size("1k") && is_size("7b")
        );
        assert!(!is_size("5 MB") && !is_size("MB") && !is_size("5mbs"));
        assert!(
            is_relpath("streams/log/stream.yaml")
                && !is_relpath("../x")
                && !is_relpath("/abs")
                && !is_relpath("a/.hidden")
        );
        assert!(
            is_stream_id("access")
                && is_stream_id("auth_log")
                && !is_stream_id("Access")
                && !is_stream_id("a.b")
        );
        assert!(
            is_index_name("veloxagent-log.custom-logs-v1")
                && !is_index_name("Upper")
                && !is_index_name("-x")
        );
        assert!(
            glob_match("rdkafka.*", "rdkafka.sasl.username")
                && glob_match(
                    "collector.*.scrape_interval",
                    "collector.cpu.scrape_interval"
                )
        );
        assert!(!glob_match("rdkafka.*", "other.key"));
    }

    /// The vendored catalogue parses, holds no excluded plugin, and is
    /// byte-identical to the registry's newest one when a checkout is given.
    #[test]
    fn vendored_catalogue_matches_the_registry() {
        let cat = builtin_catalog();
        assert!(cat.len() >= 80, "{} plugins", cat.len());
        for name in EXCLUDED_PLUGINS {
            assert!(
                cat.input(name).is_none() && cat.filter_or_processor(name).is_none(),
                "{name}"
            );
        }
        assert!(cat.input("tail").is_some() && cat.filter_or_processor("grep").is_some());
        let Some(root) = registry_root() else { return };
        let dir = root.join("fluent-bit").join(crate::spec2_catalog::VERSION);
        let mut on_disk = BTreeMap::new();
        for sub in ["inputs", "filters", "processors", "parsers", "multiline"] {
            for e in std::fs::read_dir(dir.join(sub))
                .into_iter()
                .flatten()
                .flatten()
            {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "yaml") {
                    let rel = format!("{sub}/{}", p.file_name().unwrap().to_str().unwrap());
                    on_disk.insert(rel, std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        let vendored: BTreeMap<String, String> = crate::spec2_catalog::FILES
            .iter()
            .map(|(p, y)| (p.to_string(), y.to_string()))
            .collect();
        assert_eq!(
            vendored.keys().collect::<Vec<_>>(),
            on_disk.keys().collect::<Vec<_>>(),
            "re-vendor: .github/scripts/vendor-plugin-catalog.sh <registry>"
        );
        for (k, v) in &on_disk {
            assert_eq!(&vendored[k], v, "{k} drifted; re-vendor the catalogue");
        }
    }

    /// Every package the registry publishes parses strictly and passes the
    /// catalogue rules against the registry's own plugin catalogue — the
    /// conformance gate for this module (ADR-074 decision 2).
    #[test]
    fn every_registry_package_parses_and_binds_to_the_catalogue() {
        let Some(root) = registry_root() else { return };
        let packages = root.join("packages");
        let index = Index::parse(
            &std::fs::read_to_string(packages.join("index.json")).expect("index.json"),
        )
        .expect("index parses");
        assert!(!index.packages.is_empty());
        let fb = root.join("fluent-bit");
        let version_dir = std::fs::read_dir(&fb)
            .expect("fluent-bit/")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .max()
            .expect("one catalogue version");
        let cat = PluginCatalog::load_dir(&version_dir).expect("catalogue loads");
        assert!(cat.len() >= 80, "catalogue has {} plugins", cat.len());
        let mut checked = 0;
        for p in &index.packages {
            for v in &p.versions {
                let dir = packages.join(&v.path);
                let pkg = Package::load_from_dir(&dir)
                    .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
                assert_eq!(pkg.manifest.id, p.id);
                assert_eq!(pkg.manifest.version, v.version);
                pkg.check_catalog(&cat)
                    .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
                for s in &pkg.streams {
                    if s.event_type.has_opensearch_backend() {
                        assert!(pkg.artifact_name(s).starts_with("veloxagent-"));
                    }
                }
                checked += 1;
            }
        }
        eprintln!("spec2 gate: {checked} package versions parsed and bound");
        assert!(checked >= 30);
    }
}

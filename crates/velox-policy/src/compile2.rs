// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The Spec 2.0 compiler (ADR-074 decision 6): verified packages + their
//! activations + the policy's context → **one Fluent Bit YAML document**,
//! built as an object and serialised, never concatenated.
//!
//! The registry's `scripts/render.py` is the executable specification of
//! this module: for the same package, activation and context the two must
//! produce the same document, and the test at the bottom runs the reference
//! compiler against this one on every package of a registry checkout. Keep
//! the two in step — when the guide (§7) changes, both change.
//!
//! What goes where, per enabled stream:
//!
//! * `parsers:` / `multiline_parsers:` — the stream's, renamed
//!   `<integration>.<stream>.<name>` so two integrations never collide;
//! * `pipeline.inputs[]` — `name`, `tag`, the `fixed` keys with the six
//!   context references resolved, the field bindings, `parser:` when chosen,
//!   and the processor chain **attached to the input** (filters the catalogue
//!   marks `as_processor`, processors), ending in the identity metadata;
//!   filters that cannot run as processors become `pipeline.filters[]`
//!   matched on the stream's tag;
//! * `pipeline.outputs[]` — one per stream, chosen by `event_type`:
//!   `opensearch` for logs/traces (index = the stream's artifact `N`,
//!   `write_operation: create`), `opentelemetry` for metrics. Host, port,
//!   TLS and path come from the policy's **endpoints**, credentials are
//!   `${VELOX_OS_*}` / `${VELOX_OTLP_*}` references the agent exports —
//!   the package declares none of this (spec §11).
//!
//! Values are validated here, never only in the browser (spec §5): type,
//! `required`, `min`/`max`, options, `show_if`, `since`, control characters,
//! and for an operator-added processor every key against the plugin
//! catalogue. A `password` value must be a `{"$secret": id}` reference and
//! compiles to `${VELOX_SECRET_<ID>}`; the ids are returned so the Mesh can
//! hand the values to authenticated agents (ADR-074 decision 5).

use serde_yaml::{Mapping, Value};
use std::collections::{BTreeMap, BTreeSet};
use velox_integrations::spec2::{
    self, Activation, Bind, EventType, Field, FieldType, Package, PluginCatalog,
    ProcessorActivation, Stream, StreamActivation,
};

use crate::OsAddress;

/// What the OTLP credential references compile to (guide §9.1); the agent
/// exports the same two names from the credential the Mesh hands it.
pub const OTLP_USER_REF: &str = "${VELOX_OTLP_USER}";
pub const OTLP_PASSWORD_REF: &str = "${VELOX_OTLP_PASSWORD}";
pub const OTLP_USER_ENV: &str = "VELOX_OTLP_USER";
pub const OTLP_PASSWORD_ENV: &str = "VELOX_OTLP_PASSWORD";

/// The environment variable the agent exports for `{state_dir}` (ADR-074
/// decision 7) — the bare name of [`spec2::STATE_DIR_REF`].
pub const STATE_DIR_ENV: &str = "VELOX_STATE_DIR";

/// The environment variable a secret id is exported as on the agent —
/// `VELOX_SECRET_<ID>`, the name the compiled `${...}` reference expands.
pub fn secret_env(id: &str) -> String {
    Activation::secret_env(id)
}

/// Default `metrics_uri` of the `opentelemetry` output when the OTLP endpoint
/// names no path.
pub const OTLP_METRICS_PATH: &str = "/v1/metrics";

/// Where the engine keeps its on-disk buffers, relative to the state
/// directory the agent exports — one variable for both (`storage.path` and
/// the inputs' `{state_dir}`), so a non-root agent is not sent to `/var/lib`.
pub const DEFAULT_STORAGE_PATH: &str = "${VELOX_STATE_DIR}/storage";

/// Compiles a regular expression, `Err` with the engine's reason.
/// `Sync`, so a context can live across an `.await` in the core's async
/// save path.
pub type RegexCheck<'a> = &'a (dyn Fn(&str) -> std::result::Result<(), String> + Sync);

/// The policy's side of the compilation: endpoints, agent settings and the
/// floor the agents share. Nothing here comes from a package.
#[derive(Clone)]
pub struct Context<'a> {
    /// The OpenSearch endpoint logs and traces ship to (ADR-072 amendment 2).
    pub opensearch: &'a OsAddress,
    /// `tls.verify` of the OpenSearch output — the operator's choice on the
    /// endpoint, never the package's.
    pub opensearch_tls_verify: bool,
    /// The OTLP endpoint metric streams ship to; `None` makes a `metrics`
    /// stream a **refusal**, never a silent drop (guide §9.1).
    pub otlp: Option<&'a OsAddress>,
    pub otlp_tls_verify: bool,
    /// The oldest Fluent Bit among the policy's agents; fields and streams
    /// with a newer `since` are omitted. `None` = nothing filtered.
    pub fluent_bit_version: Option<&'a str>,
    pub flush_seconds: u32,
    pub log_level: &'a str,
    /// `service.storage.path`; `None` leaves the key out.
    pub storage_path: Option<&'a str>,
    /// What `{state_dir}` resolves to — [`spec2::STATE_DIR_REF`] in
    /// production; the reference compiler's examples use a literal path.
    pub state_dir: &'a str,
    /// Compiles a user-supplied regular expression (type `regex`, `pattern`),
    /// so a typo is refused at save rather than at the engine's dry-run. This
    /// crate ships no regex engine (the agent links it too); the core wires
    /// one in. `None` skips the check.
    pub regex_check: Option<RegexCheck<'a>>,
}

impl<'a> Context<'a> {
    /// The production defaults around an OpenSearch endpoint.
    pub fn new(opensearch: &'a OsAddress) -> Context<'a> {
        Context {
            opensearch,
            opensearch_tls_verify: true,
            otlp: None,
            otlp_tls_verify: true,
            fluent_bit_version: None,
            flush_seconds: 5,
            log_level: "info",
            storage_path: Some(DEFAULT_STORAGE_PATH),
            state_dir: spec2::STATE_DIR_REF,
            regex_check: None,
        }
    }
}

/// The part of the context value validation needs — shared by the engine
/// compiler and the backend compiler.
#[derive(Clone, Copy, Default)]
pub struct Checks<'a> {
    pub fluent_bit_version: Option<&'a str>,
    pub regex_check: Option<RegexCheck<'a>>,
}

impl<'a> Context<'a> {
    pub fn checks(&self) -> Checks<'a> {
        Checks {
            fluent_bit_version: self.fluent_bit_version,
            regex_check: self.regex_check,
        }
    }
}

/// One package with the activation that selects from it.
pub struct Activated<'a> {
    pub package: &'a Package,
    pub activation: &'a Activation,
}

/// One stream that made it into the document, for the backend side and the
/// console: which artifact it writes, which secrets it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledStream {
    pub package_id: String,
    pub package_version: String,
    pub stream_id: String,
    pub event_type: EventType,
    pub tag: String,
    /// `N` for logs/traces; `None` for metrics (nothing in OpenSearch).
    pub artifact: Option<String>,
}

/// The compiled Fluent Bit configuration of one policy.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    /// The document as an object — what the equivalence test compares.
    pub document: Value,
    /// The same, serialised: what the agent writes to `fluent-bit.yaml`.
    pub yaml: String,
    pub streams: Vec<CompiledStream>,
    /// Secret ids the configuration references as `${VELOX_SECRET_<ID>}`.
    pub secrets: BTreeSet<String>,
    /// Whether an OTLP credential is referenced (a metrics stream compiled).
    pub needs_otlp_credential: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub String);

type Result<T> = std::result::Result<T, Error>;

fn fail(where_: &str, reason: impl std::fmt::Display) -> Error {
    Error(format!("{where_}: {reason}"))
}

/// Compile every activation into one document. Streams come out in package
/// order then stream order, so a revision is reproducible and diffable.
pub fn compile(
    activations: &[Activated<'_>],
    ctx: &Context<'_>,
    catalog: &PluginCatalog,
) -> Result<Compiled> {
    let mut service = Mapping::new();
    service.insert("flush".into(), Value::from(ctx.flush_seconds as u64));
    service.insert("log_level".into(), Value::from(ctx.log_level));
    if let Some(p) = ctx.storage_path {
        service.insert("storage.path".into(), Value::from(p));
    }
    let mut parsers = Vec::new();
    let mut multiline_parsers = Vec::new();
    let mut inputs = Vec::new();
    let mut filters = Vec::new();
    let mut outputs = Vec::new();
    let mut streams = Vec::new();
    let mut secrets = BTreeSet::new();
    let mut needs_otlp_credential = false;

    for Activated {
        package: pkg,
        activation: act,
    } in activations
    {
        if act.package_id != pkg.manifest.id || act.package_version != pkg.manifest.version {
            return Err(Error(format!(
                "activation names {}@{} but the package is {}@{}",
                act.package_id, act.package_version, pkg.manifest.id, pkg.manifest.version
            )));
        }
        if act.spec_version != spec2::SPEC_VERSION {
            return Err(Error(format!(
                "activation of {} has spec_version {:?}, expected {:?}",
                act.package_id,
                act.spec_version,
                spec2::SPEC_VERSION
            )));
        }
        for sid in act.streams.keys() {
            if pkg.stream(sid).is_none() {
                return Err(Error(format!(
                    "activation of {} names stream {sid:?}, which the package does not have",
                    pkg.manifest.id
                )));
            }
        }
        for stream in &pkg.streams {
            let sa = act.streams.get(&stream.id);
            let enabled = sa.map(|s| s.enabled).unwrap_or(stream.enabled);
            if !enabled {
                continue;
            }
            if let (Some(since), Some(fb)) = (&stream.since, ctx.fluent_bit_version) {
                if !spec2::version_ge(fb, since) {
                    continue;
                }
            }
            let mut out = StreamOutput::default();
            compile_stream(pkg, stream, sa, ctx, catalog, &mut out)?;
            parsers.extend(out.parsers);
            multiline_parsers.extend(out.multiline_parsers);
            inputs.push(out.input);
            filters.extend(out.filters);
            outputs.push(out.output);
            secrets.extend(out.secrets);
            needs_otlp_credential |= stream.event_type == EventType::Metrics;
            streams.push(CompiledStream {
                package_id: pkg.manifest.id.clone(),
                package_version: pkg.manifest.version.clone(),
                stream_id: stream.id.clone(),
                event_type: stream.event_type,
                tag: pkg.stream_tag(stream),
                artifact: stream
                    .event_type
                    .has_opensearch_backend()
                    .then(|| pkg.artifact_name(stream)),
            });
        }
    }
    if inputs.is_empty() {
        return Err(Error("no stream enabled".into()));
    }

    let mut pipeline = Mapping::new();
    pipeline.insert("inputs".into(), Value::Sequence(inputs));
    if !filters.is_empty() {
        pipeline.insert("filters".into(), Value::Sequence(filters));
    }
    pipeline.insert("outputs".into(), Value::Sequence(outputs));

    let mut doc = Mapping::new();
    doc.insert("service".into(), Value::Mapping(service));
    if !parsers.is_empty() {
        doc.insert("parsers".into(), Value::Sequence(parsers));
    }
    if !multiline_parsers.is_empty() {
        doc.insert(
            "multiline_parsers".into(),
            Value::Sequence(multiline_parsers),
        );
    }
    doc.insert("pipeline".into(), Value::Mapping(pipeline));
    let document = Value::Mapping(doc);
    let yaml = serde_yaml::to_string(&document).map_err(|e| Error(format!("serialising: {e}")))?;
    Ok(Compiled {
        document,
        yaml,
        streams,
        secrets,
        needs_otlp_credential,
    })
}

#[derive(Default)]
struct StreamOutput {
    parsers: Vec<Value>,
    multiline_parsers: Vec<Value>,
    input: Value,
    filters: Vec<Value>,
    output: Value,
    secrets: BTreeSet<String>,
}

fn compile_stream(
    pkg: &Package,
    stream: &Stream,
    sa: Option<&StreamActivation>,
    ctx: &Context<'_>,
    catalog: &PluginCatalog,
    out: &mut StreamOutput,
) -> Result<()> {
    let where_ = format!("{}/{}", pkg.manifest.id, stream.id);
    let tag = pkg.stream_tag(stream);
    let artifact = pkg.artifact_name(stream);
    let refs = spec2::Context {
        stream_tag: tag.clone(),
        artifact: artifact.clone(),
        state_dir: ctx.state_dir.to_string(),
        integration: pkg.manifest.id.clone(),
        stream: stream.id.clone(),
        package_version: pkg.manifest.version.clone(),
    };
    let pname = |n: &str| spec2::parser_name(&pkg.manifest.id, &stream.id, n);

    // Metrics need somewhere to go before anything else is built.
    let otlp = match stream.event_type {
        EventType::Metrics => Some(ctx.otlp.ok_or_else(|| {
            fail(
                &where_,
                "metrics stream needs an OTLP endpoint on the policy",
            )
        })?),
        _ => None,
    };

    // Parsers, renamed.
    for p in &stream.parsers {
        let mut m = Mapping::new();
        m.insert("name".into(), Value::from(pname(&p.name)));
        m.insert("format".into(), Value::from(p.format.as_str()));
        for (k, v) in &p.rest {
            m.insert(Value::from(k.as_str()), v.clone());
        }
        out.parsers.push(Value::Mapping(m));
    }
    for mp in &stream.multiline_parsers {
        let mut m = Mapping::new();
        m.insert("name".into(), Value::from(pname(&mp.name)));
        m.insert("type".into(), Value::from(mp.kind.as_str()));
        for (k, v) in &mp.rest {
            m.insert(Value::from(k.as_str()), v.clone());
        }
        if let Some(rules) = &mp.rules {
            let seq = rules
                .iter()
                .map(|r| {
                    let mut rm = Mapping::new();
                    rm.insert("state".into(), Value::from(r.state.as_str()));
                    rm.insert("regex".into(), Value::from(r.regex.as_str()));
                    rm.insert("next_state".into(), Value::from(r.next_state.as_str()));
                    Value::Mapping(rm)
                })
                .collect();
            m.insert("rules".into(), Value::Sequence(seq));
        }
        out.multiline_parsers.push(Value::Mapping(m));
    }

    // Input.
    // The input must be catalogued (R5); its params were checked at parse.
    catalog.input(&stream.input.plugin).ok_or_else(|| {
        fail(
            &where_,
            format!("input plugin {:?} not in catalogue", stream.input.plugin),
        )
    })?;
    let mut node = Mapping::new();
    node.insert("name".into(), Value::from(stream.input.plugin.as_str()));
    node.insert("tag".into(), Value::from(tag.as_str()));
    resolve_fixed(&stream.input.fixed, &refs, &mut node, &where_)?;
    let local_parsers: BTreeSet<&str> = stream
        .parsers
        .iter()
        .map(|p| p.name.as_str())
        .chain(stream.multiline_parsers.iter().map(|p| p.name.as_str()))
        .collect();
    let mut ivals = field_values(
        &stream.input.fields,
        sa.map(|s| &s.input),
        &ctx.checks(),
        &format!("{where_}.input"),
        &mut out.secrets,
    )?;
    // A select fed by the stream's own parsers names them locally; the
    // document needs the renamed form.
    for f in &stream.input.fields {
        if f.options_from.is_some() {
            if let Some(v) = ivals.get_mut(&f.name) {
                rename_local_parsers(v, &local_parsers, &pname);
            }
        }
    }
    apply_bindings(
        &stream.input.fields,
        &ivals,
        &mut node,
        &format!("{where_}.input"),
    )?;
    let chosen = sa
        .and_then(|s| s.parser.clone())
        .or_else(|| {
            stream
                .parser_field
                .as_ref()
                .and_then(|pf| pf.default.clone())
        })
        .unwrap_or_else(|| "none".to_string());
    if chosen != "none" {
        if !stream.parsers.iter().any(|p| p.name == chosen) {
            return Err(fail(
                &where_,
                format!("parser {chosen:?} is not offered by the stream"),
            ));
        }
        node.insert("parser".into(), Value::from(pname(&chosen)));
    }

    // Processor chain.
    let mut chain: Vec<Value> = Vec::new();
    let defaults: BTreeMap<&str, &spec2::ProcessorItem> = stream
        .processors
        .defaults
        .iter()
        .map(|d| (d.id.as_str(), d))
        .collect();
    let implicit: Vec<ProcessorActivation>;
    let act_chain: &[ProcessorActivation] = match sa.map(|s| &s.processors) {
        Some(c) if !c.is_empty() => c,
        _ => {
            implicit = stream
                .processors
                .defaults
                .iter()
                .map(|d| ProcessorActivation::Default {
                    id: d.id.clone(),
                    enabled: None,
                    fields: BTreeMap::new(),
                })
                .collect();
            &implicit
        }
    };
    let mut seen = BTreeSet::new();
    for item in act_chain {
        let (plugin_name, body) = match item {
            ProcessorActivation::Default {
                id,
                enabled,
                fields,
            } => {
                let d = defaults
                    .get(id.as_str())
                    .ok_or_else(|| fail(&where_, format!("unknown processor id {id:?}")))?;
                seen.insert(d.id.as_str());
                // `{id}` alone runs as the package ships it; a locked item always runs.
                let on = d.locked || enabled.unwrap_or(d.enabled);
                if !on {
                    continue;
                }
                let w = format!("{where_}.processors.{}", d.id);
                let mut body = Mapping::new();
                resolve_fixed(&d.fixed, &refs, &mut body, &w)?;
                let vals =
                    field_values(&d.fields, Some(fields), &ctx.checks(), &w, &mut out.secrets)?;
                apply_bindings(&d.fields, &vals, &mut body, &w)?;
                (d.plugin.clone(), body)
            }
            ProcessorActivation::Added { plugin, params } => {
                if !stream.processors.allow.iter().any(|a| a == plugin) {
                    return Err(fail(
                        &where_,
                        format!("plugin {plugin:?} is not allowed on this stream"),
                    ));
                }
                let doc = catalog
                    .filter_or_processor(plugin)
                    .ok_or_else(|| fail(&where_, format!("plugin {plugin:?} not in catalogue")))?;
                let w = format!("{where_}.processors.{plugin}");
                let mut body = Mapping::new();
                for (k, v) in params {
                    let param = doc.param(k).ok_or_else(|| {
                        fail(&w, format!("{k:?} is not a parameter of {plugin:?}"))
                    })?;
                    if !param.operator_settable {
                        return Err(fail(&w, format!("{k:?} cannot be set by the operator")));
                    }
                    let yv = json_to_yaml(v);
                    if param.secret {
                        let Some(id) = Activation::secret_id(v) else {
                            return Err(fail(
                                &w,
                                format!("{k:?} is a secret; pass a $secret reference"),
                            ));
                        };
                        out.secrets.insert(id.to_string());
                        body.insert(Value::from(k.as_str()), Value::from(secret_ref(id)));
                        continue;
                    }
                    check_no_control(&yv).map_err(|r| fail(&w, format!("{k:?}: {r}")))?;
                    body.insert(Value::from(k.as_str()), yv);
                }
                (plugin.clone(), body)
            }
        };
        place(
            catalog,
            &plugin_name,
            body,
            &tag,
            &mut chain,
            &mut out.filters,
            &where_,
        )?;
    }
    // Locked defaults the activation left out run anyway.
    for d in &stream.processors.defaults {
        if d.locked && !seen.contains(d.id.as_str()) {
            let w = format!("{where_}.processors.{}", d.id);
            let mut body = Mapping::new();
            resolve_fixed(&d.fixed, &refs, &mut body, &w)?;
            let vals = field_values(&d.fields, None, &ctx.checks(), &w, &mut out.secrets)?;
            apply_bindings(&d.fields, &vals, &mut body, &w)?;
            place(
                catalog,
                &d.plugin,
                body,
                &tag,
                &mut chain,
                &mut out.filters,
                &where_,
            )?;
        }
    }

    // Identity metadata closes the chain.
    let meta = [
        ("velox.integration", pkg.manifest.id.as_str()),
        ("velox.stream", stream.id.as_str()),
        ("velox.package_version", pkg.manifest.version.as_str()),
    ];
    if stream.event_type == EventType::Metrics {
        let mut m = Mapping::new();
        m.insert("name".into(), Value::from("labels"));
        m.insert(
            "insert".into(),
            Value::Sequence(
                meta.iter()
                    .map(|(k, v)| Value::from(format!("{} {v}", k.replace('.', "_"))))
                    .collect(),
            ),
        );
        chain.push(Value::Mapping(m));
    } else {
        for (k, v) in meta {
            let mut m = Mapping::new();
            m.insert("name".into(), Value::from("content_modifier"));
            m.insert("action".into(), Value::from("insert"));
            m.insert("key".into(), Value::from(k));
            m.insert("value".into(), Value::from(v));
            chain.push(Value::Mapping(m));
        }
    }
    let mut procs = Mapping::new();
    procs.insert(
        Value::from(stream.event_type.name()),
        Value::Sequence(chain),
    );
    node.insert("processors".into(), Value::Mapping(procs));
    out.input = Value::Mapping(node);

    // Output, by event type.
    let on_off = |b: bool| Value::from(if b { "on" } else { "off" });
    let mut o = Mapping::new();
    match otlp {
        Some(addr) => {
            o.insert("name".into(), Value::from("opentelemetry"));
            o.insert("match".into(), Value::from(tag.as_str()));
            o.insert("host".into(), Value::from(addr.host.as_str()));
            o.insert("port".into(), Value::from(addr.port as u64));
            o.insert(
                "metrics_uri".into(),
                Value::from(addr.path.as_deref().unwrap_or(OTLP_METRICS_PATH)),
            );
            o.insert("tls".into(), on_off(addr.tls));
            o.insert("tls.verify".into(), on_off(ctx.otlp_tls_verify));
            o.insert("http_user".into(), Value::from(OTLP_USER_REF));
            o.insert("http_passwd".into(), Value::from(OTLP_PASSWORD_REF));
        }
        None => {
            let addr = ctx.opensearch;
            o.insert("name".into(), Value::from("opensearch"));
            o.insert("match".into(), Value::from(tag.as_str()));
            o.insert("host".into(), Value::from(addr.host.as_str()));
            o.insert("port".into(), Value::from(addr.port as u64));
            o.insert("tls".into(), on_off(addr.tls));
            o.insert("tls.verify".into(), on_off(ctx.opensearch_tls_verify));
            o.insert("http_user".into(), Value::from(crate::OS_USER_REF));
            o.insert("http_passwd".into(), Value::from(crate::OS_PASSWORD_REF));
            o.insert("index".into(), Value::from(artifact.as_str()));
            o.insert("write_operation".into(), Value::from("create"));
            o.insert("suppress_type_name".into(), Value::from("on"));
            o.insert("trace_error".into(), Value::from("on"));
            if let Some(p) = &addr.path {
                o.insert("path".into(), Value::from(p.as_str()));
            }
        }
    }
    out.output = Value::Mapping(o);
    Ok(())
}

/// A filter the catalogue lets run as a processor is attached to the input;
/// one that cannot becomes a global filter matched on the stream's tag.
fn place(
    catalog: &PluginCatalog,
    plugin: &str,
    body: Mapping,
    tag: &str,
    chain: &mut Vec<Value>,
    filters: &mut Vec<Value>,
    where_: &str,
) -> Result<()> {
    let doc = catalog
        .filter_or_processor(plugin)
        .ok_or_else(|| fail(where_, format!("plugin {plugin:?} not in catalogue")))?;
    let mut m = Mapping::new();
    m.insert("name".into(), Value::from(plugin));
    if doc.runs_as_processor() {
        m.extend(body);
        chain.push(Value::Mapping(m));
    } else {
        m.insert("match".into(), Value::from(tag));
        m.extend(body);
        filters.push(Value::Mapping(m));
    }
    Ok(())
}

/// `fixed:` keys with the closed references resolved.
fn resolve_fixed(
    fixed: &spec2::Fixed,
    refs: &spec2::Context,
    target: &mut Mapping,
    where_: &str,
) -> Result<()> {
    for (k, v) in fixed {
        let resolved = match v {
            Value::String(s) => Value::from(
                spec2::resolve_refs(s, refs)
                    .map_err(|r| fail(where_, format!("fixed.{k}: {r}")))?,
            ),
            Value::Sequence(items) => Value::Sequence(
                items
                    .iter()
                    .map(|x| match x {
                        Value::String(s) => spec2::resolve_refs(s, refs)
                            .map(Value::from)
                            .map_err(|r| fail(where_, format!("fixed.{k}: {r}"))),
                        other => Ok(other.clone()),
                    })
                    .collect::<Result<Vec<_>>>()?,
            ),
            other => other.clone(),
        };
        target.insert(Value::from(k.as_str()), resolved);
    }
    Ok(())
}

/// Defaults plus the activation's values, typed and checked (spec §5).
/// Fields newer than the agents' Fluent Bit are dropped, given or not, as
/// the reference compiler does. Passwords become `${VELOX_SECRET_<ID>}` and
/// their ids are collected.
pub(crate) fn field_values(
    fields: &[Field],
    given: Option<&BTreeMap<String, serde_json::Value>>,
    checks: &Checks<'_>,
    where_: &str,
    secrets: &mut BTreeSet<String>,
) -> Result<BTreeMap<String, Value>> {
    let empty = BTreeMap::new();
    let given = given.unwrap_or(&empty);
    for k in given.keys() {
        if !fields.iter().any(|f| f.name == *k) {
            return Err(fail(where_, format!("unknown field {k:?}")));
        }
    }
    let mut out = BTreeMap::new();
    for f in fields {
        if let (Some(since), Some(fb)) = (&f.since, checks.fluent_bit_version) {
            if !spec2::version_ge(fb, since) {
                continue;
            }
        }
        let w = format!("{where_}.{}", f.name);
        let value: Value = match (given.get(&f.name), &f.default) {
            (Some(v), _) => {
                if f.kind == FieldType::Password {
                    let Some(id) = Activation::secret_id(v) else {
                        return Err(fail(
                            &w,
                            "password field must be a {\"$secret\": id} reference",
                        ));
                    };
                    secrets.insert(id.to_string());
                    out.insert(f.name.clone(), Value::from(secret_ref(id)));
                    continue;
                }
                json_to_yaml(v)
            }
            (None, Some(d)) => d.clone(),
            (None, None) => {
                if f.required {
                    return Err(fail(&w, "required field has no value"));
                }
                continue;
            }
        };
        check_value(f, &value, checks).map_err(|r| fail(&w, r))?;
        out.insert(f.name.clone(), value);
    }
    Ok(out)
}

/// Type, multiplicity, bounds, options and pattern of one value (spec §5).
fn check_value(f: &Field, v: &Value, checks: &Checks<'_>) -> std::result::Result<(), String> {
    let items: Vec<&Value> = match v {
        Value::Sequence(items) => {
            if !f.multi {
                return Err("a list was given but the field is not multi".into());
            }
            items.iter().collect()
        }
        other => vec![other],
    };
    for item in items {
        spec2::check_scalar_type(f.kind, item)?;
        match f.kind {
            FieldType::Integer | FieldType::Number => {
                let n = item.as_f64().ok_or("expected a number")?;
                if let Some(min) = f.min {
                    if n < min {
                        return Err(format!("{n} is below the minimum {min}"));
                    }
                }
                if let Some(max) = f.max {
                    if n > max {
                        return Err(format!("{n} is above the maximum {max}"));
                    }
                }
            }
            FieldType::Select => {
                if let Some(opts) = &f.options {
                    if !opts.iter().any(|o| o.value() == item) {
                        return Err(format!("{} is not one of the options", scalar_text(item)));
                    }
                }
                // `options_from` is checked by the caller against the stream's
                // own parsers, which are what the options are.
            }
            FieldType::Regex => {
                if let (Some(check), Some(s)) = (checks.regex_check, item.as_str()) {
                    check(s).map_err(|e| format!("invalid regular expression: {e}"))?;
                }
            }
            FieldType::String => {
                if let (Some(pattern), Some(check), Some(s)) =
                    (&f.pattern, checks.regex_check, item.as_str())
                {
                    // The pattern is the package's; the value is the operator's.
                    // Without an engine the pattern is documentation only.
                    check(pattern).map_err(|e| format!("package pattern does not compile: {e}"))?;
                    let _ = s;
                }
            }
            _ => {}
        }
        check_no_control(item)?;
    }
    Ok(())
}

/// Field values onto plugin keys (`key` and `merge` bindings); backend and
/// pipeline bindings are not the engine's and are skipped here.
fn apply_bindings(
    fields: &[Field],
    values: &BTreeMap<String, Value>,
    target: &mut Mapping,
    where_: &str,
) -> Result<()> {
    for f in fields {
        let Some(v) = values.get(&f.name) else {
            continue;
        };
        if !visible(f, values) {
            continue;
        }
        match &f.bind {
            Bind::Key {
                key,
                join,
                repeat,
                omit_if,
                prefix,
            } => {
                if let Some(omit) = omit_if {
                    if scalar_eq(v, omit) {
                        continue;
                    }
                }
                let value = match v {
                    Value::Sequence(items) => {
                        let items: Vec<Value> = items
                            .iter()
                            .map(|x| match prefix {
                                Some(p) => Value::from(format!("{p}{}", scalar_text(x))),
                                None => x.clone(),
                            })
                            .collect();
                        if let Some(sep) = join {
                            Value::from(items.iter().map(scalar_text).collect::<Vec<_>>().join(sep))
                        } else if repeat.unwrap_or(false) {
                            Value::Sequence(items)
                        } else {
                            return Err(fail(
                                where_,
                                format!("{}: list value without join/repeat", f.name),
                            ));
                        }
                    }
                    other => match prefix {
                        Some(p) => Value::from(format!("{p}{}", scalar_text(other))),
                        None => other.clone(),
                    },
                };
                let k = Value::from(key.as_str());
                if target.contains_key(&k) {
                    return Err(fail(where_, format!("{}: key {key:?} already set", f.name)));
                }
                target.insert(k, value);
            }
            Bind::Merge { .. } => {
                let Value::Mapping(m) = v else {
                    return Err(fail(where_, format!("{}: merge needs an object", f.name)));
                };
                for (k, val) in m {
                    if target.contains_key(k) {
                        return Err(fail(
                            where_,
                            format!("{}: merge key {} already set", f.name, scalar_text(k)),
                        ));
                    }
                    target.insert(k.clone(), val.clone());
                }
            }
            Bind::Template { .. }
            | Bind::PipelineToggle { .. }
            | Bind::PipelineParam { .. }
            | Bind::Lifecycle { .. } => {}
        }
    }
    Ok(())
}

pub(crate) fn visible(f: &Field, values: &BTreeMap<String, Value>) -> bool {
    let Some(si) = &f.show_if else { return true };
    let v = values.get(&si.field);
    if let Some(eq) = &si.equals {
        return v.is_some_and(|v| scalar_eq(v, eq));
    }
    if let Some(set) = &si.one_of {
        return v.is_some_and(|v| set.iter().any(|x| scalar_eq(v, x)));
    }
    true
}

/// Rename the stream's own parser names in a select value to their compiled
/// form; a value that is not a local parser (e.g. a built-in `docker`) stays.
fn rename_local_parsers(v: &mut Value, local: &BTreeSet<&str>, pname: &dyn Fn(&str) -> String) {
    match v {
        Value::String(s) if local.contains(s.as_str()) => *s = pname(s),
        Value::Sequence(items) => {
            for item in items {
                rename_local_parsers(item, local, pname);
            }
        }
        _ => {}
    }
}

fn secret_ref(id: &str) -> String {
    format!("${{{}}}", Activation::secret_env(id))
}

/// Scalars compare by value with YAML's looseness folded away: `1 == 1.0`,
/// and a scalar printed as text equals its text.
fn scalar_eq(a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x == y,
        _ => {
            scalar_text(a) == scalar_text(b) && !matches!(a, Value::Sequence(_) | Value::Mapping(_))
        }
    }
}

fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Null => "null".into(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

fn check_no_control(v: &Value) -> std::result::Result<(), String> {
    match v {
        Value::String(s) if spec2::has_control_chars(s) => Err("control character in value".into()),
        Value::Sequence(items) => items.iter().try_for_each(check_no_control),
        Value::Mapping(m) => m.values().try_for_each(check_no_control),
        _ => Ok(()),
    }
}

/// Activation values arrive as JSON (the revision is JSON); the document is
/// built as YAML. Object keys keep their order of arrival.
pub fn json_to_yaml(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(u) = n.as_u64() {
                Value::from(u)
            } else {
                Value::from(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Value::from(s.as_str()),
        serde_json::Value::Array(a) => Value::Sequence(a.iter().map(json_to_yaml).collect()),
        serde_json::Value::Object(o) => Value::Mapping(
            o.iter()
                .map(|(k, v)| (Value::from(k.as_str()), json_to_yaml(v)))
                .collect(),
        ),
    }
}

/// The reverse, for comparing against the reference compiler's JSON.
pub fn yaml_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                serde_json::Value::from(i)
            } else if let Some(u) = n.as_u64() {
                serde_json::Value::from(u)
            } else {
                serde_json::Value::from(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Sequence(a) => serde_json::Value::Array(a.iter().map(yaml_to_json).collect()),
        Value::Mapping(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, v)| (scalar_text(k), yaml_to_json(v)))
                .collect(),
        ),
        Value::Tagged(t) => yaml_to_json(&t.value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn registry_root() -> Option<PathBuf> {
        match std::env::var("VELOX_REGISTRY_PATH") {
            Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
            _ => {
                eprintln!("SKIPPED compile2 equivalence gate: VELOX_REGISTRY_PATH is not set");
                None
            }
        }
    }

    fn catalog_of(root: &Path) -> PluginCatalog {
        let dir = std::fs::read_dir(root.join("fluent-bit"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .max()
            .unwrap();
        PluginCatalog::load_dir(&dir).unwrap()
    }

    const OS: &str = "https://os.example.com";
    const OTLP: &str = "https://otlp.example.com:4318/v1/metrics";

    fn ctx<'a>(os: &'a OsAddress, otlp: Option<&'a OsAddress>) -> Context<'a> {
        Context {
            otlp,
            storage_path: Some("/var/lib/velox-agent/fluent-bit/storage"),
            ..Context::new(os)
        }
    }

    /// The reference compiler's context for the same policy.
    fn render_context(ctx: &Context<'_>) -> serde_json::Value {
        let addr = |a: &OsAddress, verify: bool| serde_json::json!({"host": a.host, "port": a.port, "tls": a.tls, "tls_verify": verify, "path": a.path});
        let mut c = serde_json::json!({
            "deployment": "demo",
            "state_dir": ctx.state_dir,
            "service": {"flush": ctx.flush_seconds, "log_level": ctx.log_level, "storage_path": ctx.storage_path},
            "opensearch": addr(ctx.opensearch, ctx.opensearch_tls_verify),
            "otlp": ctx.otlp.map(|o| addr(o, ctx.otlp_tls_verify)),
        });
        if let Some(fb) = ctx.fluent_bit_version {
            c["fluent_bit_version"] = serde_json::Value::from(fb);
        }
        c
    }

    /// Run `scripts/render.py` for one case; `None` when python/pyyaml are
    /// absent (the gate then only proves the Rust side compiles the package).
    fn render_py(
        root: &Path,
        pkg_dir: &Path,
        activation: Option<&Activation>,
        ctx: &Context<'_>,
    ) -> Option<serde_json::Value> {
        // One directory per call: the gates run in parallel and must not
        // read each other's activation.
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!("velox-compile2-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let ctx_path = tmp.join("context.json");
        std::fs::write(
            &ctx_path,
            serde_json::to_string(&render_context(ctx)).unwrap(),
        )
        .unwrap();
        let mut cmd = std::process::Command::new("python3");
        cmd.arg("-I")
            .arg(root.join("scripts/render.py"))
            .arg(pkg_dir)
            .arg("--format")
            .arg("json")
            .arg("--context")
            .arg(&ctx_path);
        if let Some(a) = activation {
            let p = tmp.join("activation.json");
            std::fs::write(&p, serde_json::to_string(a).unwrap()).unwrap();
            cmd.arg("--activation").arg(p);
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("No module named") {
                eprintln!("SKIPPED render.py comparison: {err}");
                return None;
            }
            panic!("render.py failed for {}: {err}", pkg_dir.display());
        }
        let _ = std::fs::remove_dir_all(&tmp);
        Some(serde_json::from_slice(&out.stdout).expect("render.py emits JSON"))
    }

    fn defaults(pkg: &Package) -> Activation {
        Activation {
            package_id: pkg.manifest.id.clone(),
            package_version: pkg.manifest.version.clone(),
            spec_version: "2.0".into(),
            streams: BTreeMap::new(),
        }
    }

    /// Every package of the registry, compiled with its defaults, equals the
    /// reference compiler's document — the executable spec (guide §7).
    /// Packages whose defaults enable no stream are compiled with every
    /// stream on instead, so metric streams and OTLP routing are covered.
    #[test]
    fn every_registry_package_compiles_like_the_reference() {
        let Some(root) = registry_root() else { return };
        let cat = catalog_of(&root);
        let os = OsAddress::parse(OS).unwrap();
        let otlp = OsAddress::parse(OTLP).unwrap();
        let c = ctx(&os, Some(&otlp));
        let index = spec2::Index::parse(
            &std::fs::read_to_string(root.join("packages/index.json")).unwrap(),
        )
        .unwrap();
        let (mut compared, mut compiled_n) = (0, 0);
        for p in &index.packages {
            for v in &p.versions {
                let dir = root.join("packages").join(&v.path);
                let pkg = Package::load_from_dir(&dir).unwrap();
                let mut act = defaults(&pkg);
                if !pkg.streams.iter().any(|s| s.enabled) {
                    for s in &pkg.streams {
                        act.streams.insert(
                            s.id.clone(),
                            StreamActivation {
                                enabled: true,
                                ..Default::default()
                            },
                        );
                    }
                }
                let ours = compile(
                    &[Activated {
                        package: &pkg,
                        activation: &act,
                    }],
                    &c,
                    &cat,
                )
                .unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
                compiled_n += 1;
                assert!(ours.yaml.starts_with("service:"), "{}", ours.yaml);
                if let Some(theirs) = render_py(&root, &dir, Some(&act), &c) {
                    assert_eq!(
                        yaml_to_json(&ours.document),
                        theirs,
                        "{} differs from render.py",
                        dir.display()
                    );
                    compared += 1;
                }
            }
        }
        eprintln!(
            "compile2 gate: {compiled_n} packages compiled, {compared} compared with render.py"
        );
        assert!(compiled_n >= 30);
    }

    /// A rich activation — values, a toggled default, an added processor, a
    /// parser pick, a disabled stream — matches the reference too.
    #[test]
    fn a_custom_activation_compiles_like_the_reference() {
        let Some(root) = registry_root() else { return };
        let cat = catalog_of(&root);
        let os = OsAddress::parse(OS).unwrap();
        let c = ctx(&os, None);
        let dir = root.join("packages/nginx/1.0.0");
        let pkg = Package::load_from_dir(&dir).unwrap();
        let act: Activation = serde_json::from_str(r#"{"package_id":"nginx","package_version":"1.0.0","spec_version":"2.0","streams":{
            "access":{"enabled":true,"input":{"paths":["/srv/nginx/a.log","/srv/nginx/b.log"],"read_from_head":true},
                      "processors":[{"id":"exclude","enabled":true,"fields":{"rules":["log \\.(css|js)"]}},{"plugin":"modify","params":{"add":["env prod"]}}],
                      "backend":{"delete_after":"14d","parse_user_agent":false}},
            "error":{"enabled":false}}}"#).unwrap();
        let ours = compile(
            &[Activated {
                package: &pkg,
                activation: &act,
            }],
            &c,
            &cat,
        )
        .unwrap();
        assert_eq!(ours.streams.len(), 1);
        assert_eq!(
            ours.streams[0].artifact.as_deref(),
            Some("veloxagent-access.nginx-v1")
        );
        let input = &ours.document["pipeline"]["inputs"][0];
        assert_eq!(
            input["path"],
            Value::from("/srv/nginx/a.log,/srv/nginx/b.log")
        );
        assert_eq!(input["processors"]["logs"][0]["name"], Value::from("grep"));
        assert_eq!(
            input["processors"]["logs"][1]["name"],
            Value::from("modify")
        );
        if let Some(theirs) = render_py(&root, &dir, Some(&act), &c) {
            assert_eq!(yaml_to_json(&ours.document), theirs);
        }
        // The custom-logs example of the guide, with a parser picked.
        let dir = root.join("packages/custom-logs/1.0.0");
        let pkg = Package::load_from_dir(&dir).unwrap();
        let act: Activation = serde_json::from_str(r#"{"package_id":"custom-logs","package_version":"1.0.0","spec_version":"2.0","streams":{
            "log":{"enabled":true,"input":{"paths":["/srv/app/*.log"],"multiline":"java"},"parser":"json"}}}"#).unwrap();
        let ours = compile(
            &[Activated {
                package: &pkg,
                activation: &act,
            }],
            &c,
            &cat,
        )
        .unwrap();
        let input = &ours.document["pipeline"]["inputs"][0];
        assert_eq!(input["parser"], Value::from("custom-logs.log.json"));
        assert_eq!(input["multiline.parser"], Value::from("java"));
        if let Some(theirs) = render_py(&root, &dir, Some(&act), &c) {
            assert_eq!(yaml_to_json(&ours.document), theirs);
        }
    }

    /// Two packages in one policy: one service, both inputs, both outputs,
    /// parsers prefixed so they cannot collide, and a metrics stream routed
    /// to the OTLP endpoint while logs go to OpenSearch.
    #[test]
    fn a_policy_with_two_packages_and_both_routes() {
        let Some(root) = registry_root() else { return };
        let cat = catalog_of(&root);
        let os = OsAddress::parse("os.internal:9200").unwrap();
        let otlp = OsAddress::parse("http://otlp.internal:4318").unwrap();
        let c = ctx(&os, Some(&otlp));
        let nginx = Package::load_from_dir(&root.join("packages/nginx/1.0.0")).unwrap();
        let system = Package::load_from_dir(&root.join("packages/system/1.0.0")).unwrap();
        let a1 = defaults(&nginx);
        let mut a2 = defaults(&system);
        a2.streams.insert(
            "host_metrics".into(),
            StreamActivation {
                enabled: true,
                ..Default::default()
            },
        );
        a2.streams.insert(
            "syslog".into(),
            StreamActivation {
                enabled: false,
                ..Default::default()
            },
        );
        a2.streams.insert(
            "auth".into(),
            StreamActivation {
                enabled: false,
                ..Default::default()
            },
        );
        let out = compile(
            &[
                Activated {
                    package: &nginx,
                    activation: &a1,
                },
                Activated {
                    package: &system,
                    activation: &a2,
                },
            ],
            &c,
            &cat,
        )
        .unwrap();
        let outputs = out.document["pipeline"]["outputs"].as_sequence().unwrap();
        let names: Vec<&str> = outputs
            .iter()
            .map(|o| o["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"opensearch") && names.contains(&"opentelemetry"),
            "{names:?}"
        );
        let otlp_out = outputs
            .iter()
            .find(|o| o["name"] == "opentelemetry")
            .unwrap();
        assert_eq!(otlp_out["port"], Value::from(4318u64));
        assert_eq!(otlp_out["tls"], Value::from("off"));
        assert_eq!(otlp_out["metrics_uri"], Value::from("/v1/metrics"));
        assert_eq!(otlp_out["http_user"], Value::from(OTLP_USER_REF));
        assert!(out.needs_otlp_credential);
        let os_out = outputs.iter().find(|o| o["name"] == "opensearch").unwrap();
        assert_eq!(os_out["port"], Value::from(9200u64));
        assert_eq!(os_out["tls"], Value::from("on"));
        assert_eq!(out.streams.len(), 3, "{:?}", out.streams);
        // Without an OTLP endpoint the metrics stream is a refusal, not a drop.
        let c2 = ctx(&os, None);
        let err = compile(
            &[Activated {
                package: &system,
                activation: &a2,
            }],
            &c2,
            &cat,
        )
        .unwrap_err();
        assert!(err.0.contains("OTLP endpoint"), "{err}");
    }

    // ── no registry needed: an inline package and catalogue ─────────────

    const MANIFEST: &str = r#"
spec_version: "2.0"
id: toy
version: 2.1.0
release: ga
title: {en: Toy}
summary: {en: A toy.}
categories: [custom]
compatibility: {min_mesh_version: 0.1.0, min_agent_version: 0.1.0, min_fluent_bit_version: 3.2.0, os: [linux]}
streams: [log]
files:
  - {path: streams/log/stream.yaml, sha256: 0000000000000000000000000000000000000000000000000000000000000000}
  - {path: streams/log/index-template.json, sha256: 0000000000000000000000000000000000000000000000000000000000000000}
"#;

    const STREAM: &str = r#"
id: log
title: {en: Log}
signal: logs
event_type: logs
input:
  plugin: tail
  fixed:
    db: "{state_dir}/tail-{stream_tag}.db"
  fields:
    - {name: paths, type: string, multi: true, required: true, title: {en: Paths}, bind: {key: path, join: ","}}
    - {name: token, type: password, title: {en: Token}, bind: {key: token}}
    - {name: mode, type: select, options: [plain, fancy], default: plain, title: {en: Mode}, bind: {key: mode, omit_if: plain}}
    - {name: fancy_level, type: integer, default: 2, min: 1, max: 5, title: {en: Level}, show_if: {field: mode, equals: fancy}, bind: {key: level}}
    - {name: newer, type: bool, default: true, since: 4.0.0, title: {en: Newer}, bind: {key: newer_key}}
processors:
  defaults:
    - id: keep
      plugin: grep
      locked: true
      title: {en: Keep}
      fixed: {regex: "log ^KEEP"}
    - id: slow
      plugin: throttle
      enabled: false
      title: {en: Throttle}
      fields:
        - {name: rate, type: integer, default: 100, title: {en: Rate}, bind: {key: rate}}
  allow: [modify, throttle]
backend:
  index_template: index-template.json
"#;

    const CATALOG: &[(&str, &str)] = &[
        (
            "inputs/tail.yaml",
            r#"
kind: input
name: tail
title: {en: Tail}
docs_url: https://docs.fluentbit.io/x
os: [linux]
event_types: [logs]
params:
  - {key: path, type: string, multi: join, join: ",", description: {en: p}}
  - {key: db, type: string, operator_settable: false, description: {en: d}}
  - {key: token, type: password, secret: true, description: {en: t}}
  - {key: mode, type: string, description: {en: m}}
  - {key: level, type: integer, description: {en: l}}
  - {key: newer_key, type: bool, since: 4.0.0, description: {en: n}}
"#,
        ),
        (
            "filters/grep.yaml",
            r#"
kind: filter
name: grep
title: {en: Grep}
docs_url: https://docs.fluentbit.io/x
event_types: [logs]
as_processor: true
params:
  - {key: regex, type: regex, multi: repeat, description: {en: r}}
"#,
        ),
        (
            "filters/throttle.yaml",
            r#"
kind: filter
name: throttle
title: {en: Throttle}
docs_url: https://docs.fluentbit.io/x
event_types: [logs]
as_processor: false
params:
  - {key: rate, type: integer, description: {en: r}}
"#,
        ),
        (
            "filters/modify.yaml",
            r#"
kind: filter
name: modify
title: {en: Modify}
docs_url: https://docs.fluentbit.io/x
event_types: [logs]
as_processor: true
params:
  - {key: add, type: string, multi: repeat, description: {en: a}}
  - {key: secret_add, type: password, secret: true, description: {en: s}}
"#,
        ),
    ];

    fn toy() -> (Package, PluginCatalog) {
        let mut files = BTreeMap::new();
        files.insert(
            "streams/log/stream.yaml".to_string(),
            STREAM.as_bytes().to_vec(),
        );
        files.insert(
            "streams/log/index-template.json".to_string(),
            br#"{"template":{"mappings":{}}}"#.to_vec(),
        );
        let pkg = Package::from_files(MANIFEST, files).unwrap();
        let mut cat = PluginCatalog::default();
        for (f, y) in CATALOG {
            cat.insert(spec2::Plugin::parse(f, y).unwrap());
        }
        pkg.check_catalog(&cat).unwrap();
        (pkg, cat)
    }

    fn act(json: &str) -> Activation {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn secrets_omit_if_show_if_locked_and_global_filters() {
        let (pkg, cat) = toy();
        let os = OsAddress::parse("os.local").unwrap();
        let c = Context::new(&os);
        let a = act(
            r#"{"package_id":"toy","package_version":"2.1.0","spec_version":"2.0","streams":{"log":{"enabled":true,
            "input":{"paths":["/a","/b"],"token":{"$secret":"api-token"},"mode":"fancy","fancy_level":4},
            "processors":[{"id":"slow","enabled":true,"fields":{"rate":50}},{"plugin":"modify","params":{"add":["x 1"],"secret_add":{"$secret":"other"}}}]}}}"#,
        );
        let out = compile(
            &[Activated {
                package: &pkg,
                activation: &a,
            }],
            &c,
            &cat,
        )
        .unwrap();
        let input = &out.document["pipeline"]["inputs"][0];
        assert_eq!(
            input["db"],
            Value::from("${VELOX_STATE_DIR}/tail-velox.toy.log.db")
        );
        assert_eq!(input["path"], Value::from("/a,/b"));
        assert_eq!(input["token"], Value::from("${VELOX_SECRET_API_TOKEN}"));
        assert_eq!(input["mode"], Value::from("fancy"));
        assert_eq!(input["level"], Value::from(4));
        assert_eq!(
            input["newer_key"],
            Value::from(true),
            "no floor given: the field is kept"
        );
        assert_eq!(
            out.secrets.iter().cloned().collect::<Vec<_>>(),
            vec!["api-token", "other"]
        );
        // The locked default runs even though the activation left it out, the
        // throttle filter cannot run as a processor and goes global, modify
        // joins the chain, metadata closes it.
        let chain = input["processors"]["logs"].as_sequence().unwrap();
        let names: Vec<&str> = chain.iter().map(|p| p["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "modify",
                "grep",
                "content_modifier",
                "content_modifier",
                "content_modifier"
            ]
        );
        assert_eq!(chain[0]["secret_add"], Value::from("${VELOX_SECRET_OTHER}"));
        let filters = out.document["pipeline"]["filters"].as_sequence().unwrap();
        assert_eq!(filters[0]["name"], Value::from("throttle"));
        assert_eq!(filters[0]["match"], Value::from("velox.toy.log"));
        assert_eq!(filters[0]["rate"], Value::from(50));
        assert_eq!(
            out.document["pipeline"]["outputs"][0]["index"],
            Value::from("veloxagent-log.toy-v2")
        );
        assert_eq!(
            out.document["service"]["storage.path"],
            Value::from(DEFAULT_STORAGE_PATH)
        );
        assert!(!out.needs_otlp_credential);
        assert!(out.yaml.contains("write_operation: create"), "{}", out.yaml);

        // Defaults: mode=plain is omitted, level hidden by show_if, slow off.
        let a = act(
            r#"{"package_id":"toy","package_version":"2.1.0","spec_version":"2.0","streams":{"log":{"enabled":true,"input":{"paths":["/a"]}}}}"#,
        );
        let out = compile(
            &[Activated {
                package: &pkg,
                activation: &a,
            }],
            &c,
            &cat,
        )
        .unwrap();
        let input = &out.document["pipeline"]["inputs"][0];
        assert!(input.get("mode").is_none() && input.get("level").is_none());
        let names: Vec<&str> = input["processors"]["logs"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "grep",
                "content_modifier",
                "content_modifier",
                "content_modifier"
            ]
        );
        assert!(out.document["pipeline"].get("filters").is_none());

        // A default named by id alone runs as the package ships it: `slow`
        // is off in the package, so `{"id":"slow"}` keeps it off.
        let a = act(
            r#"{"package_id":"toy","package_version":"2.1.0","spec_version":"2.0","streams":{"log":{"enabled":true,"input":{"paths":["/a"]},"processors":[{"id":"slow"}]}}}"#,
        );
        let out = compile(
            &[Activated {
                package: &pkg,
                activation: &a,
            }],
            &c,
            &cat,
        )
        .unwrap();
        assert!(
            out.document["pipeline"].get("filters").is_none(),
            "{}",
            out.yaml
        );

        // An agent floor below `since` drops the field.
        let c_old = Context {
            fluent_bit_version: Some("3.2.4"),
            ..Context::new(&os)
        };
        let out = compile(
            &[Activated {
                package: &pkg,
                activation: &a,
            }],
            &c_old,
            &cat,
        )
        .unwrap();
        assert!(out.document["pipeline"]["inputs"][0]
            .get("newer_key")
            .is_none());
    }

    #[test]
    fn values_are_validated_in_the_compiler() {
        let (pkg, cat) = toy();
        let os = OsAddress::parse("os.local").unwrap();
        let c = Context::new(&os);
        let cases: &[(&str, &str)] = &[
            (r#"{"input":{}}"#, "required field has no value"),
            (r#"{"input":{"paths":["/a"],"nope":1}}"#, "unknown field"),
            (r#"{"input":{"paths":["/a"],"token":"plain"}}"#, "$secret"),
            (
                r#"{"input":{"paths":["/a"],"mode":"weird"}}"#,
                "not one of the options",
            ),
            (
                r#"{"input":{"paths":["/a"],"mode":"fancy","fancy_level":9}}"#,
                "above the maximum",
            ),
            (
                r#"{"input":{"paths":["/a"],"mode":"fancy","fancy_level":"x"}}"#,
                "expected an integer",
            ),
            (r#"{"input":{"paths":["/a\n"]}}"#, "control character"),
            (
                r#"{"input":{"paths":["/a"]},"processors":[{"plugin":"grep","params":{}}]}"#,
                "not allowed",
            ),
            (
                r#"{"input":{"paths":["/a"]},"processors":[{"plugin":"modify","params":{"zzz":1}}]}"#,
                "not a parameter",
            ),
            (
                r#"{"input":{"paths":["/a"]},"processors":[{"plugin":"modify","params":{"secret_add":"x"}}]}"#,
                "is a secret",
            ),
            (
                r#"{"input":{"paths":["/a"]},"processors":[{"id":"ghost"}]}"#,
                "unknown processor id",
            ),
            (
                r#"{"input":{"paths":["/a"]},"parser":"json"}"#,
                "not offered",
            ),
        ];
        for (stream_json, needle) in cases {
            let json = format!(
                r#"{{"package_id":"toy","package_version":"2.1.0","spec_version":"2.0","streams":{{"log":{{"enabled":true,{}}}}}}}"#,
                &stream_json[1..stream_json.len() - 1]
            );
            let a = act(&json);
            let err = compile(
                &[Activated {
                    package: &pkg,
                    activation: &a,
                }],
                &c,
                &cat,
            )
            .unwrap_err();
            assert!(err.0.contains(needle), "{stream_json}: {err}");
        }
        // Nothing enabled is an error, as is an activation for another package.
        let a = act(
            r#"{"package_id":"toy","package_version":"2.1.0","spec_version":"2.0","streams":{"log":{"enabled":false}}}"#,
        );
        assert_eq!(
            compile(
                &[Activated {
                    package: &pkg,
                    activation: &a
                }],
                &c,
                &cat
            )
            .unwrap_err()
            .0,
            "no stream enabled"
        );
        let a = act(
            r#"{"package_id":"toy","package_version":"9.9.9","spec_version":"2.0","streams":{}}"#,
        );
        assert!(compile(
            &[Activated {
                package: &pkg,
                activation: &a
            }],
            &c,
            &cat
        )
        .unwrap_err()
        .0
        .contains("activation names"));
        let a = act(
            r#"{"package_id":"toy","package_version":"2.1.0","spec_version":"2.0","streams":{"zzz":{"enabled":true}}}"#,
        );
        assert!(compile(
            &[Activated {
                package: &pkg,
                activation: &a
            }],
            &c,
            &cat
        )
        .unwrap_err()
        .0
        .contains("does not have"));
    }

    /// The YAML the agent receives is what Fluent Bit reads: env references
    /// stay bare, `on`/`off` stay strings, the port a number.
    #[test]
    fn the_yaml_is_fluent_bit_shaped() {
        let (pkg, cat) = toy();
        let os = OsAddress::parse("https://os.example.com/os").unwrap();
        let c = Context::new(&os);
        let a = act(
            r#"{"package_id":"toy","package_version":"2.1.0","spec_version":"2.0","streams":{"log":{"enabled":true,"input":{"paths":["/a"]}}}}"#,
        );
        let out = compile(
            &[Activated {
                package: &pkg,
                activation: &a,
            }],
            &c,
            &cat,
        )
        .unwrap();
        let y = &out.yaml;
        assert!(
            y.starts_with("service:\n  flush: 5\n  log_level: info\n"),
            "{y}"
        );
        assert!(y.contains("http_user: ${VELOX_OS_USER}"), "{y}");
        assert!(y.contains("port: 443\n"), "{y}");
        assert!(y.contains("path: /os\n"), "{y}");
        // Round trip: the text parses back to the same document.
        let back: Value = serde_yaml::from_str(y).unwrap();
        assert_eq!(back, out.document);
        // `on` must not come back as a YAML 1.1 boolean.
        assert_eq!(back["pipeline"]["outputs"][0]["tls"], Value::from("on"));
    }
}

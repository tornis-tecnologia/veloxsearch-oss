// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The policy model and its compiler. Pure, like the core's `upgrade` and
//! `snapshot` modules: this crate renders and decides, `velox-mesh` stores and
//! serves, `velox-agent` applies.
//!
//! ## The decoupling rule (veloxagent/docs/ARCHITECTURE.md §3)
//!
//! Integrations are **data** (a signed package: manifest + assets, ADR-039).
//! What turns a package plus variables into a collector fragment is the
//! shared `velox-integrations` crate (`crates/velox-integrations`, ADR-068),
//! the same renderer and closed token set the core's DaemonSet path uses.
//! The compiler never renders a package itself: it asks a [`FragmentRenderer`]
//! for each activation and only **assembles** the result into one
//! configuration per engine. [`PackageRenderer`] is the production renderer,
//! over a [`PackageStore`] of verified packages. Velox Mesh therefore owns
//! distribution and lifecycle, never the integrations' content, and an
//! integration written for the core's DaemonSet path works unchanged under
//! Velox Mesh.
//!
//! ## An activation has two halves
//!
//! Turning on an integration is not only a collector stanza. The Elastic
//! Fleet journey an operator already knows — *agent → policy → integration* —
//! means that picking "nginx" brings everything that makes nginx logs
//! **useful**, not just collected:
//!
//! | Half | What it is | Who performs it |
//! | --- | --- | --- |
//! | edge | the Fluent Bit / otelcol stanza that tails and ships | the agent, from [`Compiled::bodies`] |
//! | backend | ingest pipeline, index template, index pattern, dashboards | the core, on the target deployment |
//!
//! Both halves come out of the **same signed package** (ADR-039), so they
//! cannot drift: the manifest that names `agent.conf.tmpl` also names
//! `pipeline.json`, `index-template.json` and `saved-objects.ndjson`, and the
//! core's apply-engine already installs that side per deployment. This crate
//! therefore compiles a policy into *both*: the engine bodies, and
//! [`Compiled::backend`] — the inventory the target deployment must carry
//! before the policy's data means anything. The Mesh does not write to
//! OpenSearch; it states the requirement and the core fulfils it, the same
//! split the core keeps between its pure modules and `k8s.rs`.
//!
//! ## Shape of a policy
//!
//! A policy is a named, versioned set of *activations* (package id + version +
//! variables), one or more *outputs* (where data goes) and agent settings.
//! Revisions are immutable: editing a policy creates revision N+1, agents
//! report the revision they applied, and the UI shows "12 of 14 agents on
//! revision 7". That is the Fleet model, and the Alloy model too once you
//! strip the names.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use velox_integrations::Package;

/// Which child process a fragment is for. The agent supervises one of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// Logs, host metrics and the OTLP relay: Fluent Bit, the engine every
    /// existing integration targets (ARCHITECTURE.md §5.5).
    FluentBit,
    /// The OpenTelemetry Collector on the host, by exception only (an
    /// integration that ships an `otel.yaml.tmpl`).
    OtelCollector,
}

/// One integration turned on in a policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activation {
    pub package_id: String,
    pub package_version: String,
    /// Schema 1.1: variables the package manifest declares at the top level.
    /// Kept for revisions written before inputs existed; a renderer treats
    /// them as the values of the implicit `logs` input. Validated against the
    /// manifest by the renderer, not here.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// Schema 1.2 (ADR-073): the inputs turned on, each with its values. The
    /// same shape the cluster's own record carries, so the Data tab and a
    /// policy edit one thing. Empty = the package's default-enabled inputs
    /// with default values.
    #[serde(default)]
    pub inputs: BTreeMap<String, velox_integrations::InputValues>,
}

impl Activation {
    /// What this activation selects, in the shared crate's terms. A 1.1
    /// activation that set top-level `vars` and no inputs is the `logs` input
    /// with those values — the only input such a package has.
    pub fn selection(&self) -> velox_integrations::InputSelection {
        let mut sel = velox_integrations::InputSelection {
            inputs: self.inputs.clone(),
        };
        if sel.inputs.is_empty() && !self.vars.is_empty() {
            sel.inputs.insert(
                "logs".into(),
                velox_integrations::InputValues {
                    vars: self.vars.clone(),
                },
            );
        }
        sel
    }
}

/// Where a policy's data goes. Credentials are **not** in the policy: the
/// Mesh hands each agent its own scoped credential through the connection
/// settings channel, and the compiled config references it as `${VAR}`,
/// the same way the core keeps `HTTP_Passwd` out of ConfigMaps
/// (`src/agents.rs` PASSWORD_REF).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Output {
    /// Straight to a deployment's OpenSearch over its bulk API. The default
    /// and the right answer up to tens of thousands of events per second.
    OpenSearch { deployment: String },
    /// To a Velox Gateway over HTTP + TLS (`out_http`, gzip JSON, a
    /// per-agent token), which buffers into the queue tier and answers 2xx
    /// only after the queue confirmed the write. Behind it sits either Data
    /// Prepper (Kafka) or `velox-queue relay` (Valkey); which one is the
    /// gateway's configuration, never the policy's. ARCHITECTURE.md §5.2.
    Gateway { endpoint: String },
}

/// Knobs that are the agent's, not an integration's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettings {
    /// On-disk buffer ceiling for Fluent Bit (`storage.total_limit_size`).
    pub buffer_limit: String,
    /// Fluent Bit `Flush` seconds.
    pub flush_seconds: u32,
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self {
            buffer_limit: "1G".into(),
            flush_seconds: 5,
        }
    }
}

/// One immutable revision of a policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub activations: Vec<Activation>,
    pub outputs: Vec<Output>,
    #[serde(default)]
    pub agent: AgentSettings,
}

/// A rendered piece of collector configuration for one engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub engine: Engine,
    pub body: String,
}

/// The backend half of one activation: what the target deployment must carry
/// for this integration's data to be parsed, mapped, retained and shown.
///
/// It names the package rather than carrying rendered bodies, because the core
/// already installs exactly this (`integrations::apply_package`) and owns every
/// OpenSearch write. `resources` is the inventory for the UI — the four
/// families an operator sees when they pick an integration — derived from the
/// package itself, so it cannot claim an artifact the package does not ship.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendRequirement {
    pub deployment: String,
    pub package_id: String,
    pub package_version: String,
    /// The name this integration's artifacts share on the agent path —
    /// `veloxagent-<integration>-<signal>-v<major>` — which is also the index
    /// the compiled collector config writes to and the pattern the collector
    /// account is granted. Carried rather than re-derived so the config, the
    /// artifacts and the grant cannot be computed three different ways.
    pub artifact_name: String,
    /// `(kind, id)` for everything installing this package creates, under the
    /// ids this path uses.
    pub resources: Vec<(String, String)>,
    /// The input ids this activation turns on (ADR-073), in package order.
    #[serde(default)]
    pub inputs: Vec<String>,
}

/// The artifact families an operator chooses between when they read an
/// integration. A package always ships an agent config; the rest are optional
/// and the manifest says which it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ArtifactSummary {
    pub ingest_pipeline: bool,
    pub index_template: bool,
    pub index_patterns: usize,
    pub dashboards: usize,
}

impl ArtifactSummary {
    /// Count the families in a package's created-resource set.
    pub fn of(resources: &[(String, String)]) -> ArtifactSummary {
        let mut s = ArtifactSummary::default();
        for (kind, _) in resources {
            match kind.as_str() {
                "IngestPipeline" => s.ingest_pipeline = true,
                "IndexTemplate" => s.index_template = true,
                "IndexPattern" => s.index_patterns += 1,
                "SavedObject" => s.dashboards += 1,
                _ => {}
            }
        }
        s
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("activation {package_id}@{package_version}: {reason}")]
    Render {
        package_id: String,
        package_version: String,
        reason: String,
    },
    #[error("policy has no outputs")]
    NoOutput,
}

/// The boundary between "which integrations, for whom" (this crate) and
/// "what an integration renders to" (`velox-integrations`). [`PackageRenderer`]
/// is the real one; tests fake it.
pub trait FragmentRenderer {
    fn render(&self, activation: &Activation, outputs: &[Output]) -> Result<Vec<Fragment>, String>;

    /// The backend half of the same activation. The default is "none", so a
    /// renderer that only cares about the edge (the tests' fake) stays
    /// three lines long; [`PackageRenderer`] overrides it from the package.
    fn backend(
        &self,
        _activation: &Activation,
        _outputs: &[Output],
    ) -> Result<Option<BackendRequirement>, String> {
        Ok(None)
    }
}

/// Where verified packages come from: the Mesh's package cache, filled only
/// by packages `catalog::verify_package` accepted (signature verification is
/// the whole security boundary, ADR-039; the Mesh adds no second one).
pub trait PackageStore {
    fn get(&self, id: &str, version: &str) -> Option<&Package>;
}

/// What the agent's collector reaches: the OpenSearch endpoint a policy's
/// `Output::OpenSearch { deployment }` resolves to. The namespace keeps
/// `{tenant}` and the in-cluster `{os_host}` identical to the DaemonSet path;
/// `os_host` overrides the latter for hosts outside the cluster, which cannot
/// resolve `<deployment>.<ns>.svc` (ARCHITECTURE.md §5.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub deployment: String,
    pub namespace: String,
    pub os_host: Option<String>,
}

/// What a rendered config carries where the core's path carries the admin
/// credentials: the agent exports both from the scoped credential the Mesh
/// hands it over the connection-settings channel, and Fluent Bit expands
/// `${VAR}` at start (the core's `PASSWORD_REF` does the same for ConfigMaps).
pub const OS_USER_REF: &str = "${VELOX_OS_USER}";
pub const OS_PASSWORD_REF: &str = "${VELOX_OS_PASSWORD}";

/// The same two names without the `${...}`, which is what the agent exports
/// into the engine's environment. They are consts rather than a `strip` at
/// the call site because the two spellings have to agree: a rename on one
/// side and not the other produces a configuration whose credentials are
/// silently empty, and the symptom is a 401 from OpenSearch — an hour of
/// looking in the wrong place.
pub const OS_USER_ENV: &str = "VELOX_OS_USER";
pub const OS_PASSWORD_ENV: &str = "VELOX_OS_PASSWORD";

/// The production [`FragmentRenderer`]: `velox_integrations::render` over the
/// package's `agent.conf.tmpl`, with the closed variable set resolved from the
/// policy's OpenSearch output. The same function, the same eight tokens and the
/// same fail-closed rule as the core — the "works unchanged" guarantee is this
/// struct being thin.
pub struct PackageRenderer<'a, S: PackageStore + ?Sized> {
    pub store: &'a S,
    pub target: Target,
}

impl<S: PackageStore + ?Sized> PackageRenderer<'_, S> {
    fn package(&self, activation: &Activation) -> Result<&Package, String> {
        self.store
            .get(&activation.package_id, &activation.package_version)
            .ok_or_else(|| "package is not in the verified store".to_string())
    }

    /// The deployment a policy's data lands in. Today's templates bake
    /// `out_opensearch` in, so a gateway output needs the `out_http` rewrite
    /// the phase-3 contract defines (ARCHITECTURE.md §5.2); refuse rather
    /// than ship a config that ignores the policy's output.
    fn target<'o>(&self, outputs: &'o [Output]) -> Result<&'o str, String> {
        let dep = outputs
            .iter()
            .find_map(|o| match o {
                Output::OpenSearch { deployment } => Some(deployment.as_str()),
                Output::Gateway { .. } => None,
            })
            .ok_or_else(|| "no OpenSearch output; gateway outputs land in phase 3".to_string())?;
        if dep != self.target.deployment {
            return Err(format!(
                "output names deployment '{dep}' but the target is '{}'",
                self.target.deployment
            ));
        }
        Ok(dep)
    }
}

impl<S: PackageStore + ?Sized> FragmentRenderer for PackageRenderer<'_, S> {
    fn render(&self, activation: &Activation, outputs: &[Output]) -> Result<Vec<Fragment>, String> {
        let pkg = self.package(activation)?;
        self.target(outputs)?;
        // Every enabled input, for the HOST target (this is Velox Agent's
        // side), each writing to its own index: every artifact either
        // collection path creates is named
        // `veloxagent-<integration>-<signal>-v<major>` (ADR-071) — the prefix
        // is what the collector account is granted, and the version keeps a
        // mapping change from having to be made in place. Variables are
        // validated against the input's declaration, defaults filled, unknown
        // names refused — the same function the core's cluster path runs.
        let vars_for = |index: &str| {
            let mut vars = velox_integrations::Vars::new(
                &self.target.deployment,
                &self.target.namespace,
                index,
                &pkg.manifest.id,
                OS_USER_REF,
                OS_PASSWORD_REF,
            );
            if let Some(h) = &self.target.os_host {
                vars = vars.with_os_host(h);
            }
            vars
        };
        let rendered = velox_integrations::render_inputs(
            pkg,
            &activation.package_version,
            &activation.selection(),
            velox_integrations::TargetKind::Host,
            &vars_for,
        )
        .map_err(|e| e.to_string())?;
        Ok(rendered
            .into_iter()
            .map(|r| Fragment {
                engine: match r.engine {
                    velox_integrations::EngineKind::FluentBit => Engine::FluentBit,
                    velox_integrations::EngineKind::OtelCollector => Engine::OtelCollector,
                },
                body: r.body,
            })
            .collect())
    }

    /// The other half: everything this package creates in the target
    /// deployment's OpenSearch, read off the package itself. The Mesh writes
    /// none of it — it states the requirement and the core's apply-engine
    /// fulfils it, so "what the DaemonSet path installs" and "what a policy
    /// brings" are the same set by construction.
    fn backend(
        &self,
        activation: &Activation,
        outputs: &[Output],
    ) -> Result<Option<BackendRequirement>, String> {
        let pkg = self.package(activation)?;
        let deployment = self.target(outputs)?.to_string();
        // The renamed inventory, not the package's declared one: on this path
        // the artifacts carry the `veloxagent-` names, and an inventory that
        // named the package's own ids would describe objects that are not
        // there (and would claim objects the DaemonSet path owns).
        let resources =
            velox_integrations::agent_created_resources(pkg, &activation.package_version)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|(kind, id)| (kind.name().to_string(), id))
                .collect();
        let inputs = activation
            .selection()
            .enabled(pkg)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(i, _)| i.id)
            .collect();
        Ok(Some(BackendRequirement {
            deployment,
            package_id: pkg.manifest.id.clone(),
            package_version: activation.package_version.clone(),
            artifact_name: velox_integrations::agent_artifact_name(
                &pkg.manifest.id,
                &pkg.manifest.index,
                &activation.package_version,
            ),
            resources,
            inputs,
        }))
    }
}

/// The configuration an agent receives: one body per engine it must run,
/// plus a hash the agent echoes back so the Mesh knows what is applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compiled {
    pub policy_id: String,
    pub revision: u64,
    /// Engine → full configuration body. An engine absent here is one the
    /// agent must stop.
    pub bodies: BTreeMap<Engine, String>,
    /// What each activation needs on the deployment it ships to, in
    /// activation order. Empty only for a policy whose every output is a
    /// gateway (the queue tier owns the backend side there).
    #[serde(default)]
    pub backend: Vec<BackendRequirement>,
    /// sha256 over `revision` and every body, hex. Deterministic: the same
    /// revision compiles to the same hash on every Mesh replica.
    pub hash: String,
}

/// Compile one revision. Fragments are assembled in activation order, so a
/// policy's YAML is reproducible and diffable between revisions.
pub fn compile(policy: &Policy, renderer: &dyn FragmentRenderer) -> Result<Compiled, CompileError> {
    if policy.outputs.is_empty() {
        return Err(CompileError::NoOutput);
    }
    let mut bodies: BTreeMap<Engine, Vec<String>> = BTreeMap::new();
    let mut backend: Vec<BackendRequirement> = Vec::new();
    for act in &policy.activations {
        let fail = |reason: String| CompileError::Render {
            package_id: act.package_id.clone(),
            package_version: act.package_version.clone(),
            reason,
        };
        let frags = renderer.render(act, &policy.outputs).map_err(fail)?;
        for f in frags {
            bodies.entry(f.engine).or_default().push(f.body);
        }
        if let Some(req) = renderer.backend(act, &policy.outputs).map_err(fail)? {
            backend.push(req);
        }
    }
    // Fluent Bit gets ONE configuration: a single `[SERVICE]` section the
    // agent settings decide, then every fragment with its own stripped
    // (ADR-073) — two inputs, or two integrations, must not hand the engine
    // two SERVICE sections. The OTel body is YAML and concatenates as before.
    let bodies: BTreeMap<Engine, String> = bodies
        .into_iter()
        .map(|(e, parts)| match e {
            Engine::FluentBit => {
                let refs: Vec<&str> = parts.iter().map(String::as_str).collect();
                (
                    e,
                    velox_integrations::assemble_fluent_bit(
                        &velox_integrations::fluent_bit_service(policy.agent.flush_seconds, None),
                        &refs,
                    ),
                )
            }
            Engine::OtelCollector => (e, parts.join("\n")),
        })
        .collect();

    let mut h = Sha256::new();
    h.update(policy.revision.to_be_bytes());
    for (e, b) in &bodies {
        h.update(format!("{e:?}\n"));
        h.update(b.as_bytes());
        h.update(b"\n--\n");
    }
    let hash = h.finalize().iter().map(|b| format!("{b:02x}")).collect();

    Ok(Compiled {
        policy_id: policy.id.clone(),
        revision: policy.revision,
        bodies,
        backend,
        hash,
    })
}

#[cfg(test)]
mod tests {

    /// The reference a package renders and the variable the agent exports are
    /// the same name, or the credential never reaches the engine.
    #[test]
    fn the_reference_and_the_variable_cannot_drift() {
        assert_eq!(OS_USER_REF, format!("${{{OS_USER_ENV}}}"));
        assert_eq!(OS_PASSWORD_REF, format!("${{{OS_PASSWORD_ENV}}}"));
    }
    use super::*;

    /// Stands in for the open-source renderer: emits a Fluent Bit stanza per
    /// activation, and an OTel stanza when the package id says so.
    struct Fake;
    impl FragmentRenderer for Fake {
        fn render(&self, a: &Activation, outputs: &[Output]) -> Result<Vec<Fragment>, String> {
            if a.package_id == "broken" {
                return Err("manifest declares no agent asset".into());
            }
            let mut out = vec![Fragment {
                engine: Engine::FluentBit,
                body: format!(
                    "# {}@{} -> {} outputs",
                    a.package_id,
                    a.package_version,
                    outputs.len()
                ),
            }];
            if a.package_id.starts_with("otel-") {
                out.push(Fragment {
                    engine: Engine::OtelCollector,
                    body: format!("receivers: [{}]", a.package_id),
                });
            }
            Ok(out)
        }
    }

    fn policy(acts: &[&str]) -> Policy {
        Policy {
            id: "p1".into(),
            name: "web tier".into(),
            revision: 7,
            activations: acts
                .iter()
                .map(|id| Activation {
                    package_id: id.to_string(),
                    package_version: "1.0.0".into(),
                    vars: BTreeMap::new(),
                    inputs: BTreeMap::new(),
                })
                .collect(),
            outputs: vec![Output::OpenSearch {
                deployment: "logs-a1b2".into(),
            }],
            agent: AgentSettings::default(),
        }
    }

    #[test]
    fn fragments_are_grouped_per_engine_in_activation_order() {
        let c = compile(&policy(&["nginx", "otel-host", "postgres"]), &Fake).unwrap();
        let fb = &c.bodies[&Engine::FluentBit];
        assert!(fb.find("nginx").unwrap() < fb.find("otel-host").unwrap());
        assert!(fb.find("otel-host").unwrap() < fb.find("postgres").unwrap());
        assert_eq!(c.bodies[&Engine::OtelCollector], "receivers: [otel-host]");
        assert_eq!(c.revision, 7);
    }

    #[test]
    fn an_engine_with_no_fragment_is_absent_so_the_agent_stops_it() {
        let c = compile(&policy(&["nginx"]), &Fake).unwrap();
        assert!(!c.bodies.contains_key(&Engine::OtelCollector));
    }

    #[test]
    fn the_hash_is_deterministic_and_changes_with_the_revision() {
        let a = compile(&policy(&["nginx"]), &Fake).unwrap();
        let b = compile(&policy(&["nginx"]), &Fake).unwrap();
        assert_eq!(a.hash, b.hash);
        let mut p = policy(&["nginx"]);
        p.revision = 8;
        assert_ne!(compile(&p, &Fake).unwrap().hash, a.hash);
    }

    #[test]
    fn a_render_failure_names_the_activation() {
        let err = compile(&policy(&["nginx", "broken"]), &Fake).unwrap_err();
        assert_eq!(
            err,
            CompileError::Render {
                package_id: "broken".into(),
                package_version: "1.0.0".into(),
                reason: "manifest declares no agent asset".into(),
            }
        );
    }

    #[test]
    fn no_output_is_refused_before_rendering() {
        let mut p = policy(&["nginx"]);
        p.outputs.clear();
        assert_eq!(compile(&p, &Fake), Err(CompileError::NoOutput));
    }

    // ---- PackageRenderer: the real boundary, over an in-memory store ----

    use velox_integrations::{Assets, Manifest, Teardown};

    fn nginx_like(agent_config: &str) -> Package {
        Package {
            manifest: Manifest {
                id: "nginx".into(),
                index: "nginx-logs".into(),
                dashboard: "velox-nginx".into(),
                assets: Assets {
                    pipeline: None,
                    index_template: None,
                    saved_objects: "saved-objects.ndjson".into(),
                    agent_config: "agent.conf.tmpl".into(),
                    otel_config: None,
                },
                variables: vec![],
                inputs: vec![],
                teardown: Teardown {
                    ingest_pipeline: None,
                    index_template: None,
                    ingest_pipelines: vec![],
                    index_templates: vec![],
                    index_pattern: "nginx-logs".into(),
                    saved_objects: vec![],
                    index_patterns: vec![],
                    ism_policies: vec![],
                    datasources: vec![],
                },
            },
            pipeline: None,
            index_template: None,
            saved_objects: String::new(),
            agent_config: agent_config.into(),
            otel_config: None,
            extra: BTreeMap::new(),
        }
    }

    /// A schema-1.1 package: one variable with a default, one required, and
    /// an OTel asset.
    fn parameterised() -> Package {
        let mut pkg = nginx_like(
            "[INPUT]\n    Path ${path}\n    Port ${port}\n[OUTPUT]\n    Index {index}\n",
        );
        pkg.manifest.variables = vec![
            velox_integrations::Variable {
                name: "path".into(),
                kind: velox_integrations::VarType::String,
                default: Some("/var/log/nginx/access.log".into()),
                required: false,
                description: BTreeMap::new(),
                targets: vec![],
            },
            velox_integrations::Variable {
                name: "port".into(),
                kind: velox_integrations::VarType::Integer,
                default: None,
                required: true,
                description: BTreeMap::new(),
                targets: vec![],
            },
        ];
        pkg.otel_config =
            Some("receivers:\n  filelog:\n    include: [${path}]\n# {recipe_id}\n".into());
        pkg
    }

    struct Store(BTreeMap<(String, String), Package>);
    impl PackageStore for Store {
        fn get(&self, id: &str, version: &str) -> Option<&Package> {
            self.0.get(&(id.to_string(), version.to_string()))
        }
    }

    const TMPL: &str = "[OUTPUT]\n    Host {os_host}\n    HTTP_User {os_user}\n    HTTP_Passwd {os_password}\n    Index {index}\n    # {tenant} {recipe_id} {ns}\n";

    fn store(tmpl: &str) -> Store {
        let mut m = BTreeMap::new();
        m.insert(("nginx".to_string(), "1.0.0".to_string()), nginx_like(tmpl));
        Store(m)
    }

    fn target(os_host: Option<&str>) -> Target {
        Target {
            deployment: "logs-a1b2".into(),
            namespace: "veloxsearch".into(),
            os_host: os_host.map(str::to_string),
        }
    }

    /// The package renders with the same eight tokens the core uses, the
    /// credentials as `${VAR}` references, and the in-cluster host by default.
    #[test]
    fn package_renderer_uses_the_shared_renderer_and_credential_refs() {
        let s = store(TMPL);
        let r = PackageRenderer {
            store: &s,
            target: target(None),
        };
        let c = compile(&policy(&["nginx"]), &r).unwrap();
        let fb = &c.bodies[&Engine::FluentBit];
        assert!(fb.contains("Host logs-a1b2.veloxsearch.svc"), "{fb}");
        assert!(fb.contains("HTTP_User ${VELOX_OS_USER}"), "{fb}");
        assert!(fb.contains("HTTP_Passwd ${VELOX_OS_PASSWORD}"), "{fb}");
        // The agent path's own name, not the package's (ADR-071).
        assert!(fb.contains("Index veloxagent-nginx-logs-v1"), "{fb}");
        assert!(fb.contains("# velox-logs-a1b2 nginx veloxsearch"), "{fb}");
        assert!(!c.bodies.contains_key(&Engine::OtelCollector));
    }

    /// A host outside the cluster gets the published endpoint instead.
    #[test]
    fn package_renderer_honours_an_off_cluster_host() {
        let s = store(TMPL);
        let r = PackageRenderer {
            store: &s,
            target: target(Some("logs.example.internal:9200")),
        };
        let c = compile(&policy(&["nginx"]), &r).unwrap();
        assert!(c.bodies[&Engine::FluentBit].contains("Host logs.example.internal:9200"));
    }

    /// The fail-closed rule survives the boundary: a package with a foreign
    /// token is refused, and the error names the activation.
    #[test]
    fn package_renderer_fails_closed_on_a_foreign_token() {
        let s = store("Secret {cluster_admin_ssh}\n");
        let r = PackageRenderer {
            store: &s,
            target: target(None),
        };
        let err = compile(&policy(&["nginx"]), &r).unwrap_err();
        match err {
            CompileError::Render {
                package_id, reason, ..
            } => {
                assert_eq!(package_id, "nginx");
                assert!(reason.contains("cluster_admin_ssh"), "{reason}");
                assert!(reason.contains("closed variable set"), "{reason}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Only verified packages render; a version the store never saw is an
    /// error, never a fetch.
    #[test]
    fn package_renderer_refuses_an_unknown_package() {
        let s = store(TMPL);
        let r = PackageRenderer {
            store: &s,
            target: target(None),
        };
        let mut p = policy(&["nginx"]);
        p.activations[0].package_version = "9.9.9".into();
        let err = compile(&p, &r).unwrap_err();
        assert!(
            matches!(err, CompileError::Render { ref reason, .. } if reason.contains("verified store"))
        );
    }

    /// Honest refusals for what phase 0 does not do yet: policy variables
    /// (no manifest schema for them) and gateway outputs (phase 3).
    #[test]
    fn package_renderer_refuses_what_the_spec_does_not_define_yet() {
        let s = store(TMPL);
        let r = PackageRenderer {
            store: &s,
            target: target(None),
        };
        let mut p = policy(&["nginx"]);
        p.activations[0]
            .vars
            .insert("path".into(), "/var/log/x".into());
        assert!(
            matches!(compile(&p, &r), Err(CompileError::Render { ref reason, .. }) if reason.contains("not declared"))
        );

        let mut p = policy(&["nginx"]);
        p.outputs = vec![Output::Gateway {
            endpoint: "https://gw:8443".into(),
        }];
        assert!(
            matches!(compile(&p, &r), Err(CompileError::Render { ref reason, .. }) if reason.contains("phase 3"))
        );

        let mut p = policy(&["nginx"]);
        p.outputs = vec![Output::OpenSearch {
            deployment: "other".into(),
        }];
        assert!(
            matches!(compile(&p, &r), Err(CompileError::Render { ref reason, .. }) if reason.contains("'other'"))
        );
    }

    /// Schema 1.1 end to end: the policy value wins over the default, the
    /// default fills what the policy left out, a missing required variable
    /// is refused by name, and the OTel asset becomes the second fragment
    /// with the same substitutions.
    #[test]
    fn package_renderer_applies_declared_variables_and_renders_the_otel_asset() {
        let mut m = BTreeMap::new();
        m.insert(("nginx".to_string(), "1.0.0".to_string()), parameterised());
        let s = Store(m);
        let r = PackageRenderer {
            store: &s,
            target: target(None),
        };

        let mut p = policy(&["nginx"]);
        p.activations[0].vars.insert("port".into(), "8081".into());
        let c = compile(&p, &r).unwrap();
        let fb = &c.bodies[&Engine::FluentBit];
        assert!(fb.contains("Path /var/log/nginx/access.log"), "{fb}");
        assert!(fb.contains("Port 8081"), "{fb}");
        // The agent path's own name, not the package's (ADR-071).
        assert!(fb.contains("Index veloxagent-nginx-logs-v1"), "{fb}");
        let otel = &c.bodies[&Engine::OtelCollector];
        assert_eq!(
            otel,
            "receivers:\n  filelog:\n    include: [/var/log/nginx/access.log]\n# nginx\n"
        );

        let p = policy(&["nginx"]);
        assert!(
            matches!(compile(&p, &r), Err(CompileError::Render { ref reason, .. }) if reason == "variable `port`: required and no value or default")
        );
    }

    // ---- the backend half of an activation (the Fleet journey) ----

    /// A package with every artifact family, the way a real integration
    /// ships: a pipeline, a template, an index pattern and a dashboard.
    fn full_package() -> Package {
        let mut pkg = nginx_like("[OUTPUT]\n    Index {index}\n");
        pkg.pipeline = Some("{}".into());
        pkg.index_template = Some("{}".into());
        pkg.saved_objects = concat!(
            "{\"type\":\"index-pattern\",\"id\":\"nginx-logs\"}\n",
            "{\"type\":\"dashboard\",\"id\":\"velox-nginx-overview\"}\n",
            "{\"type\":\"visualization\",\"id\":\"velox-nginx-reqs\"}\n"
        )
        .into();
        pkg.manifest.teardown.ingest_pipeline = Some("nginx".into());
        pkg.manifest.teardown.index_template = Some("nginx-logs".into());
        pkg
    }

    fn full_store() -> Store {
        let mut m = BTreeMap::new();
        m.insert(("nginx".to_string(), "1.0.0".to_string()), full_package());
        Store(m)
    }

    /// Picking an integration brings BOTH halves: the agent stanza and the
    /// inventory the target deployment must carry. This is the property the
    /// Fleet-style journey rests on — an operator who turns on nginx gets
    /// parsing, mapping and a dashboard, not just a tail.
    #[test]
    fn an_activation_compiles_to_an_edge_half_and_a_backend_half() {
        let s = full_store();
        let r = PackageRenderer {
            store: &s,
            target: target(None),
        };
        let c = compile(&policy(&["nginx"]), &r).unwrap();

        // Edge half.
        assert!(c.bodies[&Engine::FluentBit].contains("Index veloxagent-nginx-logs-v1"));

        // Backend half: one requirement, naming the deployment the policy
        // ships to and the package that fulfils it.
        assert_eq!(c.backend.len(), 1);
        let b = &c.backend[0];
        assert_eq!(b.deployment, "logs-a1b2");
        assert_eq!(
            (b.package_id.as_str(), b.package_version.as_str()),
            ("nginx", "1.0.0")
        );

        // The inventory is read off the package, so it cannot claim an
        // artifact the package does not ship.
        let kinds: Vec<&str> = b.resources.iter().map(|(k, _)| k.as_str()).collect();
        assert!(kinds.contains(&"IngestPipeline"));
        assert!(kinds.contains(&"IndexTemplate"));
        assert!(kinds.contains(&"IndexPattern"));
        assert!(kinds.contains(&"SavedObject"));
        // ...and every id is this path's, not the package's. Both paths can
        // be active on one deployment, and installing under the package's own
        // saved-object ids would repoint the DaemonSet install's panels.
        assert_eq!(b.artifact_name, "veloxagent-nginx-logs-v1");
        assert!(b
            .resources
            .iter()
            .any(|(_, id)| id == "veloxagent-nginx-logs-v1-overview"));
        assert!(b
            .resources
            .iter()
            .all(|(_, id)| id.starts_with("veloxagent-nginx-logs-v1")));

        let sum = ArtifactSummary::of(&b.resources);
        assert_eq!(
            sum,
            ArtifactSummary {
                ingest_pipeline: true,
                index_template: true,
                index_patterns: 1,
                dashboards: 2, // the dashboard + its visualization
            }
        );
    }

    /// A package that ships no pipeline and no template says so, instead of
    /// the screen promising four families for every integration (the
    /// `kubernetes` package is exactly this: dynamic mapping, no pipeline).
    #[test]
    fn the_inventory_reflects_what_the_package_actually_ships() {
        let mut m = BTreeMap::new();
        let mut bare = nginx_like("[OUTPUT]\n    Index {index}\n");
        bare.saved_objects = "{\"type\":\"index-pattern\",\"id\":\"k8s-logs\"}\n".into();
        m.insert(("nginx".to_string(), "1.0.0".to_string()), bare);
        let s = Store(m);
        let r = PackageRenderer {
            store: &s,
            target: target(None),
        };
        let c = compile(&policy(&["nginx"]), &r).unwrap();
        assert_eq!(
            ArtifactSummary::of(&c.backend[0].resources),
            ArtifactSummary {
                ingest_pipeline: false,
                index_template: false,
                index_patterns: 1,
                dashboards: 0,
            }
        );
    }

    /// Two activations, two requirements, in activation order — so the UI can
    /// show "this policy brings N artifacts" without re-deriving anything.
    #[test]
    fn every_activation_contributes_its_own_requirement() {
        let mut m = BTreeMap::new();
        m.insert(("nginx".to_string(), "1.0.0".to_string()), full_package());
        let mut pg = full_package();
        pg.manifest.id = "postgres".into();
        pg.manifest.index = "postgres-logs".into();
        m.insert(("postgres".to_string(), "1.0.0".to_string()), pg);
        let s = Store(m);
        let r = PackageRenderer {
            store: &s,
            target: target(None),
        };
        let c = compile(&policy(&["nginx", "postgres"]), &r).unwrap();
        let ids: Vec<&str> = c.backend.iter().map(|b| b.package_id.as_str()).collect();
        assert_eq!(ids, vec!["nginx", "postgres"]);
    }

    /// The fake renderer answers "no backend half" by default, so a compile
    /// that only cares about the edge still works — and a policy with no
    /// usable output never reaches the backend question.
    #[test]
    fn a_renderer_without_a_backend_half_still_compiles() {
        let c = compile(&policy(&["nginx"]), &Fake).unwrap();
        assert!(c.backend.is_empty());
    }
}

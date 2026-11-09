// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The policy model and its compiler. Pure, like the core's `upgrade` and
//! `snapshot` modules: this crate renders and decides, `velox-mesh` stores and
//! serves, `velox-agent` applies.
//!
//! ## The decoupling rule (veloxagent/docs/ARCHITECTURE.md §3)
//!
//! Integrations are **data** (a signed package: manifest + assets, ADR-039).
//! What turns a package plus variables into a collector fragment lives in the
//! core today (`src/agents.rs::fluent_bit_conf_template`,
//! `src/integrations.rs` interpolation) and is to be extracted into a shared
//! crate (`velox-integrations`, PLAN.md fase 0) that both workspaces depend
//! on. This crate never renders a package itself: it asks a
//! [`FragmentRenderer`] for each activation and only **assembles** the result
//! into one configuration per engine. Velox Mesh therefore owns distribution
//! and lifecycle, never the integrations' content, and an integration written
//! for the core's DaemonSet path works unchanged under Velox Mesh.
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

/// Which child process a fragment is for. The agent supervises one of each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// Logs: Fluent Bit, the engine every existing integration targets.
    FluentBit,
    /// Metrics and traces: the OpenTelemetry Collector.
    OtelCollector,
}

/// One integration turned on in a policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activation {
    pub package_id: String,
    pub package_version: String,
    /// Variables the package manifest declares. Validated against the
    /// manifest by the renderer, not here.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
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

/// The open-source side of the boundary. Implemented over the core's
/// renderer; faked in tests.
pub trait FragmentRenderer {
    fn render(&self, activation: &Activation, outputs: &[Output]) -> Result<Vec<Fragment>, String>;
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
    for act in &policy.activations {
        let frags =
            renderer
                .render(act, &policy.outputs)
                .map_err(|reason| CompileError::Render {
                    package_id: act.package_id.clone(),
                    package_version: act.package_version.clone(),
                    reason,
                })?;
        for f in frags {
            bodies.entry(f.engine).or_default().push(f.body);
        }
    }
    let bodies: BTreeMap<Engine, String> = bodies
        .into_iter()
        .map(|(e, parts)| (e, parts.join("\n")))
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
        hash,
    })
}

#[cfg(test)]
mod tests {
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
}

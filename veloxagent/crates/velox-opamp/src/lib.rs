// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
//! The wire contract between Velox Agent and Velox Mesh.
//!
//! It follows **OpAMP** (Open Agent Management Protocol, opentelemetry.io/
//! docs/specs/opamp — status Beta): agent-initiated, the agent reports its
//! description, health, effective config and the status of the last remote
//! config; the server answers with a remote config (bodies + hash), connection
//! settings, a package offer or a command. Reasons for adopting it rather than
//! inventing a check-in protocol (veloxagent/docs/PLAN.md §2.4):
//!
//! * the OpenTelemetry Collector already ships an OpAMP supervisor, so the
//!   metrics/traces engine can be managed natively by the same Mesh;
//! * Grafana's Fleet Management and Bindplane speak it, so a customer with
//!   another OpAMP agent can point it at the Mesh;
//! * the message set already covers upgrades and certificate rotation, two
//!   things a home-grown protocol always adds late and badly.
//!
//! No Rust implementation of OpAMP exists upstream (only Go, .NET, Java,
//! Python), so this crate **is** ours. Transport today: plain HTTP, the agent
//! POSTs when it has something to say and otherwise every
//! [`DEFAULT_POLL_SECONDS`] (the spec's default). WebSocket can come later for
//! instant server-to-agent delivery; the messages do not change.
//!
//! Encoding: JSON over HTTP in this first cut, because every tool in the
//! shop can read it and the volume is negligible (one small message per agent
//! per poll). **Every type and field here is named exactly as the protobuf
//! schema names it** (`proto/opamp/v1/opamp.proto`, vendored with its
//! Apache-2.0 notice in `proto/NOTICE.md`), and the test
//! `json_field_names_exist_in_the_proto` reads that file to prove it. Two
//! deliberate JSON-side simplifications, both lossless on the way to
//! protobuf: attribute lists are maps (the proto's `repeated KeyValue`), and
//! bitmask fields (`capabilities`, `flags`) are lists of the enum names. A
//! switch to `prost` is therefore mechanical and the two encodings can
//! coexist behind a content-type (PLAN.md, marco "fim da fase 1").

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Spec default: how often an idle agent polls over plain HTTP.
pub const DEFAULT_POLL_SECONDS: u64 = 30;
/// Spec default path. Port 4320 is the spec's default too.
pub const DEFAULT_PATH: &str = "/v1/opamp";

/// What the agent says about itself. Alloy calls these *attributes* and
/// matches policies on them; so does the Mesh (label-based assignment,
/// ARCHITECTURE.md §4.2). Keys under `velox.` are reserved for the agent
/// (`velox.agent.version`, `velox.os`, `velox.arch`); the rest is the
/// operator's (`env=prod`, `team=payments`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDescription {
    pub identifying_attributes: BTreeMap<String, String>,
    pub non_identifying_attributes: BTreeMap<String, String>,
}

/// OpAMP `RemoteConfigStatuses`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemoteConfigStatuses {
    Unset,
    Applied,
    Applying,
    Failed,
}

/// OpAMP `RemoteConfigStatus`: the agent's verdict on the last config it was
/// handed. `last_remote_config_hash` is the Mesh's compiled hash echoed back —
/// that echo is how the UI knows "12 of 14 on revision 7" without trusting a
/// counter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfigStatus {
    pub last_remote_config_hash: String,
    pub status: RemoteConfigStatuses,
    /// Set on `Failed`: the engine's own words (Fluent Bit's dry-run output),
    /// shown verbatim in the agent's detail screen.
    #[serde(default)]
    pub error_message: String,
}

/// OpAMP `ComponentHealth`, one level deep: the agent plus each supervised
/// engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub healthy: bool,
    /// Unix nanoseconds, as the spec has it.
    pub start_time_unix_nano: u64,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub status: String,
    /// When `status` was observed; 0 when the agent did not say.
    #[serde(default)]
    pub status_time_unix_nano: u64,
    #[serde(default)]
    pub component_health_map: BTreeMap<String, ComponentHealth>,
}

/// OpAMP `AgentCapabilities`, spelled out instead of the proto's bitmask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AgentCapabilities {
    ReportsStatus,
    AcceptsRemoteConfig,
    ReportsEffectiveConfig,
    ReportsHealth,
    ReportsRemoteConfig,
    ReportsHeartbeat,
    AcceptsRestartCommand,
    AcceptsOpAMPConnectionSettings,
    AcceptsOtherConnectionSettings,
}

/// Agent → server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToServer {
    /// Stable per installation (persisted next to the agent, like Alloy's
    /// `alloy_seed.json`). UUID string.
    pub instance_uid: String,
    pub sequence_num: u64,
    #[serde(default)]
    pub agent_description: Option<AgentDescription>,
    #[serde(default)]
    pub remote_config_status: Option<RemoteConfigStatus>,
    #[serde(default)]
    pub health: Option<ComponentHealth>,
    #[serde(default)]
    pub capabilities: Vec<AgentCapabilities>,
}

/// OpAMP `AgentConfigObject`: one configuration body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfigObject {
    pub body: String,
    pub content_type: String,
}

/// OpAMP `AgentConfigMap`: bodies by engine name (`fluent-bit`, `otelcol`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfigMap {
    pub config_map: BTreeMap<String, AgentConfigObject>,
}

/// OpAMP `AgentRemoteConfig`: the compiled policy and its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRemoteConfig {
    pub config: AgentConfigMap,
    pub config_hash: String,
}

/// OpAMP `ServerToAgentFlags`, spelled out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ServerToAgentFlags {
    /// The server lost this agent's state (restart, failover) and wants
    /// everything again.
    ReportFullState,
    ReportAvailableComponents,
}

/// OpAMP `ServerErrorResponseType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerErrorResponseType {
    Unknown,
    BadRequest,
    Unavailable,
}

/// OpAMP `ServerErrorResponse`. Also how *parked* and *read-only* reach the
/// host: a seat shortfall is explained where the operator is looking, not
/// only in the UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerErrorResponse {
    #[serde(rename = "type")]
    pub kind: ServerErrorResponseType,
    #[serde(default)]
    pub error_message: String,
}

/// Server → agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToAgent {
    pub instance_uid: String,
    #[serde(default)]
    pub remote_config: Option<AgentRemoteConfig>,
    #[serde(default)]
    pub flags: Vec<ServerToAgentFlags>,
    #[serde(default)]
    pub error_response: Option<ServerErrorResponse>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const PROTO: &str = include_str!("../proto/opamp/v1/opamp.proto");

    #[test]
    fn a_report_round_trips_and_unknown_optional_fields_default() {
        let msg = AgentToServer {
            instance_uid: "0193a4b2-…".into(),
            sequence_num: 3,
            agent_description: None,
            remote_config_status: Some(RemoteConfigStatus {
                last_remote_config_hash: "abc".into(),
                status: RemoteConfigStatuses::Applied,
                error_message: String::new(),
            }),
            health: None,
            capabilities: vec![AgentCapabilities::ReportsStatus],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"status\":\"APPLIED\""));
        let back: AgentToServer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);

        let minimal: AgentToServer =
            serde_json::from_str(r#"{"instance_uid":"x","sequence_num":1}"#).unwrap();
        assert!(minimal.remote_config_status.is_none());
    }

    // ---- conformance with the vendored proto -------------------------------

    /// The field names declared by `message <name> { ... }` in the proto,
    /// including those inside a `oneof`, excluding nested messages/enums. A
    /// deliberately small reader: proto3 field lines are `<type> <name> = N;`.
    fn proto_fields(message: &str) -> BTreeSet<String> {
        let start = PROTO
            .find(&format!("message {message} {{"))
            .unwrap_or_else(|| panic!("message {message} is not in opamp.proto"));
        let mut depth = 0i32;
        let mut oneof_depth: Option<i32> = None;
        let mut fields = BTreeSet::new();
        for raw in PROTO[start..].lines() {
            let line = raw.split("//").next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let opens = line.matches('{').count() as i32;
            let closes = line.matches('}').count() as i32;
            if depth == 1 && line.starts_with("oneof ") {
                oneof_depth = Some(depth + opens);
            }
            let at_field_depth = depth == 1 || oneof_depth == Some(depth);
            if at_field_depth && opens == 0 && line.ends_with(';') && line.contains('=') {
                let lhs = line.split('=').next().unwrap().trim();
                if let Some(name) = lhs.rsplit(char::is_whitespace).next() {
                    fields.insert(name.to_string());
                }
            }
            depth += opens - closes;
            if oneof_depth.is_some_and(|d| depth < d) {
                oneof_depth = None;
            }
            if depth <= 0 {
                break;
            }
        }
        fields
    }

    /// The variant names of `enum <name>` in the proto, with the
    /// `<EnumName>_` prefix the spec uses stripped.
    fn proto_variants(enum_name: &str) -> BTreeSet<String> {
        let start = PROTO
            .find(&format!("enum {enum_name} {{"))
            .unwrap_or_else(|| panic!("enum {enum_name} is not in opamp.proto"));
        let prefix = format!("{enum_name}_");
        PROTO[start..]
            .lines()
            .skip(1)
            .take_while(|l| !l.trim_start().starts_with('}'))
            .filter_map(|l| {
                let line = l.split("//").next().unwrap_or("").trim();
                let lhs = line.split('=').next()?.trim();
                (!lhs.is_empty() && line.contains('='))
                    .then(|| lhs.strip_prefix(&prefix).unwrap_or(lhs).to_string())
            })
            .collect()
    }

    /// JSON object keys of a serialised value — our side of the comparison.
    fn json_keys<T: Serialize>(v: &T) -> BTreeSet<String> {
        serde_json::to_value(v)
            .unwrap()
            .as_object()
            .expect("a struct")
            .keys()
            .cloned()
            .collect()
    }

    fn assert_subset(ours: BTreeSet<String>, theirs: BTreeSet<String>, message: &str) {
        let foreign: Vec<_> = ours.difference(&theirs).collect();
        assert!(
            foreign.is_empty(),
            "{message}: fields {foreign:?} are not in the proto (has {theirs:?})"
        );
    }

    /// Every field our JSON types name exists on the proto message of the same
    /// name — the guard against inventing a protocol while calling it OpAMP.
    #[test]
    fn json_field_names_exist_in_the_proto() {
        // The reader itself, on a message with a oneof and a map.
        let serr = proto_fields("ServerErrorResponse");
        assert!(serr.contains("retry_info"), "oneof fields count: {serr:?}");
        assert!(proto_fields("ComponentHealth").contains("component_health_map"));

        let health = ComponentHealth {
            healthy: true,
            start_time_unix_nano: 1,
            last_error: String::new(),
            status: "ok".into(),
            status_time_unix_nano: 2,
            component_health_map: BTreeMap::new(),
        };
        let a2s = AgentToServer {
            instance_uid: "x".into(),
            sequence_num: 1,
            agent_description: Some(AgentDescription::default()),
            remote_config_status: Some(RemoteConfigStatus {
                last_remote_config_hash: "h".into(),
                status: RemoteConfigStatuses::Applied,
                error_message: String::new(),
            }),
            health: Some(health.clone()),
            capabilities: vec![],
        };
        let cfg = AgentRemoteConfig {
            config: AgentConfigMap {
                config_map: BTreeMap::from([(
                    "fluent-bit".to_string(),
                    AgentConfigObject {
                        body: String::new(),
                        content_type: "text/plain".into(),
                    },
                )]),
            },
            config_hash: "h".into(),
        };
        let err = ServerErrorResponse {
            kind: ServerErrorResponseType::Unavailable,
            error_message: "parked".into(),
        };
        let s2a = ServerToAgent {
            instance_uid: "x".into(),
            remote_config: Some(cfg.clone()),
            flags: vec![ServerToAgentFlags::ReportFullState],
            error_response: Some(err.clone()),
        };

        assert_subset(
            json_keys(&a2s),
            proto_fields("AgentToServer"),
            "AgentToServer",
        );
        assert_subset(
            json_keys(&s2a),
            proto_fields("ServerToAgent"),
            "ServerToAgent",
        );
        assert_subset(
            json_keys(&AgentDescription::default()),
            proto_fields("AgentDescription"),
            "AgentDescription",
        );
        assert_subset(
            json_keys(a2s.remote_config_status.as_ref().unwrap()),
            proto_fields("RemoteConfigStatus"),
            "RemoteConfigStatus",
        );
        assert_subset(
            json_keys(&health),
            proto_fields("ComponentHealth"),
            "ComponentHealth",
        );
        assert_subset(
            json_keys(&cfg),
            proto_fields("AgentRemoteConfig"),
            "AgentRemoteConfig",
        );
        assert_subset(
            json_keys(&cfg.config),
            proto_fields("AgentConfigMap"),
            "AgentConfigMap",
        );
        assert_subset(
            json_keys(&cfg.config.config_map["fluent-bit"]),
            proto_fields("AgentConfigObject"),
            "AgentConfigObject",
        );
        assert_subset(
            json_keys(&err),
            proto_fields("ServerErrorResponse"),
            "ServerErrorResponse",
        );
    }

    /// Enum variants likewise: ours serialise to the proto's names (minus the
    /// `<Enum>_` prefix the spec repeats on every variant).
    #[test]
    fn enum_variants_exist_in_the_proto() {
        let name = |v: &dyn erased::Ser| v.name();
        let statuses = [
            RemoteConfigStatuses::Unset,
            RemoteConfigStatuses::Applied,
            RemoteConfigStatuses::Applying,
            RemoteConfigStatuses::Failed,
        ];
        let theirs = proto_variants("RemoteConfigStatuses");
        for s in statuses {
            assert!(theirs.contains(&name(&s)), "{s:?} not in {theirs:?}");
        }
        let theirs = proto_variants("AgentCapabilities");
        for c in [
            AgentCapabilities::ReportsStatus,
            AgentCapabilities::AcceptsRemoteConfig,
            AgentCapabilities::ReportsEffectiveConfig,
            AgentCapabilities::ReportsHealth,
            AgentCapabilities::ReportsRemoteConfig,
            AgentCapabilities::ReportsHeartbeat,
            AgentCapabilities::AcceptsRestartCommand,
            AgentCapabilities::AcceptsOpAMPConnectionSettings,
            AgentCapabilities::AcceptsOtherConnectionSettings,
        ] {
            assert!(theirs.contains(&name(&c)), "{c:?} not in {theirs:?}");
        }
        let theirs = proto_variants("ServerToAgentFlags");
        for f in [
            ServerToAgentFlags::ReportFullState,
            ServerToAgentFlags::ReportAvailableComponents,
        ] {
            assert!(theirs.contains(&name(&f)), "{f:?} not in {theirs:?}");
        }
        let theirs = proto_variants("ServerErrorResponseType");
        for t in [
            ServerErrorResponseType::Unknown,
            ServerErrorResponseType::BadRequest,
            ServerErrorResponseType::Unavailable,
        ] {
            assert!(theirs.contains(&name(&t)), "{t:?} not in {theirs:?}");
        }
    }

    /// The serialised name of a unit variant, for the enum checks above.
    mod erased {
        pub trait Ser {
            fn name(&self) -> String;
        }
        impl<T: serde::Serialize> Ser for T {
            fn name(&self) -> String {
                serde_json::to_value(self)
                    .unwrap()
                    .as_str()
                    .expect("unit variant")
                    .to_string()
            }
        }
    }
}

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
//! per poll). The field names mirror the protobuf schema so a switch to prost
//! later is mechanical and the two can coexist behind a content-type.

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

/// OpAMP `RemoteConfigStatus.Status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RemoteConfigStatus {
    Unset,
    Applied,
    Applying,
    Failed,
}

/// The agent's verdict on the last config it was handed. `last_remote_config_hash`
/// is the Mesh's compiled hash echoed back — that echo is how the UI knows
/// "12 of 14 on revision 7" without trusting a counter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfigReport {
    pub last_remote_config_hash: String,
    pub status: RemoteConfigStatus,
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
    #[serde(default)]
    pub component_health_map: BTreeMap<String, ComponentHealth>,
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
    pub remote_config_status: Option<RemoteConfigReport>,
    #[serde(default)]
    pub health: Option<ComponentHealth>,
    /// Bitmask of `AgentCapabilities`; spelled out for now.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// One engine's configuration body, by engine name (`fluent-bit`, `otelcol`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRemoteConfig {
    pub config_map: BTreeMap<String, ConfigFile>,
    pub config_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFile {
    pub body: String,
    pub content_type: String,
}

/// Server → agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerToAgent {
    pub instance_uid: String,
    #[serde(default)]
    pub remote_config: Option<AgentRemoteConfig>,
    /// `ServerToAgentFlags.ReportFullState`: the server lost this agent's
    /// state (restart, failover) and wants everything again.
    #[serde(default)]
    pub report_full_state: bool,
    /// Error the agent should log and show; also carries *parked* and
    /// *read-only* so a seat shortfall is explained on the host, not only in
    /// the UI.
    #[serde(default)]
    pub error_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_round_trips_and_unknown_optional_fields_default() {
        let msg = AgentToServer {
            instance_uid: "0193a4b2-…".into(),
            sequence_num: 3,
            agent_description: None,
            remote_config_status: Some(RemoteConfigReport {
                last_remote_config_hash: "abc".into(),
                status: RemoteConfigStatus::Applied,
                error_message: String::new(),
            }),
            health: None,
            capabilities: vec![],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"status\":\"APPLIED\""));
        let back: AgentToServer = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);

        let minimal: AgentToServer =
            serde_json::from_str(r#"{"instance_uid":"x","sequence_num":1}"#).unwrap();
        assert!(minimal.remote_config_status.is_none());
    }
}

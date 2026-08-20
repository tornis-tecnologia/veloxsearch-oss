// Copyright (C) 2026 Tornis Desenvolvimento
// SPDX-License-Identifier: AGPL-3.0-only
pub mod api;

#[cfg(feature = "ssr")]
pub mod k8s;

#[cfg(feature = "ssr")]
pub mod auth;

// #80: per-request ownership enforcement — the Scope extractor, the Deployment
// capability token every deployment path takes instead of a raw name, and the
// tenant→namespace resolution behind them (ADR-044 layer 1).
#[cfg(feature = "ssr")]
pub mod scope;

#[cfg(feature = "ssr")]
pub mod discovery;

/// Telemetry a cluster ALREADY emits (#65) — the discovery half of the
/// sidecar marriage. Extends `discovery`, never replaces it.
#[cfg(feature = "ssr")]
pub mod telemetry;

#[cfg(feature = "ssr")]
pub mod recipes;

#[cfg(feature = "ssr")]
pub mod agents;

#[cfg(feature = "ssr")]
pub mod integrations;

// ADR-039 runtime catalog client (#75): fetch + verify + install packages from
// the external registry, with a compiled-in bootstrap floor for egress-less
// installs (ADR-047).
#[cfg(feature = "ssr")]
pub mod catalog;

// ADR-053: the OTel observability stack — the second collection option,
// additive to the Fluent Bit recipes above.
#[cfg(feature = "ssr")]
pub mod otel_stack;

// #74 golden gate: registry packages (external veloxsearch-registry repo,
// via VELOX_REGISTRY_PATH — #105) are frozen byte-equivalents of the
// in-binary recipe catalog. Test-only — nothing ships from it.
#[cfg(all(test, feature = "ssr"))]
mod registry_golden;

#[cfg(feature = "ssr")]
pub mod bootstrap;

#[cfg(feature = "ssr")]
pub mod access;

#[cfg(feature = "ssr")]
pub mod profiles;

#[cfg(feature = "ssr")]
pub mod metrics;

#[cfg(feature = "ssr")]
pub mod capacity;

#[cfg(feature = "ssr")]
pub mod db;

// #79 (ADR-041): control-plane accounts on top of the datastore — self-serve
// signup, email verification, password reset, and the tenant a session
// carries. Flag-gated by VELOX_MULTITENANT_AUTH (default OFF).
#[cfg(feature = "ssr")]
pub mod tenants;

// ADR-038's corporate-email denylist, ported server-side so signup and the
// landing page's demo gate agree on who is a qualified user.
#[cfg(feature = "ssr")]
pub mod email_denylist;

// Transactional mail for the account flows above; logs the link when no SMTP
// relay is configured.
#[cfg(feature = "ssr")]
pub mod mail;

// ADR-045 (#56): pure generators for the per-deployment auth provider. No
// cluster calls — MR2 wires the artifacts onto the CR.
#[cfg(feature = "ssr")]
pub mod auth_provider;

// Pre-save reachability probe for the configured provider (ADR-045 MR2).
#[cfg(feature = "ssr")]
pub mod auth_probe;

// ADR-048 (#109): version catalog, upgrade pre-flight and the pure reduction of
// the CR's upgrade reporting. `k8s.rs` does the writes.
#[cfg(feature = "ssr")]
pub mod upgrade;

// ADR-050: what a deployment is doing right now and whether it has settled —
// the predicate that replaces `health == "green"` everywhere.
#[cfg(feature = "ssr")]
pub mod activity;

// ADR-049: pure renderers and rules for the snapshot repository + the scheduled
// snapshot policy. `k8s.rs` owns the writes.
#[cfg(feature = "ssr")]
pub mod snapshot;

// ADR-052: the work a create/save defers until the cluster settles, the record
// of what has actually been applied (kept on the CR, never in memory), and the
// bounded retry schedule. `k8s.rs` owns the writes and runs the applier.
#[cfg(feature = "ssr")]
pub mod provisioning;

// ADR-048 rev. 2: the hourly upstream version check that turns into the
// "Upgrade v3.8.0" tag on a deployment. Suggestion only — the pre-flight and
// the catalog rules are unchanged.
#[cfg(feature = "ssr")]
pub mod version_feed;

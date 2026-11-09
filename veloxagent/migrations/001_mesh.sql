-- Copyright (C) 2026 Tornis Desenvolvimento
-- SPDX-License-Identifier: AGPL-3.0-only
--
-- Velox Mesh schema (ADR-068, veloxagent/docs/ARCHITECTURE.md §4.1).
--
-- Postgres is the Mesh's only state: replicas are interchangeable behind a
-- Service, like the Fleet Server. Applied by the hand-rolled runner in
-- `store.rs`, which mirrors the core's `db.rs` — plain SQL, append-only, one
-- transaction per file, a ledger of versions rather than content.
--
-- Times are **unix seconds** (bigint), not timestamptz, because every
-- comparison in this product is arithmetic on unix seconds already
-- (`velox-license` expiry, `last_seen` staleness) and one representation
-- beats two plus a conversion at every boundary.
--
-- `tenant_id` is on every table from the first migration even though
-- multitenancy lands later: adding it afterwards means rewriting every query
-- and every index, and the column costs nothing while there is one tenant.

-- A policy: the named thing an operator edits. The current revision is a
-- pointer, so reading "what should this agent run" is one indexed lookup.
CREATE TABLE IF NOT EXISTS mesh_policies (
    id               text PRIMARY KEY,
    tenant_id        text NOT NULL DEFAULT 'default',
    name             text NOT NULL,
    current_revision bigint NOT NULL DEFAULT 1,
    created_at       bigint NOT NULL,
    updated_at       bigint NOT NULL
);

-- Revisions are IMMUTABLE: editing a policy inserts N+1, never updates N.
-- That is what makes "12 of 14 agents on revision 7" answerable, and what
-- makes a rollback a pointer move instead of a restore.
--
-- `body` is the policy document (activations, outputs, agent settings) as
-- `velox_policy::Policy` serialises it. `compiled_hash` is the hash the
-- agent echoes back; it is stored so the Mesh never has to recompile to
-- answer "is this agent current?".
CREATE TABLE IF NOT EXISTS mesh_policy_revisions (
    policy_id     text   NOT NULL REFERENCES mesh_policies(id) ON DELETE CASCADE,
    revision      bigint NOT NULL,
    tenant_id     text   NOT NULL DEFAULT 'default',
    body          jsonb  NOT NULL,
    -- What the agent actually receives: engine name -> configuration body,
    -- compiled BY THE CORE (it owns the registry client and the signing
    -- keyring, so it is the only side that holds a verified package). The
    -- Mesh stores and serves it and never needs the registry.
    compiled      jsonb  NOT NULL,
    compiled_hash text   NOT NULL,
    created_at    bigint NOT NULL,
    PRIMARY KEY (policy_id, revision)
);

-- An enrollment token points at a policy, is reusable and revocable. The
-- SECRET IS NEVER STORED: only its sha256, the same way the core keeps
-- session and reset tokens. Expiry blocks new enrollments and does not touch
-- agents already enrolled (Fleet does the same and is right).
CREATE TABLE IF NOT EXISTS mesh_enrollment_tokens (
    id          text PRIMARY KEY,
    tenant_id   text NOT NULL DEFAULT 'default',
    token_hash  text NOT NULL UNIQUE,
    policy_id   text NOT NULL REFERENCES mesh_policies(id) ON DELETE CASCADE,
    created_at  bigint NOT NULL,
    expires_at  bigint,
    revoked     boolean NOT NULL DEFAULT false
);

-- One enrolled agent.
--
-- `credential_hash` is the per-agent bearer the Mesh issues AT enrollment and
-- the agent sends on every check-in. It is separate from the enrollment token
-- on purpose: Fleet separates the communication key from the output key, and
-- here a control credential is not a data credential. Revoking one agent must
-- not revoke the fleet.
--
-- `applied_hash` is what the agent SAYS it is running — echoed back from the
-- config it applied. Trusting that echo rather than a counter the Mesh
-- increments is the difference between a rollout view that is true and one
-- that is optimistic.
--
-- `status` is NOT stored: up/down and healthy/failed are derived from
-- `last_seen`, `applied_hash` and `health` at read time (`fleet.rs`). A
-- stored status is a status that goes stale the moment a writer dies.
CREATE TABLE IF NOT EXISTS mesh_agents (
    instance_uid    text PRIMARY KEY,
    tenant_id       text NOT NULL DEFAULT 'default',
    policy_id       text REFERENCES mesh_policies(id) ON DELETE SET NULL,
    credential_hash text NOT NULL,
    hostname        text NOT NULL DEFAULT '',
    attributes      jsonb NOT NULL DEFAULT '{}'::jsonb,
    enrolled_at     bigint NOT NULL,
    last_seen       bigint NOT NULL,
    applied_revision bigint,
    applied_hash    text,
    config_status   text NOT NULL DEFAULT 'UNSET',
    config_error    text NOT NULL DEFAULT '',
    health          jsonb,
    unenrolled      boolean NOT NULL DEFAULT false
);

-- The fleet list sorts by last check-in and filters by policy; both are the
-- screen's default reads.
CREATE INDEX IF NOT EXISTS mesh_agents_last_seen ON mesh_agents (tenant_id, last_seen DESC);
CREATE INDEX IF NOT EXISTS mesh_agents_policy ON mesh_agents (policy_id);

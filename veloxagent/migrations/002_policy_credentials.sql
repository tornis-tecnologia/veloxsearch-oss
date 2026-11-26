-- Copyright (C) 2026 Tornis Desenvolvimento
-- SPDX-License-Identifier: AGPL-3.0-only
--
-- The output credential an agent's collector authenticates to OpenSearch with
-- (ADR-068). Written by the CORE when a policy is saved — it is the side that
-- holds the deployment's admin login and creates the account
-- (`src/os_agent_user.rs`) — and served by the Mesh to an authenticated agent
-- on that policy, never to anyone else.
--
-- ## Why it is here and not in the policy
--
-- Policy revisions are immutable and kept forever, and everyone who can read
-- the fleet can read them. A credential inside a revision would therefore be
-- a credential in a permanent, widely-read audit trail. The compiled
-- configuration carries only `${VELOX_OS_USER}` / `${VELOX_OS_PASSWORD}`, the
-- same references the core's ConfigMap path uses, and the values travel on
-- this separate channel.
--
-- ## Why the password is stored, and not its hash
--
-- Every other secret in this schema is kept as sha256, because the Mesh only
-- ever needs to *verify* it. This one it must *hand over*: the agent cannot
-- authenticate to OpenSearch with a hash. Elastic Fleet has the same property
-- for its output API keys and keeps them the same way. What limits the damage
-- is the account itself — it can append to the policy's indices and do
-- nothing else, not even read them back (see `role_body` in the core).
--
-- One row per policy. Two policies on the same deployment share one
-- OpenSearch account, so they carry the same value; keying by policy is what
-- lets the Mesh answer "what does THIS agent need" with one indexed lookup
-- and without parsing a policy body.
CREATE TABLE IF NOT EXISTS mesh_policy_credentials (
    policy_id  text PRIMARY KEY REFERENCES mesh_policies(id) ON DELETE CASCADE,
    tenant_id  text NOT NULL DEFAULT 'default',
    username   text NOT NULL,
    password   text NOT NULL,
    updated_at bigint NOT NULL
);

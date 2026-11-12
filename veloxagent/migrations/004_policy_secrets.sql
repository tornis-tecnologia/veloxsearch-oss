-- Copyright (C) 2026 Tornis Desenvolvimento
-- SPDX-License-Identifier: AGPL-3.0-only
--
-- Integration secrets of a policy (ADR-074 decision 5): the values behind the
-- `${VELOX_SECRET_<ID>}` references a Spec 2.0 configuration compiles a
-- `password` field to. Same reasoning as `mesh_policy_credentials` (002):
-- revisions are immutable and widely read, so the revision stores only
-- `{"$secret": "<id>"}` and the value lives here, written by the core when a
-- policy is saved and served only to an agent authenticated on that policy.
--
-- Stored, not hashed, for the same reason as the collector password: the
-- agent must hand the value to the engine. Keyed (policy, secret id) so a
-- save can replace one secret and keep the others the operator did not
-- retype.
CREATE TABLE IF NOT EXISTS mesh_policy_secrets (
    policy_id  text NOT NULL REFERENCES mesh_policies(id) ON DELETE CASCADE,
    secret_id  text NOT NULL,
    value      text NOT NULL,
    updated_at bigint NOT NULL,
    PRIMARY KEY (policy_id, secret_id)
);

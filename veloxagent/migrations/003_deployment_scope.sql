-- Copyright (C) 2026 Tornis Desenvolvimento
-- SPDX-License-Identifier: AGPL-3.0-only
--
-- The Mesh is a deployment's, not the installation's (ADR-072).
--
-- Until here every list the Mesh answered — policies, agents, tokens — was
-- the installation's, and the deployment a policy wrote into appeared only
-- inside `mesh_policy_revisions.body->'outputs'`. One controller keeps
-- serving every deployment (the service is infrastructure, like the Fleet
-- Server), but the DATA is partitioned: every policy and every agent carries
-- the `(deployment_ns, deployment)` pair the core's `Deployment` capability
-- is built from, and every console query filters on it.
--
-- ## Why the agent carries the pair itself
--
-- An agent could derive its deployment through `policy_id`, but that column
-- goes NULL when the policy is deleted (ON DELETE SET NULL), and an agent
-- whose policy was deleted still belongs to the deployment it enrolled under
-- — it must keep showing in that deployment's fleet so the operator can
-- re-assign or unenrol it. The pair is copied from the policy AT ENROLLMENT
-- and never changes: moving a host to another deployment is a new enrollment
-- (ADR-072 §3), because a credential issued for one data path must never
-- start writing into another.
--
-- ## Back-fill
--
-- The deployment NAME is recoverable from the current revision's body. The
-- NAMESPACE is not stored anywhere in this schema, so it stays '' here and
-- the core — the side that knows which namespace a deployment lives in —
-- adopts such rows the first time it asks about a deployment of that name
-- (`store::adopt_legacy`). Rows that never get adopted are hidden, not
-- deleted. The Mesh has not been released; this reaches development
-- databases only.

ALTER TABLE mesh_policies
    ADD COLUMN IF NOT EXISTS deployment_ns text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS deployment    text NOT NULL DEFAULT '';

ALTER TABLE mesh_agents
    ADD COLUMN IF NOT EXISTS deployment_ns text NOT NULL DEFAULT '',
    ADD COLUMN IF NOT EXISTS deployment    text NOT NULL DEFAULT '';

UPDATE mesh_policies p
   SET deployment = COALESCE(
         (SELECT r.body->'outputs'->0->>'deployment'
            FROM mesh_policy_revisions r
           WHERE r.policy_id = p.id AND r.revision = p.current_revision),
         '')
 WHERE p.deployment = '';

UPDATE mesh_agents a
   SET deployment = p.deployment
  FROM mesh_policies p
 WHERE a.policy_id = p.id AND a.deployment = '';

-- The deployment's screen reads its policies by name and its fleet by last
-- check-in; both are now per pair.
CREATE INDEX IF NOT EXISTS mesh_policies_deployment
    ON mesh_policies (deployment_ns, deployment, name);
DROP INDEX IF EXISTS mesh_agents_last_seen;
CREATE INDEX IF NOT EXISTS mesh_agents_deployment_last_seen
    ON mesh_agents (deployment_ns, deployment, last_seen DESC);

# ADR-070 — A host agent's collector writes with a scoped OpenSearch account, handed to it by the Mesh

**Status:** proposed
**Date:** 2026-11-09

## Context

A policy compiles to a Fluent Bit configuration whose output stanza carries
`HTTP_User ${VELOX_OS_USER}` / `HTTP_Passwd ${VELOX_OS_PASSWORD}` — references,
never values, exactly as the in-cluster ConfigMap path does
(`agents::PASSWORD_REF`). Until now nothing filled them for a host agent. The
first real end-to-end run on a host made that concrete: Fluent Bit 5.2.0
started, tailed `/var/log/auth.log`, and every flush came back

```
[warn] [env] variable ${VELOX_OS_USER} is used but not set
[error] [output:opensearch] HTTP status=401 URI=/_bulk, response: Unauthorized
```

The in-cluster path answers this by giving collectors the deployment's **admin**
login through a Secret mounted into the pod. That is defensible on a node of
the cluster; it is not defensible on someone's VM. An agent holds its
credential as plainly as the host holds anything, and an admin login there
means one compromised web server can delete every index in the deployment.

Three further constraints shaped the answer:

- **OpenSearch has no API keys.** Elasticsearch does (`POST
  /_security/api_key`); the OpenSearch security plugin never adopted the
  feature. The equivalent is an internal user mapped to a role, so the
  credential is a username and a password, and every property that matters has
  to come from the role.
- **The destination is the deployment the policy names**, resolved at
  enrollment through the policy the enrollment token points at. Nothing about
  the credential is the host's to choose.
- **Policy revisions are immutable and kept forever**, and anyone who can read
  the fleet can read them. A credential inside a revision would be a password
  in a permanent, widely-read record.

## Decision

1. **One least-privilege account per deployment, created by the core.**
   `src/os_agent_user.rs` writes a role (`veloxagent-role`) and an internal
   user (`veloxagent-user`) through the security API with the deployment's
   admin login — the core is the only side that holds it. The role grants
   `indices:data/write/bulk` at the cluster level (OpenSearch checks the bulk
   action as a cluster permission *and* per index; the index-level grant alone
   fails every flush with a 403 that reads like a bad password) plus `read`,
   `write` and `create_index` on the agent path's own artifact names
   (ADR-071). Nothing wider: no `indices_all`, no wildcard, no cluster
   permission beyond bulk — and a name that does not carry the
   `veloxagent-` prefix is refused rather than granted, so the account cannot
   be talked into reaching the DaemonSet path's data by a caller that passes
   the wrong list.

2. **The account is shared by the agents of a deployment, not per host.**
   Per-agent accounts are the stronger model and are not ruled out, but they
   mean a security-plugin write per host and a rotation path that does not
   exist yet. What limits the damage today is the role, not the number of
   accounts.

3. **The password lives in a Secret beside the deployment, read-or-create.**
   Re-saving a policy must not rotate it under agents that have not picked the
   new one up — a rotation nobody applied is an outage with a 401 for a
   symptom. Deleting the Secret is therefore the rotation gesture.

4. **The Mesh is the courier, over the check-in it already authenticates.**
   One row per policy (`mesh_policy_credentials`), written by the core after
   the policy exists and served only to an agent that authenticated on that
   policy. This is the one secret in the Mesh's schema kept in the clear,
   because the Mesh must *hand it over* and an agent cannot authenticate to
   OpenSearch with a hash; Elastic Fleet keeps its output API keys the same
   way, and a test in `store.rs` exists to make anyone relaxing that rule say
   so out loud.

5. **It is a declared extension, not OpAMP.** The field on the check-in
   response is `velox_output_credential`. OpAMP's `connection_settings` offers
   an agent the settings for its own link back to the server, and its
   `OtherConnectionSettings` carries an endpoint, headers and a certificate —
   not a username and a password for a data-path output. Putting ours there
   would have dressed an invention up as protocol, so `VELOX_EXTENSIONS` in
   `velox-opamp` names it and the proto-subset guard allows exactly that.

6. **The agent keeps it in memory and puts it in the engine's environment.**
   Never on disk: writing it beside the configuration would put a password on
   the disk of every collecting host, which is what the `${...}` references
   exist to avoid. Fluent Bit expands `${VAR}` from its own environment at
   start, so a rotation means restarting the engine — the agent compares a
   fingerprint, never the password, and restarts only when the value changed.
   The variable names come from `velox_policy::OS_USER_ENV` /
   `OS_PASSWORD_ENV`, the same crate that renders the references, with a test
   asserting the two spellings cannot drift.

7. **Absent credential ≠ stop collecting.** An agent offered no credential
   applies its configuration anyway and lets the engine report what it cannot
   do. A host that stops collecting is worse than a host whose output is
   rejected, and the rejection is visible on the fleet screen.

## Consequences

- A host agent can be given a credential that can read and append to its own
  indices and do nothing else. Verified on a live deployment: documents from a real
  `/var/log/auth.log` land in `ssh-logs`, and deleting the Secret and saving
  again rotates the password, updates the account, reaches the agent, restarts
  the engine and keeps collecting with no failed flush.
- The in-cluster DaemonSet path still uses the admin login. Moving it onto this
  account is the obvious follow-up and is deliberately not in this ADR: it
  changes a path that works today, and it should change on its own evidence.
- Per-agent accounts, and revoking one host's write access without touching
  the fleet, remain open.
- The Mesh's database now holds a password. It is scoped to appending into
  known indices, and it is the Mesh's own operator who already controls that
  database — but it is a reason to keep the Mesh's Postgres as private as the
  core's.

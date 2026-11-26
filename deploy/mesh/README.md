# Velox Mesh — deployment assets

Velox Mesh is the fleet controller for Velox Agents (ADR-068, ADR-072). It is
its own binary (`velox-mesh`, from the `veloxagent/` workspace), its own image
and its own release; one Mesh serves every deployment of an installation and
runs in its own namespace, `velox-mesh` (ADR-072 amendment 5).

| File | What it is |
| --- | --- |
| `Dockerfile` | Runtime-only image, same base and hardening as `deploy/Dockerfile` (trixie-slim, `apt upgrade`, uid 10001). Expects a prebuilt `velox-mesh` and `LICENSE` in the context. |
| `build-image.sh` | Builds `velox-mesh` from `veloxagent/` and the image `docker.io/tornistecnologia/veloxsearch-mesh:<veloxagent version>`. `--profile debug\|release`, `--tag`, `--target` (e.g. a static musl build), `--skip-build`, `--push`. Nothing is pushed without `--push`. |
| `mesh.yaml` | Namespace `velox-mesh` (Pod Security `restricted`), ServiceAccount (no API token), Deployment `velox-mesh`, ClusterIP Service `velox-mesh:4320`. Compiled into the core: the console's install button applies exactly this file. |

The Mesh's contract with its environment (`veloxagent/crates/velox-mesh/src/main.rs`):

| Variable | Here |
| --- | --- |
| `VELOX_MESH_ADDR` | `0.0.0.0:4320` (the binary's default, `127.0.0.1:4320`, is unreachable in a pod) |
| `VELOX_MESH_PG_URL` | Secret `velox-mesh`, key `pg-url`. Required: the binary exits without it, and runs its embedded migrations before serving. |
| `VELOX_MESH_SERVICE_TOKEN` | Secret `velox-mesh`, key `service-token`. The core reads the same key to authenticate. |

There is no licence and no agent limit: the Mesh manages every host enrolled.

## Install from the console (the default)

*Settings → Velox Mesh → Install in this cluster.* The core, with rights
granted in `velox-mesh` only (`deploy/install.yaml`):

1. applies the namespace from `mesh.yaml`;
2. keeps Secret `velox-mesh` if it exists (its database password and service
   token are what a running Mesh uses) and generates any missing key;
3. creates — or resets the password of — the `velox_mesh` role and creates
   the `velox_mesh` database if missing, on the installation's Postgres. The
   one superuser connection the core makes (`db::ensure_mesh_database`); it
   never drops anything;
4. server-side applies the ServiceAccount, Deployment and Service;
5. finds the Mesh (`mesh::discover`) and, in public-URL mode, gives every
   deployment its route `https://<deployment>-mesh.<domain>` (ADR-072
   amendment 4).

Pressing it again reapplies: an upgrade to the image the core names. That is
the pinned tag in `mesh.yaml`, or `VELOX_MESH_IMAGE` on the core (a mirror, an
air-gapped registry, a development build).

## Install by hand (GitOps)

Prerequisites: `deploy/install.yaml` applied, first-run bootstrap complete,
`veloxsearch-postgres-0` Ready, and the image published (or mirrored) at the
tag `mesh.yaml` pins:

```bash
deploy/mesh/build-image.sh --push     # prints the digest; pin mesh.yaml to it
```

1. **Database and Secret**, in one shell so the password reaches both:

   ```bash
   MESH_PG_PW="$(openssl rand -hex 32)"
   kubectl -n veloxsearch-system exec -i veloxsearch-postgres-0 -- \
     psql -v ON_ERROR_STOP=1 -v pw="$MESH_PG_PW" -U postgres -d postgres <<'EOSQL'
   CREATE ROLE velox_mesh LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD :'pw';
   CREATE DATABASE velox_mesh OWNER velox_mesh;
   REVOKE ALL ON DATABASE velox_mesh FROM PUBLIC;
   EOSQL

   kubectl -n velox-mesh create secret generic velox-mesh \
     --from-literal=pg-url="postgres://velox_mesh:${MESH_PG_PW}@veloxsearch-postgres.veloxsearch-system.svc:5432/velox_mesh?sslmode=disable" \
     --from-literal=service-token="$(openssl rand -hex 32)"
   unset MESH_PG_PW
   ```

   `sslmode=disable` is required: the Mesh connects with `NoTls`
   (`store.rs`), and the hop never leaves the cluster.

2. **Apply**

   ```bash
   kubectl apply -f deploy/mesh/mesh.yaml
   kubectl -n velox-mesh rollout status deploy/velox-mesh
   kubectl -n velox-mesh logs deploy/velox-mesh
   # velox-mesh: listening on http://0.0.0.0:4320
   ```

3. **Nothing to wire.** The core finds Service and Secret `velox-mesh` in
   `velox-mesh` at start and whenever a status read finds no Mesh.
   `VELOX_MESH_URL` + `VELOX_MESH_SERVICE_TOKEN` on the core still override
   discovery, for a Mesh that lives elsewhere.

## Agents outside the cluster

In public-URL mode (*Settings → Access*) each deployment gets
`https://<deployment>-mesh.<domain>`: an Ingress in `velox-mesh` routing `/v1`
(enrol + OpAMP) and `/healthz` only, TLS only. It is the deployment's
*Public URL* Mesh endpoint, beside the matching OpenSearch one. On an
`sslip.io` domain with no TLS Secret the ingress serves its self-signed
default certificate: both endpoints are marked insecure, the compiler writes
`tls.verify Off`, and the enrol command carries `--insecure`. Set a TLS Secret
(a wildcard for the domain) in *Settings → Access* to verify instead.

On the host, the agent's installer enrols as part of the install. It sets up
Fluent Bit from upstream, `/opt/velox-agent`, the `velox-agent` user and the
systemd unit (ADR-069 amendment 1; `veloxagent/README.md`):

```bash
curl -fsSLO https://github.com/tornis-tecnologia/veloxsearch-oss/releases/download/velox-agent-v0.2.0/install.sh
sudo bash install.sh --mesh https://<deployment>-mesh.<ip>.sslip.io --token <token> --insecure
sudo velox-agent doctor
```

## Rollback

```bash
# remove the controller (state stays in Postgres); the console shows the
# install guide again on its next status read
kubectl -n velox-mesh delete deploy/velox-mesh svc/velox-mesh sa/velox-mesh

# roll the controller back one image instead of removing it
kubectl -n velox-mesh rollout undo deploy/velox-mesh
```

Migrations are append-only and there are no down-migrations: an image
rollback runs against a schema that may be newer than it knows. Take a dump
before upgrading the Mesh:

```bash
kubectl -n veloxsearch-system exec veloxsearch-postgres-0 -- \
  pg_dump -U postgres -Fc velox_mesh > velox_mesh.dump
```

Dropping the data is a separate, deliberate step (it forgets every agent,
policy and token):

```bash
kubectl -n velox-mesh delete secret velox-mesh
kubectl -n veloxsearch-system exec -i veloxsearch-postgres-0 -- \
  psql -v ON_ERROR_STOP=1 -U postgres -d postgres <<'EOSQL'
DROP DATABASE velox_mesh;
DROP ROLE velox_mesh;
EOSQL
```

## What is not covered

- **Publishing the image.** `veloxsearch-mesh` is not in `release.yml`: no CI
  build, no cosign signature, no digest pin. `mesh.yaml` pins `:0.1.0` by tag
  until then, and the install button pulls that tag unless the core has
  `VELOX_MESH_IMAGE`.
- **Postgres reconnection.** The Mesh opens one connection at start and does
  not reconnect; `/healthz` does not touch the database. Restart it after any
  Postgres restart: `kubectl -n velox-mesh rollout restart deploy/velox-mesh`.
- **NetworkPolicy.** None is shipped. The console face is protected by the
  service token alone.
- **Backups.** The Mesh's database rides the control-plane Postgres; nothing
  here schedules a dump of `velox_mesh`.
- **The gateway and queue** (ARCHITECTURE.md §5.2), **multi-arch
  images** (the build is the host's architecture).

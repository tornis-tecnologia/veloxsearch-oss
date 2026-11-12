# Velox Mesh — deployment assets

Velox Mesh is the fleet controller for Velox Agents (ADR-068, ADR-072). It is
its own binary (`velox-mesh`, from the `veloxagent/` workspace), its own image
and its own release; one Mesh serves every deployment of an installation and
runs in `veloxsearch-system`, beside the core.

| File | What it is |
| --- | --- |
| `Dockerfile` | Runtime-only image, same base and hardening as `deploy/Dockerfile` (trixie-slim, `apt upgrade`, uid 10001). Expects a prebuilt `velox-mesh` and `LICENSE` in the context. |
| `build-image.sh` | Builds `velox-mesh` from `veloxagent/` and the image `docker.io/tornistecnologia/veloxsearch-mesh:<veloxagent version>`. `--profile debug\|release`, `--tag`, `--target` (e.g. a static musl build), `--skip-build`, `--push`. Nothing is pushed without `--push`. |
| `mesh.yaml` | ServiceAccount (no API token), Deployment `velox-mesh`, ClusterIP Service `velox-mesh:4320`, and a commented-out Ingress for the agent face. The header carries every command below, verbatim. |

The Mesh's contract with its environment (`veloxagent/crates/velox-mesh/src/main.rs`):

| Variable | Here |
| --- | --- |
| `VELOX_MESH_ADDR` | `0.0.0.0:4320` (the binary's default, `127.0.0.1:4320`, is unreachable in a pod) |
| `VELOX_MESH_PG_URL` | Secret `velox-mesh`, key `pg-url`. Required: the binary exits without it, and runs its embedded migrations before serving. |
| `VELOX_MESH_SERVICE_TOKEN` | Secret `velox-mesh`, key `service-token`. Unset would disable the console face; the manifest makes it required. |

There is no licence variable: the binary computes its entitlement with no
licence document (`entitlement(None, …)`), so every install runs the free
tier (10 agents) until `velox-mesh license load|activate` exists.

## Rollout order

Prerequisites: `deploy/install.yaml` applied, first-run bootstrap complete,
and `veloxsearch-postgres-0` Ready. The image must be published (or
side-loaded) at the tag `mesh.yaml` pins:

```bash
deploy/mesh/build-image.sh --push     # prints the digest; pin mesh.yaml to it
```

1. **Database and Secret** — one shell, so the generated password reaches
   both. A separate role and database on the existing Postgres, never the
   core's `velox` database: both migration runners name their ledger
   `schema_migrations`, and neither role should read the other's tables.

   ```bash
   MESH_PG_PW="$(openssl rand -hex 32)"
   kubectl -n veloxsearch-system exec -i veloxsearch-postgres-0 -- \
     psql -v ON_ERROR_STOP=1 -v pw="$MESH_PG_PW" -U postgres -d postgres <<'EOSQL'
   CREATE ROLE velox_mesh LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD :'pw';
   CREATE DATABASE velox_mesh OWNER velox_mesh;
   REVOKE ALL ON DATABASE velox_mesh FROM PUBLIC;
   EOSQL

   kubectl -n veloxsearch-system create secret generic velox-mesh \
     --from-literal=pg-url="postgres://velox_mesh:${MESH_PG_PW}@veloxsearch-postgres.veloxsearch-system.svc:5432/velox_mesh?sslmode=disable" \
     --from-literal=service-token="$(openssl rand -hex 32)"
   unset MESH_PG_PW
   ```

   `sslmode=disable` is required, not a shortcut: the Mesh connects with
   `NoTls` (`store.rs`), and the hop never leaves the cluster.

2. **Apply**

   ```bash
   kubectl apply -f deploy/mesh/mesh.yaml
   kubectl -n veloxsearch-system rollout status deploy/velox-mesh
   kubectl -n veloxsearch-system logs deploy/velox-mesh
   # velox-mesh 0.1.0 — tier free, 10 seats
   # velox-mesh: listening on http://0.0.0.0:4320
   ```

3. **Wire the core.** The URL goes in `veloxsearch-env` (non-secret, ADR-034;
   not in `upgrade.yaml`, so upgrades keep it). The token is a `secretKeyRef`
   from the same Secret, so the two ends cannot drift. `install.yaml` is not
   edited.

   ```bash
   kubectl -n veloxsearch-system patch configmap veloxsearch-env --type merge \
     -p '{"data":{"VELOX_MESH_URL":"http://velox-mesh.veloxsearch-system.svc:4320"}}'

   kubectl -n veloxsearch-system patch deploy/veloxsearch --patch-file /dev/stdin <<'PATCH'
   spec:
     template:
       spec:
         containers:
           - name: veloxsearch
             env:
               - name: VELOX_MESH_SERVICE_TOKEN
                 valueFrom:
                   secretKeyRef:
                     name: velox-mesh
                     key: service-token
   PATCH
   ```

   The Deployment patch rolls the core, which also picks up the ConfigMap.
   Check it in the console: a deployment's *Mesh* tab → *Settings* shows the
   Mesh as reachable (`GET /healthz`, `src/mesh.rs`) instead of the install
   guide.

4. **Expose the agent face** (only if agents live outside the cluster, which
   is the point of the Mesh). Uncomment the Ingress at the bottom of
   `mesh.yaml`, replace `INGRESS-IP`, and re-apply. It routes only `/v1`
   (`/v1/enroll`, `/v1/opamp`); `/api/mesh/*` and `/healthz` match nothing.
   The certificate **must be publicly trusted**: `velox-agent` trusts only the
   WebPKI roots, so Traefik's default self-signed certificate or an internal
   CA makes every enrolment fail. Use a Let's Encrypt `ClusterIssuer` on
   cert-manager (bootstrap installs cert-manager but creates no issuer) or a
   certificate you already hold as a `kubernetes.io/tls` Secret.

   ```bash
   curl -sS -o /dev/null -w '%{http_code}\n' https://mesh.<ip>.sslip.io/v1/opamp -X POST   # 4xx from the Mesh, not a TLS error
   curl -sS -o /dev/null -w '%{http_code}\n' https://mesh.<ip>.sslip.io/api/mesh/overview -X POST   # 404 from Traefik
   ```

5. **Enrol an agent.** In the console, on the deployment's *Mesh* tab:
   *Settings* → add a Mesh endpoint `https://mesh.<ip>.sslip.io` (no `:4320`;
   the ingress listens on 443) and an OpenSearch endpoint; *Policies* → *New
   policy* bound to both; *Add agent* → mint a token → run the generated
   install guide on the host, ending in

   ```bash
   velox-agent enroll --mesh https://mesh.<ip>.sslip.io --token <token>
   velox-agent doctor
   ```

   The host appears on the *Overview* (the fleet) on its first check-in.

## Rollback

Each step undoes independently, newest first:

```bash
# un-publish the agent face (enrolled agents start failing check-ins; they keep
# their last applied config and Fluent Bit keeps shipping)
kubectl -n veloxsearch-system delete ingress velox-mesh-agents

# unwire the core → the console shows the install guide again
kubectl -n veloxsearch-system set env deploy/veloxsearch VELOX_MESH_SERVICE_TOKEN-
kubectl -n veloxsearch-system patch configmap veloxsearch-env --type json \
  -p '[{"op":"remove","path":"/data/VELOX_MESH_URL"}]'
kubectl -n veloxsearch-system rollout restart deploy/veloxsearch

# remove the controller (state stays in Postgres)
kubectl delete -f deploy/mesh/mesh.yaml

# roll the controller back one image instead of removing it
kubectl -n veloxsearch-system rollout undo deploy/velox-mesh
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
kubectl -n veloxsearch-system delete secret velox-mesh
kubectl -n veloxsearch-system exec -i veloxsearch-postgres-0 -- \
  psql -v ON_ERROR_STOP=1 -U postgres -d postgres <<'EOSQL'
DROP DATABASE velox_mesh;
DROP ROLE velox_mesh;
EOSQL
```

## What is not covered

- **Publishing the image.** `veloxsearch-mesh` is not in `release.yml`: no CI
  build, no cosign signature, no digest-pinned published `mesh.yaml`, and the
  console's install guide points at `https://get.veloxsearch.ai/mesh.yaml`,
  which this repository does not produce. `mesh.yaml` pins `:0.1.0` by tag
  until then.
- **Postgres reconnection.** The Mesh opens one connection at start and does
  not reconnect: if `veloxsearch-postgres` restarts, the Mesh keeps serving
  `/healthz` (which does not touch the database) while every real request
  fails, and the probes do not notice. Restart it after any Postgres restart:
  `kubectl -n veloxsearch-system rollout restart deploy/velox-mesh`.
- **NetworkPolicy.** None is shipped. The console face is protected by the
  service token alone; any pod in the cluster can reach port 4320. Tenant
  namespaces (ADR-044) would need an egress rule to the Mesh for in-cluster
  agents; external agents come in through the Ingress.
- **Other ingress controllers, NodePort, LoadBalancer.** Only a Traefik
  Ingress is sketched. VMs on the node network can reach a NodePort instead,
  but that exposes the console face too and is not provided here.
- **Backups.** The Mesh's database rides the control-plane Postgres; nothing
  here schedules a dump of `velox_mesh`.
- **Licences, the gateway and queue** (ARCHITECTURE.md §5.2), **multi-arch
  images** (the build is the host's architecture), and **upgrading** beyond
  re-tagging the image.

<!--
Copyright (C) 2026 Tornis Desenvolvimento
SPDX-License-Identifier: AGPL-3.0-only
-->
# Satellite test services

Two throwaway services a VeloxSearch deployment can be pointed at, so the two
integration surfaces that need something on the other end can be exercised on
any cluster:

| Fixture | Exercises | Code under test |
| --- | --- | --- |
| `openldap.yaml` | The auth-provider axis (ADR-045) | `src/auth_provider.rs`, `src/auth_probe.rs`, `views_auth_provider.jsx` |
| `minio.yaml` | The snapshot repository (ADR-049/042) | `src/snapshot.rs`, `k8s::{plan,set,verify}_snapshot_*`, `views_snapshot.jsx` |

Plain manifests, no Helm, no kustomize — `kubectl apply -f` on k8s, k3s, k0s,
minikube or kubeadm alike. Nothing in them is distribution-specific: no
IngressClass, no LoadBalancer, no node selector, and the one PVC takes the
cluster default StorageClass.

**These are fixtures, not deployments.** Credentials are in the clear in the
manifests by design. Do not apply them to a cluster that matters.

## Install

```sh
kubectl apply -f tests/satellites/openldap.yaml
kubectl apply -f tests/satellites/minio.yaml

kubectl -n velox-test-ldap rollout status deployment/openldap --timeout=120s
kubectl -n minio          rollout status deployment/minio     --timeout=180s
```

Uninstall is the inverse, and takes the PVC with it:

```sh
kubectl delete -f tests/satellites/minio.yaml -f tests/satellites/openldap.yaml
```

## OpenLDAP — what to paste into the auth-provider form

The fixture seeds this tree under `dc=velox,dc=test`:

```
ou=people                          ou=groups
  uid=alice   password alice123      cn=velox-admins    member: alice
  uid=bob     password bob123        cn=velox-readers   member: bob, carol
  uid=carol   password carol123
```

Deployment → **Authentication** tab → kind **LDAP**:

| Field | Value |
| --- | --- |
| Hosts | `openldap.velox-test-ldap.svc.cluster.local:389` |
| Bind DN | `cn=admin,dc=velox,dc=test` |
| Bind password | `velox-test-admin` |
| User base | `ou=people,dc=velox,dc=test` |
| User search | `(uid={0})` |
| Username attribute | `uid` |
| Role base | `ou=groups,dc=velox,dc=test` |
| Role search | `(member={0})` |
| User role attribute | *leave empty* |
| Role name | `cn` |
| Resolve nested roles | off |
| Enable SSL / StartTLS | off / off |

Leaving **User role attribute** empty is load-bearing, not laziness:
`auth_provider::ldap_backend` pins `userroleattribute: null` when it is blank,
which stops the security plugin falling back to `memberOf` and double-counting
groups. The fixture uses plain `groupOfNames` entries with no `memberof`
overlay, so attribute-based lookup would find nothing anyway.

Suggested role mappings, which give you one account per privilege level:

| Directory group | VeloxSearch role |
| --- | --- |
| `velox-admins` | `all_access` |
| `velox-readers` | `readall`, `kibana_user` |

Then log in to Dashboards as `alice` / `alice123`. The break-glass admin keeps
working throughout — `auth_provider` emits `basic_internal_auth_domain` for
every kind, so a broken directory can never lock you out of the cluster.

### What "Test connection" checks

The pre-save probe (`auth_probe::probe_ldap`) walks TCP reachability, the bind
as Bind DN, a sample user search through User search under User base, and the
group resolution. All five operations were verified against this exact seed:

```
bind cn=admin                                          ok
(uid=alice) under ou=people                            1 entry
(member=uid=alice,...) under ou=groups                 cn=velox-admins
(member=uid=bob,...)   under ou=groups                 cn=velox-readers
simple bind uid=alice / alice123                       ok
simple bind uid=alice / wrong                          Invalid credentials (49)
```

### One restart on first boot is normal

The pod exits 1 a few seconds into its very first start and comes back Ready on
the retry — `RESTARTS 1` with nothing wrong. That is osixia finishing its
one-time config step and handing over to a fresh process. A restart count that
keeps climbing is a real fault; a single one at creation is not.

### Resetting the directory

Storage is `emptyDir`, so the tree is re-seeded from the ConfigMap on every
restart:

```sh
kubectl -n velox-test-ldap rollout restart deployment/openldap
```

Edit the LDIF in `openldap.yaml` and re-apply to change what gets seeded.

### TLS

Not shipped, and deliberately so. `auth_provider::ldap_backend` hardcodes
`verify_hostnames: true`, so LDAPS or StartTLS against this fixture needs a
certificate whose SAN carries `openldap.velox-test-ldap.svc.cluster.local` and
its CA pasted into the form's trusted-CA field. cert-manager is always present
on a bootstrapped cluster and can issue that, but `osixia/openldap:1.5.0`
silently skips its TLS setup when the certificate directory is mounted
read-only — it logs nothing and simply does not listen on 636, which reads
exactly like a network problem. Rather than ship a fixture that fails that way,
the plaintext path is the supported one. Test the TLS path against a real
directory.

## MinIO — what to paste into the snapshot form

Deployment → **Snapshots**:

| Field | Value |
| --- | --- |
| Bucket | `velox-snapshots` |
| Endpoint | `http://minio.minio.svc.cluster.local:9000` |
| Region | `us-east-1` |
| Path-style access | on |
| Access key | `veloxtest` |
| Secret key | `veloxtest-secret` |
| Base path | leave empty — filled with the deployment name |

The bucket is created at first boot by `MINIO_DEFAULT_BUCKETS`, because
`k8s::verify_snapshot_repo` surfaces a missing bucket as an error and the
control plane never creates one.

**Saving credentials restarts the OpenSearch nodes.** That is by design, not a
fault: the `repository-s3` plugin reads its keys from the keystore, which the
operator populates from a Secret at pod start, so a credential change is the
one snapshot edit that requires a roll (`snapshot::needs_restart`).

Console, for eyeballing what actually landed in the bucket:

```sh
kubectl -n minio port-forward svc/minio 9001:9001
# http://localhost:9001 — veloxtest / veloxtest-secret
```

## Multitenancy

With `VELOX_MULTITENANT_AUTH` off (the default) both fixtures are reachable
with nothing further applied: every session is `Scope::Admin`, deployments land
in the control-plane namespace, and no tenant NetworkPolicy set exists.

With the flag **on**, the ADR-044 tenant set is default-deny on both directions
and enumerates five flows. Two consequences:

- **MinIO works as-is** — but only because this fixture lands in a namespace
  called `minio`. Egress rule (5) opens port 9000 to the namespace named by
  `VELOX_MINIO_NS`, which `k8s::NamespaceLayout::current` defaults to `minio`.
  Move the fixture and you must also set `VELOX_MINIO_NAMESPACE` on the control
  plane, or snapshots die at the CNI with nothing pointing here.
- **LDAP does not.** The tenant set has no LDAP egress hole at all, so an
  OpenSearch node cannot reach any external directory. The failure surfaces as
  the security plugin timing out during a rolling restart — nothing in it names
  the network. Apply the narrow fix per tenant namespace:

  ```sh
  kubectl -n velox-t-<slug> apply -f tests/satellites/netpol-tenant-egress-ldap.yaml
  ```

  That file is a test-fixture workaround, not a product change: widening the
  ADR-044 flow set is an ADR decision.

Either way, enforcement is the CNI's job. k3s' embedded controller enforces
NetworkPolicy; a CNI without policy support accepts these objects and enforces
nothing.

## Images

| Image | Why this one |
| --- | --- |
| `osixia/openldap:1.5.0` | Env-driven seeding, anonymously pullable. Unmaintained upstream, which is acceptable for a fixture and is why `--copy-service` matters — without that argument the bootstrap never reads the mounted LDIF. |
| `bitnamilegacy/minio:2025.7.23-debian-12-r5` | `minio/minio` on Docker Hub no longer serves anonymous pulls — every tag, including pinned `RELEASE.*` ones, answers `denied: requested access to the resource is denied`. `bitnamilegacy` is the frozen Bitnami archive: real MinIO, still pullable, pinned to the last build published. |

Both are archives. A pull failure here means re-pointing the manifest, not a
broken cluster. Side-load them the same way you side-load the app image if the
target cluster has no egress:

```sh
docker pull osixia/openldap:1.5.0
docker save osixia/openldap:1.5.0 | sudo k3s ctr -n k8s.io images import -
```

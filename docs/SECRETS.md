# Secrets

Every secret VeloxSearch reads or creates, where it lives, who generates it, and
how to rotate it. This is the inventory the code refers to when it says
"docs/SECRETS.md".

The principle throughout: **VeloxSearch generates its own secrets with the OS
CSPRNG and stores them in Kubernetes Secrets.** Nothing is compiled into the
binary, nothing has a default password, and nothing is written to a log. The
only secrets an operator supplies by hand are for external systems VeloxSearch
does not own — an S3 endpoint, an LDAP directory, an SMTP relay.

## What the control plane creates

### `veloxsearch-credentials` — the control-plane admin

| | |
| --- | --- |
| Namespace | `veloxsearch-system` |
| Created by | `src/auth.rs`, at first-run setup |
| Keys | `user`, `hash` (bcrypt), `session_secret` |

The admin account created on the first-run screen. The password is stored as a
bcrypt hash — the plaintext exists only in the request that set it.
`session_secret` is 32 random bytes generated at the same moment and used to
HMAC-sign the session cookie.

**Rotation.** Change the password through the UI. To force every session to log
out, delete the Secret's `session_secret` key and restart the Pod — a new one is
generated, and every outstanding cookie stops verifying.

> **The development fallback.** When there is no managed Secret,
> `VELOX_SESSION_SECRET` is read from the environment, and when *that* is unset
> the code falls back to a well-known literal. That path exists so the binary
> runs on a laptop with no cluster. **Always set `VELOX_SESSION_SECRET`** if you
> run VeloxSearch outside the managed install — a bare process, a custom
> manifest, `docker run`. See [SECURITY.md](../SECURITY.md).

### `veloxsearch-postgres-credentials` — the control-plane store

| | |
| --- | --- |
| Namespace | `veloxsearch-system` |
| Created by | `src/db.rs` (`ensure_pg_secret`), at first boot with `VELOX_PG_ENABLED=1` |
| Keys | `superuser-password`, `app-password` |

Two CSPRNG passwords, generated exactly once and never rotated implicitly:

- `superuser-password` — the Postgres image's own superuser. Used only by the
  database Pod itself (entrypoint and initdb).
- `app-password` — the scoped, **non-superuser** `velox` role the application
  connects as, created by the initdb script in `deploy/install.yaml`.

The alphabet is alphanumeric only, deliberately: the value has to survive being
a psql `:'var'` literal, a URL component and a shell word.

**Rotation** is manual and two-sided, because doing it implicitly would lock the
app out of its own store:

```sh
kubectl -n veloxsearch-system exec sts/veloxsearch-postgres -- \
  psql -U postgres -c "ALTER ROLE velox PASSWORD 'new-password'"
kubectl -n veloxsearch-system patch secret veloxsearch-postgres-credentials \
  --type=json -p='[{"op":"replace","path":"/data/app-password","value":"'"$(printf %s 'new-password' | base64 -w0)"'"}]'
kubectl -n veloxsearch-system rollout restart deploy/veloxsearch
```

### Per-deployment OpenSearch admin

| | |
| --- | --- |
| Namespace | The deployment's own namespace |
| Created by | `src/k8s.rs`, at cluster provisioning |

Each provisioned OpenSearch cluster gets its own CSPRNG admin password —
there is no shared or default credential across deployments. The operator seeds
the admin hash from the `adminCredentialsSecret`.

**Rotation** goes through the UI. It is not a plain Secret edit: the admin user
is `is_reserved: true` in the operator-seeded security config, so it cannot be
changed through the OpenSearch security REST API. The reset path rewrites the
Secret and then forces the operator to reconcile and re-run its securityconfig
job so the new hash is applied.

### Per-deployment snapshot repository credentials

| | |
| --- | --- |
| Secret name | `<deployment>-snapshot-s3` |
| Created by | `src/snapshot.rs` / `src/k8s.rs`, from what the operator enters |
| Keys | `accessKey`, `secretKey` |

S3 credentials for the snapshot repository. **Operator-supplied** — these belong
to your object store, not to VeloxSearch.

The UI never returns a stored secret. When you edit a repository without
retyping the key, the form submits the sentinel `secret_kept` and the existing
value is preserved. A round-trip through the UI therefore cannot silently blank
a credential, and cannot leak one to a browser either.

## What the operator supplies

### LDAP / OIDC provider credentials

| | |
| --- | --- |
| Entered in | The deployment's authentication screen |
| Stored in | The deployment's namespace |

The LDAP bind password, and the OIDC client secret (read from
`VELOX_OIDC_CLIENT_SECRET` where it is provided by environment). A private CA
for an LDAPS directory is stored alongside as `idp-ca.pem` and parsed into the
rustls root store the pre-save probe dials with — so the probe reaches the
directory the same way the cluster will.

The same `__velox_secret_kept__` sentinel applies: editing a provider without
retyping the password preserves the stored one.

### SMTP relay credentials

| | |
| --- | --- |
| Variables | `VELOX_SMTP_HOST`, `VELOX_SMTP_PORT`, `VELOX_SMTP_USER`, `VELOX_SMTP_PASSWORD`, `VELOX_SMTP_TLS`, `VELOX_MAIL_FROM` |

Used only by the control-plane account flows (signup verification and
password-reset links), and **entirely optional**: with no `VELOX_SMTP_HOST` the
mailer logs the link instead of sending it, and the SMTP client is never dialled.

Put `VELOX_SMTP_PASSWORD` in a Kubernetes Secret and reference it with
`secretKeyRef` — not in the `veloxsearch-env` ConfigMap, which is for non-secret
configuration.

### Private registry pull credentials

Only needed if you pull the image from a private mirror; the default image is
public and needs none.

```sh
velox init --pull-token <token> --pull-user <user> --registry <host>
```

This creates a `kubernetes.io/dockerconfigjson` Secret named `velox-pull` in
`veloxsearch-system` before applying the manifest. See
[INSTALLER.md](INSTALLER.md).

### Integration registry token

`VELOX_REGISTRY_TOKEN` authenticates reads against a **private** mirror of the
integration registry. The default public registry needs no token.

This token protects *access to the catalog*, not the integrity of what comes
back — that is the signature check, and it is identical on every transport.

## What is deliberately not a secret

- **The integration signing key in `keys/`** is a public key. Publishing it is
  the point: verification works with no network and cannot be redirected by a
  compromised registry. The private half is never in this repository nor in the
  image — see [keys/README.md](../keys/README.md).
- **`VELOX_ADMIN_USER` / `VELOX_ADMIN_PASSWORD`** are a legacy break-glass path
  that predates the managed Secret. Prefer first-run setup; if you use them, they
  are credentials in the environment and should come from a Secret.
- **Everything else prefixed `VELOX_`** is non-secret configuration and belongs
  in the `veloxsearch-env` ConfigMap.

## Managing them with External Secrets

`deploy/secrets/external-secrets.aws.example.yaml` is a worked example of
sourcing these from AWS Secrets Manager (or HashiCorp Vault — swap the provider
block, the `ExternalSecret` objects are unchanged) under a `veloxsearch/` path
prefix. It contains references only, no values.

## Checklist for a production install

- [ ] `VELOX_SESSION_SECRET` set, or the managed `veloxsearch-credentials`
      Secret in place
- [ ] `VELOX_COOKIE_SECURE=1` if served over HTTPS
- [ ] `VELOX_SMTP_PASSWORD` from a Secret, never from the ConfigMap
- [ ] Snapshot S3 credentials scoped to the snapshot bucket only
- [ ] The generated Postgres passwords left as generated, or rotated on both
      sides together
- [ ] No credential in `deploy/install.yaml` as applied

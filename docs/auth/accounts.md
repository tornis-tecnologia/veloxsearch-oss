# Control-plane accounts

The multi-user half of authentication: self-serve signup, email verification,
password reset, and the login lookup that gives a session its tenant.

**This is off by default and the whole feature is flag-gated.** With
`VELOX_MULTITENANT_AUTH=0` — the default — every entry point returns
immediately, the four account endpoints answer 404, and the admin credential is
the only way in.

## The two authentication halves

| | `src/auth.rs` | `src/tenants.rs` |
| --- | --- | --- |
| Owns | The single admin credential, the HMAC session cookie | Users and tenants |
| Stored in | A Kubernetes Secret (`veloxsearch-credentials`) | Postgres |
| Flag | Always on | `VELOX_MULTITENANT_AUTH` |

They compose rather than replace each other: `auth.rs` still issues and verifies
every session cookie: `tenants.rs` decides which user and tenant a login resolves
to.

## Turning it on

Two flags, and one implies the other — there is nothing to store users in
without the datastore:

```yaml
VELOX_PG_ENABLED: "1"
VELOX_MULTITENANT_AUTH: "1"
```

Mail is optional but strongly recommended, because verification and reset links
are how an account is completed:

```yaml
VELOX_SMTP_HOST: "smtp.example.com"
VELOX_SMTP_TLS: "starttls"        # starttls (587) | tls (465) | none (25)
VELOX_SMTP_USER: "velox"
VELOX_MAIL_FROM: "VeloxSearch <no-reply@example.com>"
VELOX_PUBLIC_URL: "https://velox.example.com"
```

`VELOX_SMTP_PASSWORD` is a secret and belongs in a Secret — see
[../SECRETS.md](../SECRETS.md).

**With no `VELOX_SMTP_HOST`, the app logs each link at warn level instead of
sending it.** That is deliberate: the flow can be exercised end-to-end with no
relay. It is not a production configuration — anyone who can read the Pod's logs
can complete anyone's signup.

**Set `VELOX_PUBLIC_URL`** to how users actually reach the panel. It is the base
the emailed links are built from; unset, the links point at localhost.

## Two disciplines the code is shaped around

### User enumeration

A public signup form and a public reset form are both oracles for "does this
address have an account" — unless every branch answers identically. So they do:
signup on a taken address, reset for an unknown address, and both happy paths
all return the same `202`.

The cost is real and worth naming: "you already have an account" has to be said
in the **email** rather than in the HTTP response, which is the one channel a
stranger cannot read. A patch that makes the response more helpful by
distinguishing those cases is a security regression, not a UX improvement.

### Tokens are secrets, rows are not

Only the SHA-256 digest of a verification or reset token is stored (see
`migrations/002_auth_tokens.sql`). Lookup is by digest — an index probe against
a 190-bit random value, so there is no timing signal worth defending and a
database read cannot yield a usable token.

Tokens are single-use and expire; expired ones are refused and pruned.

## The corporate-email gate

Self-serve signup refuses free mailbox providers (`src/email_denylist.rs`,
ADR-038). The rule is conservative on purpose: a public suffix is **required**,
so a company whose domain happens to start with a provider's name still gets in
(`someone@gmail.acme.com` is accepted; `someone@gmail.com` is not).

The list is data, sorted, and asserted sorted by a test. Adding a provider is a
data change.

## Tenants

A tenant is the ownership boundary: it owns a namespace (`velox-t-<slug>`), and
a session's tenant is what `Scope` derives from. Slug collisions fall back to a
suffix.

The per-tenant namespace bundle — ResourceQuota, LimitRange, default-deny
NetworkPolicy — comes from `deploy/tenant-templates/`. See
[../PREMISES.md](../PREMISES.md) for why every deployment gets its own
namespace, and `deploy/tenant-templates/README.md` for what the bundle enforces
and, importantly, what it does not.

## Testing it

The account flows are `#[ignore]`d without a database:

```sh
docker run --rm -d -p 5433:5432 -e POSTGRES_PASSWORD=t --name velox-pg postgres:16-alpine
VELOX_PG_TEST_URL=postgres://postgres:t@127.0.0.1:5433/postgres \
  cargo test -- --ignored --skip dump_manifests --test-threads=1
```

They cover the whole path a stranger walks: sign up, verify, log in, reset —
plus the refusals (free mailbox, weak password, expired token) and the
enumeration-safety property.

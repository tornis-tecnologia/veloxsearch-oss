-- 001_init — control-plane schema bring-up (ADR-041, #92): users / tenants /
-- membership / quotas / audit. Applied by the idempotent runner in src/db.rs;
-- the version-ledger table is owned by the RUNNER (created before any
-- migration runs), so it is deliberately NOT created here.
--
-- Runs as the NON-superuser `velox` role, which owns the `velox` database
-- (deploy/install.yaml initdb script). citext is a TRUSTED extension since
-- PG13, so the database owner may create it without superuser.
CREATE EXTENSION IF NOT EXISTS citext;

CREATE TABLE users (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email          citext UNIQUE NOT NULL,          -- case-insensitive identity (cf. ADR-036)
    password_hash  text   NOT NULL,                 -- bcrypt, same scheme as src/auth.rs
    email_verified boolean NOT NULL DEFAULT false,  -- ADR-038 denylist applies at signup (#93)
    status         text   NOT NULL DEFAULT 'active'
                   CHECK (status IN ('active', 'suspended')),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tenants (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    slug         text UNIQUE NOT NULL,   -- URL/label-safe tenant handle
    namespace    text UNIQUE NOT NULL,   -- the tenant→K8s-namespace mapping (#80/#94 reads)
    display_name text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- Membership + ownership (M:N). Postgres is authoritative on ownership; the
-- `veloxsearch.io/tenant` CR label is a denormalized cache (ADR-041 boundary).
CREATE TABLE tenant_users (
    tenant_id  uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id    uuid NOT NULL REFERENCES users(id)   ON DELETE CASCADE,
    role       text NOT NULL DEFAULT 'member'
               CHECK (role IN ('owner', 'member')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);
CREATE INDEX tenant_users_user_idx ON tenant_users (user_id);

-- Per-tenant limits; feeds the quota admission gate (#84's build).
CREATE TABLE quotas (
    tenant_id         uuid PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
    max_deployments   integer NOT NULL DEFAULT 1,
    max_total_disk_gb integer NOT NULL DEFAULT 50,
    max_nodes         integer NOT NULL DEFAULT 3,  -- sizing is 3-node per ADR-016
    updated_at        timestamptz NOT NULL DEFAULT now()
);

-- Authoritative control-plane audit (subsumes ADR-003's OTEL carve-out):
-- durable, queryable per tenant, independent of OpenSearch availability.
CREATE TABLE audit (
    id            bigserial PRIMARY KEY,
    at            timestamptz NOT NULL DEFAULT now(),
    actor_user_id uuid REFERENCES users(id),   -- nullable: system/bootstrap actions
    tenant_id     uuid REFERENCES tenants(id), -- nullable: account-level events
    action        text NOT NULL,               -- e.g. deployment.create, tenant.invite, login
    target        text,                        -- deployment name / namespace / user
    detail        jsonb NOT NULL DEFAULT '{}'
);
CREATE INDEX audit_tenant_at_idx ON audit (tenant_id, at DESC);

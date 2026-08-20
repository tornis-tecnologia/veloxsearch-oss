-- 002_auth_tokens — single-use tokens for the self-serve account flows (#79):
-- email verification and password reset. Applied by the same idempotent runner
-- in src/db.rs; the version ledger stays runner-owned (see 001_init).
--
-- Only the SHA-256 HEX DIGEST of a token is stored, never the token itself:
-- the raw value exists solely inside the mail we send, so a database read (or
-- a leaked backup) yields nothing that can be redeemed. Lookup is by digest,
-- so it is an index probe rather than a scan — no timing side channel to
-- defend, unlike a comparison against a stored plaintext secret.
--
-- Tokens are consumed, not deleted: `consumed_at` keeps a replay of the same
-- link a no-op that is visible in the row rather than an indistinguishable
-- "unknown token". Expired/consumed rows are pruned opportunistically by the
-- issuing path (src/tenants.rs), which needs no scheduler.

CREATE TABLE email_verification_tokens (
    token_hash  text PRIMARY KEY,                    -- sha256 hex of the emailed token
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at  timestamptz NOT NULL,
    consumed_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX email_verification_tokens_user_idx ON email_verification_tokens (user_id);

CREATE TABLE password_reset_tokens (
    token_hash  text PRIMARY KEY,                    -- sha256 hex of the emailed token
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at  timestamptz NOT NULL,
    consumed_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX password_reset_tokens_user_idx ON password_reset_tokens (user_id);

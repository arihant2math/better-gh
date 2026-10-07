-- P50: account and repository lifecycle (renames with redirects, soft
-- delete and restore of repositories, transfers to users with acceptance).
-- Code: bgh_core::lifecycle, bgh-repos (lifecycle.rs), bgh-accounts.

-- Old logins of renamed users and organizations. Owner lookups that miss
-- `users` fall back here (bgh_core::lifecycle::resolve_owner); every repo
-- the account owned at rename time also gets a `repo_redirects` row. The
-- old login stays reserved for its account until `reserved_until`.
CREATE TABLE login_redirects (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    old_login       TEXT NOT NULL,
    user_id         BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    reserved_until  TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX login_redirects_login_key ON login_redirects (lower(old_login));
CREATE INDEX login_redirects_user_idx ON login_redirects (user_id, created_at);

-- Enforce the reservation for every code path that creates or renames an
-- account (sign-up, admin, SSO/LDAP provisioning, importer): taking a
-- reserved login fails like a duplicate login (`users_login_key`), so the
-- existing 422 `already_exists` mapping applies. A login that is taken
-- legitimately (expired reservation, or the account reclaiming its own old
-- name) drops the redirect.
CREATE OR REPLACE FUNCTION bgh_login_reservation() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND lower(NEW.login) = lower(OLD.login) THEN
        RETURN NEW;
    END IF;
    IF EXISTS (SELECT 1 FROM login_redirects
                WHERE lower(old_login) = lower(NEW.login)
                  AND user_id <> NEW.id AND reserved_until > now()) THEN
        RAISE EXCEPTION 'login % is reserved', NEW.login
            USING ERRCODE = 'unique_violation', CONSTRAINT = 'users_login_key';
    END IF;
    DELETE FROM login_redirects WHERE lower(old_login) = lower(NEW.login);
    RETURN NEW;
END $$;

CREATE TRIGGER users_login_reservation BEFORE INSERT OR UPDATE OF login ON users
    FOR EACH ROW EXECUTE FUNCTION bgh_login_reservation();

-- Soft-deleted repositories. The `repositories` row and every row that
-- cascades from it are removed (so the name is free at once and no query
-- elsewhere needs a deleted filter) and kept here as a snapshot that
-- `POST /_bgh/repos/{id}/restore` re-inserts with the same ids. Git
-- storage stays on disk until `purge_after` (repos.purge_deleted service).
CREATE TABLE deleted_repositories (
    id             BIGINT PRIMARY KEY,
    -- No FK: the owner may be deleted too (then only a site admin sees it).
    owner_id       BIGINT NOT NULL,
    owner_login    TEXT NOT NULL,
    name           TEXT NOT NULL,
    visibility     TEXT NOT NULL,
    fork           BOOLEAN NOT NULL DEFAULT false,
    deleted_by_id  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    deleted_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    purge_after    TIMESTAMPTZ NOT NULL,
    -- Direct forks at deletion time (made self-contained before purging).
    forks          BIGINT[] NOT NULL DEFAULT '{}',
    -- Content-addressed blobs the snapshot references, kept by the LFS and
    -- attachment garbage collectors until the purge.
    lfs_oids       TEXT[] NOT NULL DEFAULT '{}',
    blob_shas      TEXT[] NOT NULL DEFAULT '{}',
    -- {"tables": [{"table", "rows"}], "relinks": [...]} (lifecycle::snapshot)
    snapshot       JSONB NOT NULL
);
CREATE INDEX deleted_repositories_owner_idx ON deleted_repositories (owner_id, deleted_at DESC);
CREATE INDEX deleted_repositories_purge_idx ON deleted_repositories (purge_after);
CREATE INDEX deleted_repositories_deleted_by_idx ON deleted_repositories (deleted_by_id);

-- Pending transfers to another user, accepted by the recipient within a
-- day. Transfers to organizations stay immediate.
CREATE TABLE repo_transfers (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    from_owner_id    BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    to_user_id       BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    new_name         TEXT NOT NULL,
    requested_by_id  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at       TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX repo_transfers_repo_key ON repo_transfers (repo_id);
CREATE INDEX repo_transfers_to_idx ON repo_transfers (to_user_id, created_at DESC);
CREATE INDEX repo_transfers_from_idx ON repo_transfers (from_owner_id);
CREATE INDEX repo_transfers_requested_by_idx ON repo_transfers (requested_by_id);
CREATE INDEX repo_transfers_expires_idx ON repo_transfers (expires_at);

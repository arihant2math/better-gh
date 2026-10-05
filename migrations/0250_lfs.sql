-- Git LFS (package git-transport).

-- Which repository may serve which LFS object. Objects are stored once on
-- disk ({data_dir}/lfs, content addressed) and shared across repositories.
CREATE TABLE lfs_objects (
    repo_id      BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    oid          TEXT NOT NULL CHECK (oid ~ '^[0-9a-f]{64}$'),
    size         BIGINT NOT NULL CHECK (size >= 0),
    uploader_id  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, oid)
);
-- Garbage collection: is an oid still referenced by any repository?
CREATE INDEX lfs_objects_oid_idx ON lfs_objects (oid);

-- Total bytes of LFS objects per repository (maintained with lfs_objects).
ALTER TABLE repositories ADD COLUMN lfs_size BIGINT NOT NULL DEFAULT 0;

-- LFS file locks (https://github.com/git-lfs/git-lfs/blob/main/docs/api/locking.md).
CREATE TABLE lfs_locks (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    ref_name    TEXT,
    owner_id    BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX lfs_locks_repo_path_key ON lfs_locks (repo_id, path);
CREATE INDEX lfs_locks_repo_idx ON lfs_locks (repo_id, id);
CREATE INDEX lfs_locks_owner_idx ON lfs_locks (owner_id);

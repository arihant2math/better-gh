-- P6: user attachments (images, videos and files pasted into comments,
-- PR descriptions, release notes and wiki pages).
--
-- Blobs are content-addressed under {data_dir}/files/attachments/ by
-- sha256; rows reference them. Attachments of a repository go away with it
-- (CASCADE; the blob is collected by the `uploads.gc` job), so a private
-- repository's attachment can never outlive its access check.
CREATE TABLE attachments (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    uuid          UUID NOT NULL UNIQUE,
    uploader_id   BIGINT REFERENCES users (id) ON DELETE SET NULL,
    -- Account whose storage quota the upload counts against.
    owner_id      BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- NULL: not tied to a repository (served publicly, like GitHub).
    repo_id       BIGINT REFERENCES repositories (id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    content_type  TEXT NOT NULL,
    size          BIGINT NOT NULL CHECK (size >= 0),
    sha256        TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX attachments_owner_idx ON attachments (owner_id);
CREATE INDEX attachments_repo_idx ON attachments (repo_id) WHERE repo_id IS NOT NULL;
CREATE INDEX attachments_uploader_idx ON attachments (uploader_id);
CREATE INDEX attachments_sha256_idx ON attachments (sha256);

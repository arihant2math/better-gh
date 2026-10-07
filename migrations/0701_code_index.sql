-- Code and commit search index (default branch of every repository).
--
-- Blob contents are stored once per blob SHA (forks and unchanged files
-- share rows) with a trigram index, so substring (ILIKE) and regex (~*)
-- searches are index-assisted. `code_files` maps each repository path on
-- the indexed commit to its blob; indexing is incremental by blob SHA.

CREATE TABLE code_blobs (
    sha      TEXT PRIMARY KEY,
    content  TEXT NOT NULL,
    size     INTEGER NOT NULL
);
CREATE INDEX code_blobs_content_trgm_idx ON code_blobs USING gin (content gin_trgm_ops);

CREATE TABLE code_files (
    repo_id    BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    path       TEXT NOT NULL,
    blob_sha   TEXT NOT NULL,
    -- File name and lowercased extension (without the dot), for filename:/extension:.
    name       TEXT NOT NULL,
    extension  TEXT,
    language   TEXT,
    size       INTEGER NOT NULL,
    PRIMARY KEY (repo_id, path)
);
CREATE INDEX code_files_blob_idx ON code_files (blob_sha);
CREATE INDEX code_files_path_trgm_idx ON code_files USING gin (lower(path) gin_trgm_ops);
CREATE INDEX code_files_language_idx ON code_files (lower(language), repo_id);
CREATE INDEX code_files_extension_idx ON code_files (extension, repo_id);

CREATE TABLE code_index_state (
    repo_id      BIGINT PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    ref          TEXT NOT NULL,
    commit_sha   TEXT NOT NULL,
    file_count   INTEGER NOT NULL DEFAULT 0,
    indexed_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Commits reachable from the default branch.
CREATE TABLE commit_index (
    repo_id          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    sha              TEXT NOT NULL,
    tree_sha         TEXT NOT NULL,
    parents          TEXT[] NOT NULL DEFAULT '{}',
    message          TEXT NOT NULL,
    author_name      TEXT NOT NULL,
    author_email     TEXT NOT NULL,
    author_date      TIMESTAMPTZ NOT NULL,
    committer_name   TEXT NOT NULL,
    committer_email  TEXT NOT NULL,
    committer_date   TIMESTAMPTZ NOT NULL,
    -- Users matched by verified email at index time.
    author_id        BIGINT REFERENCES users (id) ON DELETE SET NULL,
    committer_id     BIGINT REFERENCES users (id) ON DELETE SET NULL,
    search           TSVECTOR GENERATED ALWAYS AS (to_tsvector('english', message)) STORED,
    PRIMARY KEY (repo_id, sha)
);
CREATE INDEX commit_index_search_idx ON commit_index USING gin (search);
CREATE INDEX commit_index_sha_idx ON commit_index (sha);
CREATE INDEX commit_index_author_date_idx ON commit_index (author_date DESC);
CREATE INDEX commit_index_committer_date_idx ON commit_index (committer_date DESC);
CREATE INDEX commit_index_author_idx ON commit_index (author_id) WHERE author_id IS NOT NULL;
CREATE INDEX commit_index_committer_idx ON commit_index (committer_id) WHERE committer_id IS NOT NULL;
CREATE INDEX commit_index_author_email_idx ON commit_index (lower(author_email));

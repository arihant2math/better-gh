-- P32: commit comments (`/repos/{o}/{r}/commits/{sha}/comments`,
-- `/repos/{o}/{r}/comments/{id}`). Reactions use `reactions` with
-- subject_type 'commit_comment' (already allowed by 0003).
CREATE TABLE commit_comments (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    commit_id   TEXT NOT NULL,
    path        TEXT,
    -- Line index in the commit's diff of `path` (GitHub's `position`).
    position    INTEGER,
    -- Line number in the new version of `path`.
    line        INTEGER,
    body        TEXT NOT NULL,
    user_id     BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX commit_comments_repo_commit_idx ON commit_comments (repo_id, commit_id, id);
CREATE INDEX commit_comments_repo_idx ON commit_comments (repo_id, id);
CREATE INDEX commit_comments_user_idx ON commit_comments (user_id);

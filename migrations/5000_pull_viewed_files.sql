-- P38: server-side "Viewed" state of PR files (per reviewer). A row means
-- `user_id` marked `path` as viewed when its diff blob was `blob_sha`; the
-- file counts as viewed only while the PR diff still has that blob
-- (GraphQL `viewerViewedState`: VIEWED, DISMISSED when it changed since).
CREATE TABLE pull_viewed_files (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    pull_id     BIGINT NOT NULL REFERENCES pull_requests (issue_id) ON DELETE CASCADE,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    blob_sha    TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX pull_viewed_files_key ON pull_viewed_files (pull_id, user_id, path);
CREATE INDEX pull_viewed_files_user ON pull_viewed_files (user_id, pull_id);
CREATE INDEX pull_viewed_files_repo ON pull_viewed_files (repo_id);

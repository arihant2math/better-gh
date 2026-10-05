-- P4: links between issues and the pull requests that close them
-- (closing keywords in the PR body, or linked manually).
CREATE TABLE issue_pr_links (
    issue_id   BIGINT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    -- Issue id of the pull request.
    pull_id    BIGINT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    source     TEXT NOT NULL CHECK (source IN ('keyword', 'manual')),
    created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (issue_id, pull_id)
);
CREATE INDEX issue_pr_links_pull_idx ON issue_pr_links (pull_id);
CREATE INDEX issue_pr_links_created_by_idx ON issue_pr_links (created_by);

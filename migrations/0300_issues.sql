-- bgh-issues: mentions, sub-issues, pinned issues, transfers, indexes for
-- issue list filters.

-- Users @mentioned in an issue body or any of its comments
-- (GET .../issues?mentioned=login, /issues?filter=mentioned).
CREATE TABLE issue_mentions (
    issue_id    BIGINT NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (issue_id, user_id)
);
CREATE INDEX issue_mentions_user_idx ON issue_mentions (user_id, issue_id);

-- Sub-issues: each issue has at most one parent.
CREATE TABLE sub_issues (
    child_id    BIGINT PRIMARY KEY REFERENCES issues (id) ON DELETE CASCADE,
    parent_id   BIGINT NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    -- Ordering among siblings (ascending).
    position    DOUBLE PRECISION NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (child_id <> parent_id)
);
CREATE INDEX sub_issues_parent_idx ON sub_issues (parent_id, position);

-- Pinned issues (at most 3 per repository, enforced in code).
CREATE TABLE pinned_issues (
    issue_id      BIGINT PRIMARY KEY REFERENCES issues (id) ON DELETE CASCADE,
    repo_id       BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    position      INTEGER NOT NULL DEFAULT 0,
    pinned_by_id  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX pinned_issues_repo_idx ON pinned_issues (repo_id, position);

-- Old (repo, number) of transferred issues, for 301 redirects.
CREATE TABLE issue_transfers (
    old_repo_id  BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    old_number   BIGINT NOT NULL,
    issue_id     BIGINT NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (old_repo_id, old_number)
);
CREATE INDEX issue_transfers_issue_idx ON issue_transfers (issue_id);

-- List sort orders (created / comments) and the `since` filter.
CREATE INDEX issues_repo_state_created_idx ON issues (repo_id, state, created_at DESC, id DESC);
CREATE INDEX issues_repo_state_comments_idx ON issues (repo_id, state, comments_count DESC, id DESC);
CREATE INDEX issues_repo_updated_idx ON issues (repo_id, updated_at DESC, id DESC);
CREATE INDEX issues_closed_by_idx ON issues (closed_by_id) WHERE closed_by_id IS NOT NULL;
-- Repo-wide comment listing sorted by creation.
CREATE INDEX comments_repo_created_idx ON comments (repo_id, created_at, id);
CREATE INDEX comments_author_idx ON comments (author_id);
-- Per-issue event listing in id order and dedup lookups by kind.
CREATE INDEX issue_events_issue_event_idx ON issue_events (issue_id, event);
CREATE INDEX issue_events_actor_idx ON issue_events (actor_id);
CREATE INDEX reactions_user_idx ON reactions (user_id);

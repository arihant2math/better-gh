-- Core schema: pull requests, reviews, statuses and checks.

CREATE TABLE pull_requests (
    issue_id               BIGINT PRIMARY KEY REFERENCES issues (id) ON DELETE CASCADE,
    -- Base repository (same as issues.repo_id).
    repo_id                BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    -- NULL when the head fork was deleted.
    head_repo_id           BIGINT REFERENCES repositories (id) ON DELETE SET NULL,
    head_ref               TEXT NOT NULL,
    head_sha               TEXT NOT NULL,
    base_ref               TEXT NOT NULL,
    base_sha               TEXT NOT NULL,
    merge_base_sha         TEXT,
    merge_commit_sha       TEXT,
    merged                 BOOLEAN NOT NULL DEFAULT false,
    merged_at              TIMESTAMPTZ,
    merged_by_id           BIGINT REFERENCES users (id) ON DELETE SET NULL,
    -- NULL = not yet computed.
    mergeable              BOOLEAN,
    rebaseable             BOOLEAN,
    mergeable_state        TEXT NOT NULL DEFAULT 'unknown' CHECK (mergeable_state IN (
                               'unknown', 'clean', 'dirty', 'blocked', 'behind',
                               'unstable', 'has_hooks', 'draft')),
    draft                  BOOLEAN NOT NULL DEFAULT false,
    maintainer_can_modify  BOOLEAN NOT NULL DEFAULT false,
    -- {"enabled_by_id", "merge_method", "commit_title", "commit_message"} or NULL
    auto_merge             JSONB,
    additions              BIGINT NOT NULL DEFAULT 0,
    deletions              BIGINT NOT NULL DEFAULT 0,
    changed_files          BIGINT NOT NULL DEFAULT 0,
    commits                BIGINT NOT NULL DEFAULT 0,
    review_comments_count  BIGINT NOT NULL DEFAULT 0
);
CREATE INDEX pull_requests_base_idx ON pull_requests (repo_id, base_ref);
CREATE INDEX pull_requests_head_idx ON pull_requests (head_repo_id, head_ref);
CREATE INDEX pull_requests_head_sha_idx ON pull_requests (head_sha);

CREATE TABLE pr_reviews (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    pull_id       BIGINT NOT NULL REFERENCES pull_requests (issue_id) ON DELETE CASCADE,
    repo_id       BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    user_id       BIGINT REFERENCES users (id) ON DELETE SET NULL,
    body          TEXT NOT NULL DEFAULT '',
    state         TEXT NOT NULL DEFAULT 'PENDING' CHECK (state IN (
                      'PENDING', 'COMMENTED', 'APPROVED', 'CHANGES_REQUESTED', 'DISMISSED')),
    commit_id     TEXT,
    submitted_at  TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX pr_reviews_pull_idx ON pr_reviews (pull_id, id);
CREATE INDEX pr_reviews_user_idx ON pr_reviews (user_id);

CREATE TABLE pr_review_comments (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    pull_id              BIGINT NOT NULL REFERENCES pull_requests (issue_id) ON DELETE CASCADE,
    repo_id              BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    review_id            BIGINT REFERENCES pr_reviews (id) ON DELETE CASCADE,
    in_reply_to_id       BIGINT REFERENCES pr_review_comments (id) ON DELETE SET NULL,
    user_id              BIGINT REFERENCES users (id) ON DELETE SET NULL,
    body                 TEXT NOT NULL DEFAULT '',
    path                 TEXT NOT NULL,
    commit_id            TEXT NOT NULL,
    original_commit_id   TEXT NOT NULL,
    diff_hunk            TEXT NOT NULL DEFAULT '',
    subject_type         TEXT NOT NULL DEFAULT 'line' CHECK (subject_type IN ('line', 'file')),
    side                 TEXT CHECK (side IN ('LEFT', 'RIGHT')),
    start_side           TEXT CHECK (start_side IN ('LEFT', 'RIGHT')),
    line                 INTEGER,
    original_line        INTEGER,
    start_line           INTEGER,
    original_start_line  INTEGER,
    position             INTEGER,
    original_position    INTEGER,
    -- Set on the thread's root comment.
    resolved_at          TIMESTAMPTZ,
    resolved_by_id       BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX pr_review_comments_pull_idx ON pr_review_comments (pull_id, id);
CREATE INDEX pr_review_comments_review_idx ON pr_review_comments (review_id);
CREATE INDEX pr_review_comments_repo_updated_idx ON pr_review_comments (repo_id, updated_at);

CREATE TABLE pr_requested_reviewers (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    pull_id     BIGINT NOT NULL REFERENCES pull_requests (issue_id) ON DELETE CASCADE,
    user_id     BIGINT REFERENCES users (id) ON DELETE CASCADE,
    team_id     BIGINT REFERENCES teams (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((user_id IS NULL) <> (team_id IS NULL))
);
CREATE UNIQUE INDEX pr_requested_reviewers_user_key
    ON pr_requested_reviewers (pull_id, user_id) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX pr_requested_reviewers_team_key
    ON pr_requested_reviewers (pull_id, team_id) WHERE team_id IS NOT NULL;

-- Legacy commit statuses API.
CREATE TABLE commit_statuses (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id      BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    sha          TEXT NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('error', 'failure', 'pending', 'success')),
    context      TEXT NOT NULL DEFAULT 'default',
    description  TEXT,
    target_url   TEXT,
    avatar_url   TEXT,
    creator_id   BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX commit_statuses_sha_idx ON commit_statuses (repo_id, sha, context, id DESC);

CREATE TABLE check_suites (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id        BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    head_sha       TEXT NOT NULL,
    head_branch    TEXT,
    before_sha     TEXT,
    after_sha      TEXT,
    -- Owning integration ('actions' for built-in CI, or an app slug).
    app_slug       TEXT NOT NULL DEFAULT 'actions',
    status         TEXT NOT NULL DEFAULT 'queued' CHECK (status IN (
                       'queued', 'in_progress', 'completed', 'waiting', 'requested', 'pending')),
    conclusion     TEXT CHECK (conclusion IN (
                       'success', 'failure', 'neutral', 'cancelled', 'skipped',
                       'timed_out', 'action_required', 'stale', 'startup_failure')),
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX check_suites_sha_idx ON check_suites (repo_id, head_sha);

CREATE TABLE check_runs (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    check_suite_id  BIGINT NOT NULL REFERENCES check_suites (id) ON DELETE CASCADE,
    repo_id         BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    head_sha        TEXT NOT NULL,
    name            TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'queued' CHECK (status IN (
                        'queued', 'in_progress', 'completed', 'waiting', 'requested', 'pending')),
    conclusion      TEXT CHECK (conclusion IN (
                        'success', 'failure', 'neutral', 'cancelled', 'skipped',
                        'timed_out', 'action_required', 'stale')),
    external_id     TEXT,
    details_url     TEXT,
    -- {"title", "summary", "text", "annotations_count", "annotations_url"}
    output          JSONB NOT NULL DEFAULT '{}',
    started_at      TIMESTAMPTZ,
    completed_at    TIMESTAMPTZ,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX check_runs_sha_idx ON check_runs (repo_id, head_sha, name);
CREATE INDEX check_runs_suite_idx ON check_runs (check_suite_id);

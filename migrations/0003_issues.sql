-- Core schema: labels, milestones, issues (and PR conversation), comments,
-- reactions, timeline events.

CREATE TABLE labels (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id      BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    -- 6 hex chars, no leading '#'.
    color        TEXT NOT NULL DEFAULT 'ededed' CHECK (color ~ '^[0-9a-fA-F]{6}$'),
    description  TEXT,
    is_default   BOOLEAN NOT NULL DEFAULT false,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX labels_repo_name_key ON labels (repo_id, lower(name));

CREATE TABLE milestones (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id        BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    number         BIGINT NOT NULL,
    title          TEXT NOT NULL,
    description    TEXT,
    state          TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'closed')),
    creator_id     BIGINT REFERENCES users (id) ON DELETE SET NULL,
    -- Denormalized counters, maintained by bgh-issues.
    open_issues    BIGINT NOT NULL DEFAULT 0,
    closed_issues  BIGINT NOT NULL DEFAULT 0,
    due_on         TIMESTAMPTZ,
    closed_at      TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, number)
);
CREATE UNIQUE INDEX milestones_repo_title_key ON milestones (repo_id, lower(title));

-- Issues and pull requests share this table (pull_requests holds PR-only data).
CREATE TABLE issues (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id            BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    number             BIGINT NOT NULL,
    title              TEXT NOT NULL,
    body               TEXT,
    state              TEXT NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'closed')),
    state_reason       TEXT CHECK (state_reason IN ('completed', 'not_planned', 'reopened', 'duplicate')),
    -- NULL author renders as the "ghost" user.
    author_id          BIGINT REFERENCES users (id) ON DELETE SET NULL,
    is_pull_request    BOOLEAN NOT NULL DEFAULT false,
    milestone_id       BIGINT REFERENCES milestones (id) ON DELETE SET NULL,
    locked             BOOLEAN NOT NULL DEFAULT false,
    active_lock_reason TEXT CHECK (active_lock_reason IN ('off-topic', 'too heated', 'resolved', 'spam')),
    comments_count     BIGINT NOT NULL DEFAULT 0,
    closed_at          TIMESTAMPTZ,
    closed_by_id       BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    search             TSVECTOR GENERATED ALWAYS AS (
                           setweight(to_tsvector('english', coalesce(title, '')), 'A') ||
                           setweight(to_tsvector('english', coalesce(body, '')), 'B')
                       ) STORED,
    UNIQUE (repo_id, number)
);
CREATE INDEX issues_repo_state_updated_idx ON issues (repo_id, state, updated_at DESC);
CREATE INDEX issues_repo_kind_state_idx ON issues (repo_id, is_pull_request, state, number DESC);
CREATE INDEX issues_author_idx ON issues (author_id, updated_at DESC);
CREATE INDEX issues_milestone_idx ON issues (milestone_id) WHERE milestone_id IS NOT NULL;
CREATE INDEX issues_search_idx ON issues USING GIN (search);

CREATE TABLE issue_assignees (
    issue_id    BIGINT NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (issue_id, user_id)
);
CREATE INDEX issue_assignees_user_idx ON issue_assignees (user_id);

CREATE TABLE issue_labels (
    issue_id    BIGINT NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    label_id    BIGINT NOT NULL REFERENCES labels (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (issue_id, label_id)
);
CREATE INDEX issue_labels_label_idx ON issue_labels (label_id);

-- Issue (and PR conversation) comments.
CREATE TABLE comments (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    issue_id    BIGINT NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    -- Denormalized for repo-wide listing and sync scoping.
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    author_id   BIGINT REFERENCES users (id) ON DELETE SET NULL,
    body        TEXT NOT NULL DEFAULT '',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX comments_issue_idx ON comments (issue_id, created_at, id);
CREATE INDEX comments_repo_updated_idx ON comments (repo_id, updated_at);

CREATE TABLE reactions (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    subject_type  TEXT NOT NULL CHECK (subject_type IN (
                      'issue', 'issue_comment', 'pull_request_review_comment',
                      'commit_comment', 'release', 'discussion', 'discussion_comment')),
    subject_id    BIGINT NOT NULL,
    user_id       BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    content       TEXT NOT NULL CHECK (content IN (
                      '+1', '-1', 'laugh', 'confused', 'heart', 'hooray', 'rocket', 'eyes')),
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (subject_type, subject_id, user_id, content)
);
CREATE INDEX reactions_subject_idx ON reactions (subject_type, subject_id);

-- Timeline / issue events (labeled, assigned, closed, referenced, ...).
-- `event` uses GitHub's event names; `data` holds event-specific fields
-- (e.g. {"label": {"name", "color"}} or {"assignee_id": 1}).
CREATE TABLE issue_events (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    issue_id    BIGINT NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    actor_id    BIGINT REFERENCES users (id) ON DELETE SET NULL,
    event       TEXT NOT NULL,
    commit_id   TEXT,
    data        JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX issue_events_issue_idx ON issue_events (issue_id, created_at, id);
CREATE INDEX issue_events_repo_idx ON issue_events (repo_id, id);

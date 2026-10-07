-- Projects (GitHub Projects v2 semantics), owned by a user or an organization.

-- Per-owner project number sequence.
CREATE TABLE project_counters (
    owner_id     BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    next_number  BIGINT NOT NULL DEFAULT 1
);

CREATE TABLE projects (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_id           BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    number             BIGINT NOT NULL,
    title              TEXT NOT NULL,
    short_description  TEXT,
    readme             TEXT,
    public             BOOLEAN NOT NULL DEFAULT false,
    closed             BOOLEAN NOT NULL DEFAULT false,
    closed_at          TIMESTAMPTZ,
    creator_id         BIGINT REFERENCES users (id) ON DELETE SET NULL,
    next_view_number   BIGINT NOT NULL DEFAULT 1,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (owner_id, number)
);
CREATE INDEX projects_owner_idx ON projects (owner_id, closed, updated_at DESC);

CREATE TABLE project_linked_repos (
    project_id  BIGINT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, repo_id)
);
CREATE INDEX project_linked_repos_repo_idx ON project_linked_repos (repo_id);

-- Fields. Built-ins (title, assignees, status, labels, repository, milestone)
-- are created with the project; `status` is a single select.
CREATE TABLE project_fields (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id  BIGINT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    data_type   TEXT NOT NULL CHECK (data_type IN (
                    'title', 'assignees', 'status', 'labels', 'repository', 'milestone',
                    'text', 'number', 'date', 'single_select', 'iteration')),
    position    INT NOT NULL DEFAULT 0,
    -- single_select / status: [{"id","name","color","description"}]
    options     JSONB,
    -- iteration: {"startDate","duration","iterations":[{"id","title","startDate","duration"}]}
    iterations  JSONB,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX project_fields_project_idx ON project_fields (project_id, position, id);
CREATE UNIQUE INDEX project_fields_name_key ON project_fields (project_id, lower(name));

CREATE TABLE project_views (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id         BIGINT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    number             BIGINT NOT NULL,
    name               TEXT NOT NULL,
    layout             TEXT NOT NULL DEFAULT 'table' CHECK (layout IN ('table', 'board', 'roadmap')),
    position           INT NOT NULL DEFAULT 0,
    filter             TEXT NOT NULL DEFAULT '',
    group_by_field_id  BIGINT REFERENCES project_fields (id) ON DELETE SET NULL,
    column_field_id    BIGINT REFERENCES project_fields (id) ON DELETE SET NULL,
    date_field_id      BIGINT REFERENCES project_fields (id) ON DELETE SET NULL,
    -- [{"fieldId","direction"}]
    sort_by            JSONB NOT NULL DEFAULT '[]',
    visible_field_ids  BIGINT[] NOT NULL DEFAULT '{}',
    hidden_column_ids  TEXT[] NOT NULL DEFAULT '{}',
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, number)
);
CREATE INDEX project_views_project_idx ON project_views (project_id, position, id);
CREATE INDEX project_views_group_field_idx ON project_views (group_by_field_id) WHERE group_by_field_id IS NOT NULL;
CREATE INDEX project_views_column_field_idx ON project_views (column_field_id) WHERE column_field_id IS NOT NULL;
CREATE INDEX project_views_date_field_idx ON project_views (date_field_id) WHERE date_field_id IS NOT NULL;

CREATE TABLE project_items (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id       BIGINT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    content_type     TEXT NOT NULL CHECK (content_type IN ('Issue', 'PullRequest', 'DraftIssue')),
    issue_id         BIGINT REFERENCES issues (id) ON DELETE CASCADE,
    -- Draft issues only.
    title            TEXT,
    body             TEXT,
    assignee_ids     BIGINT[] NOT NULL DEFAULT '{}',
    archived         BOOLEAN NOT NULL DEFAULT false,
    -- Fractional index (manual order); per-view overrides in view_positions.
    position         TEXT NOT NULL COLLATE "C",
    view_positions   JSONB NOT NULL DEFAULT '{}',
    creator_id       BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((content_type = 'DraftIssue') = (issue_id IS NULL))
);
CREATE INDEX project_items_project_idx ON project_items (project_id, position);
CREATE UNIQUE INDEX project_items_issue_key ON project_items (project_id, issue_id) WHERE issue_id IS NOT NULL;
CREATE INDEX project_items_issue_idx ON project_items (issue_id) WHERE issue_id IS NOT NULL;

-- Custom field values (built-ins come from the issue / draft).
CREATE TABLE project_item_values (
    item_id     BIGINT NOT NULL REFERENCES project_items (id) ON DELETE CASCADE,
    field_id    BIGINT NOT NULL REFERENCES project_fields (id) ON DELETE CASCADE,
    value       JSONB NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (item_id, field_id)
);
CREATE INDEX project_item_values_field_idx ON project_item_values (field_id);

CREATE TABLE project_workflows (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id  BIGINT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN (
                    'item_added', 'item_reopened', 'item_closed', 'pr_merged', 'auto_add', 'auto_archive')),
    enabled     BOOLEAN NOT NULL DEFAULT false,
    config      JSONB NOT NULL DEFAULT '{}',
    -- auto_add: repositories to watch (denormalized from config for lookup).
    repo_ids    BIGINT[] NOT NULL DEFAULT '{}',
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, kind)
);
CREATE INDEX project_workflows_repos_idx ON project_workflows USING GIN (repo_ids) WHERE enabled;

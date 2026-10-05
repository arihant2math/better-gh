-- Core schema: repositories and repository-level access/settings.

CREATE TABLE repositories (
    id                          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_id                    BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name                        TEXT NOT NULL,
    description                 TEXT,
    homepage                    TEXT,
    visibility                  TEXT NOT NULL DEFAULT 'public'
                                CHECK (visibility IN ('public', 'private', 'internal')),
    fork                        BOOLEAN NOT NULL DEFAULT false,
    -- Direct parent of a fork / root of the fork network.
    parent_id                   BIGINT REFERENCES repositories (id) ON DELETE SET NULL,
    source_id                   BIGINT REFERENCES repositories (id) ON DELETE SET NULL,
    template_repository_id      BIGINT REFERENCES repositories (id) ON DELETE SET NULL,
    default_branch              TEXT NOT NULL DEFAULT 'main',
    archived                    BOOLEAN NOT NULL DEFAULT false,
    disabled                    BOOLEAN NOT NULL DEFAULT false,
    is_template                 BOOLEAN NOT NULL DEFAULT false,
    allow_forking               BOOLEAN NOT NULL DEFAULT true,
    has_issues                  BOOLEAN NOT NULL DEFAULT true,
    has_projects                BOOLEAN NOT NULL DEFAULT true,
    has_wiki                    BOOLEAN NOT NULL DEFAULT true,
    has_discussions             BOOLEAN NOT NULL DEFAULT false,
    has_pages                   BOOLEAN NOT NULL DEFAULT false,
    allow_merge_commit          BOOLEAN NOT NULL DEFAULT true,
    allow_squash_merge          BOOLEAN NOT NULL DEFAULT true,
    allow_rebase_merge          BOOLEAN NOT NULL DEFAULT true,
    allow_auto_merge            BOOLEAN NOT NULL DEFAULT false,
    allow_update_branch         BOOLEAN NOT NULL DEFAULT false,
    delete_branch_on_merge      BOOLEAN NOT NULL DEFAULT false,
    use_squash_pr_title_as_default BOOLEAN NOT NULL DEFAULT false,
    squash_merge_commit_title   TEXT NOT NULL DEFAULT 'COMMIT_OR_PR_TITLE'
                                CHECK (squash_merge_commit_title IN ('PR_TITLE', 'COMMIT_OR_PR_TITLE')),
    squash_merge_commit_message TEXT NOT NULL DEFAULT 'COMMIT_MESSAGES'
                                CHECK (squash_merge_commit_message IN ('PR_BODY', 'COMMIT_MESSAGES', 'BLANK')),
    merge_commit_title          TEXT NOT NULL DEFAULT 'MERGE_MESSAGE'
                                CHECK (merge_commit_title IN ('PR_TITLE', 'MERGE_MESSAGE')),
    merge_commit_message        TEXT NOT NULL DEFAULT 'PR_TITLE'
                                CHECK (merge_commit_message IN ('PR_BODY', 'PR_TITLE', 'BLANK')),
    web_commit_signoff_required BOOLEAN NOT NULL DEFAULT false,
    topics                      TEXT[] NOT NULL DEFAULT '{}',
    language                    TEXT,
    license_spdx_id             TEXT,
    -- Next number handed out to an issue or pull request (shared sequence).
    next_issue_number           BIGINT NOT NULL DEFAULT 1,
    next_milestone_number       BIGINT NOT NULL DEFAULT 1,
    -- Repository size in KB (updated after pushes).
    size                        BIGINT NOT NULL DEFAULT 0,
    -- Denormalized counters, maintained by the owning domain code.
    stargazers_count            BIGINT NOT NULL DEFAULT 0,
    watchers_count              BIGINT NOT NULL DEFAULT 0,
    forks_count                 BIGINT NOT NULL DEFAULT 0,
    open_issues_count           BIGINT NOT NULL DEFAULT 0,
    pushed_at                   TIMESTAMPTZ,
    created_at                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX repositories_owner_name_key ON repositories (owner_id, lower(name));
CREATE INDEX repositories_parent_idx ON repositories (parent_id);
CREATE INDEX repositories_source_idx ON repositories (source_id);
CREATE INDEX repositories_visibility_idx ON repositories (visibility, id);
CREATE INDEX repositories_topics_idx ON repositories USING GIN (topics);
CREATE INDEX repositories_name_trgm_idx ON repositories (lower(name) text_pattern_ops);

CREATE TABLE collaborators (
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    permission  TEXT NOT NULL CHECK (permission IN ('read', 'triage', 'write', 'maintain', 'admin')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, user_id)
);
CREATE INDEX collaborators_user_idx ON collaborators (user_id);

CREATE TABLE repo_invitations (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    invitee_id  BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    inviter_id  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    permission  TEXT NOT NULL CHECK (permission IN ('read', 'triage', 'write', 'maintain', 'admin')),
    expired     BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, invitee_id)
);
CREATE INDEX repo_invitations_invitee_idx ON repo_invitations (invitee_id);

CREATE TABLE team_repos (
    team_id     BIGINT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    permission  TEXT NOT NULL CHECK (permission IN ('read', 'triage', 'write', 'maintain', 'admin')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (team_id, repo_id)
);
CREATE INDEX team_repos_repo_idx ON team_repos (repo_id);

CREATE TABLE stars (
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, repo_id)
);
CREATE INDEX stars_repo_idx ON stars (repo_id, created_at);

-- Repository subscriptions ("watching").
CREATE TABLE watches (
    user_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    subscribed  BOOLEAN NOT NULL DEFAULT true,
    ignored     BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, repo_id)
);
CREATE INDEX watches_repo_idx ON watches (repo_id) WHERE subscribed;

CREATE TABLE deploy_keys (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id       BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    title         TEXT NOT NULL DEFAULT '',
    key           TEXT NOT NULL,
    fingerprint   TEXT NOT NULL,
    read_only     BOOLEAN NOT NULL DEFAULT true,
    verified      BOOLEAN NOT NULL DEFAULT true,
    added_by_id   BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at  TIMESTAMPTZ,
    UNIQUE (repo_id, fingerprint)
);
CREATE INDEX deploy_keys_fingerprint_idx ON deploy_keys (fingerprint);

-- Branch protection rules; `pattern` is an exact branch name or fnmatch glob.
CREATE TABLE branch_protections (
    id                               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id                          BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    pattern                          TEXT NOT NULL,
    -- {"strict": bool, "contexts": [..], "checks": [{"context", "app_id"}]} or NULL
    required_status_checks           JSONB,
    -- {"dismiss_stale_reviews", "require_code_owner_reviews",
    --  "required_approving_review_count", "require_last_push_approval",
    --  "dismissal_restrictions", "bypass_pull_request_allowances"} or NULL
    required_pull_request_reviews    JSONB,
    -- {"users": [ids], "teams": [ids], "apps": [ids]} or NULL (no push restrictions)
    restrictions                     JSONB,
    enforce_admins                   BOOLEAN NOT NULL DEFAULT false,
    required_linear_history          BOOLEAN NOT NULL DEFAULT false,
    allow_force_pushes               BOOLEAN NOT NULL DEFAULT false,
    allow_deletions                  BOOLEAN NOT NULL DEFAULT false,
    block_creations                  BOOLEAN NOT NULL DEFAULT false,
    required_conversation_resolution BOOLEAN NOT NULL DEFAULT false,
    required_signatures              BOOLEAN NOT NULL DEFAULT false,
    lock_branch                      BOOLEAN NOT NULL DEFAULT false,
    allow_fork_syncing               BOOLEAN NOT NULL DEFAULT false,
    created_at                       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, pattern)
);

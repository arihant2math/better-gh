-- P39: merge queue. One row per time a PR is added to the queue of its
-- base branch; active states are `queued` (waiting for a merge group),
-- `awaiting_checks` (in a merge group whose checks run) and `mergeable`
-- (its group passed). `merged`, `unmergeable` and `removed` are final.
CREATE TABLE merge_queue_entries (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id         BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    pull_id         BIGINT NOT NULL REFERENCES pull_requests (issue_id) ON DELETE CASCADE,
    base_ref        TEXT NOT NULL,
    head_sha        TEXT NOT NULL,
    enqueuer_id     BIGINT REFERENCES users (id) ON DELETE SET NULL,
    state           TEXT NOT NULL DEFAULT 'queued' CHECK (state IN
                        ('queued', 'awaiting_checks', 'mergeable', 'unmergeable', 'merged', 'removed')),
    jump            BOOLEAN NOT NULL DEFAULT false,
    -- The merge group currently building this entry (P39.2).
    group_id        BIGINT,
    failure_reason  TEXT,
    enqueued_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- A PR is queued at most once at a time.
CREATE UNIQUE INDEX merge_queue_entries_active_pull ON merge_queue_entries (pull_id)
    WHERE state IN ('queued', 'awaiting_checks', 'mergeable');
CREATE INDEX merge_queue_entries_queue ON merge_queue_entries (repo_id, base_ref, state);

-- A merge group: the base tip plus a prefix of the queue, built as a
-- temporary branch (`head_ref`, e.g. `gh-readonly-queue/main/pr-1-<sha>`)
-- whose checks decide whether its entries merge.
CREATE TABLE merge_groups (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id              BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    base_ref             TEXT NOT NULL,
    base_sha             TEXT NOT NULL,
    head_ref             TEXT NOT NULL,
    head_sha             TEXT NOT NULL,
    state                TEXT NOT NULL DEFAULT 'checking' CHECK (state IN
                             ('checking', 'success', 'failure', 'merged', 'destroyed')),
    entry_ids            BIGINT[] NOT NULL DEFAULT '{}',
    checks_requested_at  TIMESTAMPTZ,
    deadline_at          TIMESTAMPTZ,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX merge_groups_queue ON merge_groups (repo_id, base_ref, state);

ALTER TABLE merge_queue_entries
    ADD CONSTRAINT merge_queue_entries_group_fk
    FOREIGN KEY (group_id) REFERENCES merge_groups (id) ON DELETE SET NULL;

-- P31 Repository Insights: cached commit statistics, traffic counters and
-- the push / ref-change activity log.

-- Commit statistics of the default branch (`/stats/*`), computed by the
-- `repos.compute_stats` job and valid while `commit_sha` is the head.
CREATE TABLE repo_stats (
    repo_id       BIGINT PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    commit_sha    TEXT NOT NULL,
    commit_count  BIGINT NOT NULL,
    -- {authors: [{email, name, weeks: [[w, a, d, c]]}], weeks: [[w, a, d, c]],
    --  days: [[day, c]], punch: [[d, h, c]]} (sparse; see bgh-repos insights).
    data          JSONB NOT NULL,
    computed_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Page views reported by the web client beacon, aggregated per day,
-- visitor (`u:<id>` or a salted IP hash), path and referrer host.
CREATE TABLE repo_traffic_views (
    repo_id   BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    day       DATE NOT NULL,
    visitor   TEXT NOT NULL,
    path      TEXT NOT NULL,
    referrer  TEXT NOT NULL DEFAULT '',
    title     TEXT NOT NULL DEFAULT '',
    count     INT NOT NULL DEFAULT 1,
    PRIMARY KEY (repo_id, day, visitor, path, referrer)
);
CREATE INDEX repo_traffic_views_day_idx ON repo_traffic_views (day);

-- Clones (upload-pack negotiations without `have` lines), per day and visitor.
CREATE TABLE repo_traffic_clones (
    repo_id  BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    day      DATE NOT NULL,
    visitor  TEXT NOT NULL,
    count    INT NOT NULL DEFAULT 1,
    PRIMARY KEY (repo_id, day, visitor)
);
CREATE INDEX repo_traffic_clones_day_idx ON repo_traffic_clones (day);

-- Branch pushes and ref changes (`GET /repos/{o}/{r}/activity`).
CREATE TABLE repo_activity (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id        BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    ref            TEXT NOT NULL,
    before         TEXT NOT NULL,
    after          TEXT NOT NULL,
    activity_type  TEXT NOT NULL CHECK (activity_type IN (
                       'push', 'force_push', 'branch_creation', 'branch_deletion',
                       'pr_merge', 'merge_queue_merge')),
    actor_id       BIGINT REFERENCES users (id) ON DELETE SET NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX repo_activity_repo_idx ON repo_activity (repo_id, id DESC);
CREATE INDEX repo_activity_actor_idx ON repo_activity (actor_id);

-- Pulls package (B4): check annotations, check run actions, review
-- invariants and indexes for PR queries.

-- One pending review per user and pull request (GitHub rejects a second).
CREATE UNIQUE INDEX pr_reviews_one_pending_key
    ON pr_reviews (pull_id, user_id) WHERE state = 'PENDING';

-- Dismissal metadata (GitHub keeps the dismissed review's body/state).
ALTER TABLE pr_reviews ADD COLUMN dismissed_at TIMESTAMPTZ;
ALTER TABLE pr_reviews ADD COLUMN dismissal_message TEXT;

-- Review comments: thread lookups and outdated tracking.
CREATE INDEX pr_review_comments_reply_idx
    ON pr_review_comments (in_reply_to_id) WHERE in_reply_to_id IS NOT NULL;

-- Requested reviewers: remember CODEOWNERS-driven requests.
ALTER TABLE pr_requested_reviewers ADD COLUMN as_code_owner BOOLEAN NOT NULL DEFAULT false;
CREATE INDEX pr_requested_reviewers_pull_idx ON pr_requested_reviewers (pull_id, id);
CREATE INDEX pr_requested_reviewers_user_idx ON pr_requested_reviewers (user_id)
    WHERE user_id IS NOT NULL;

-- PRs associated with a commit (`GET /commits/{sha}/pulls`).
CREATE INDEX pull_requests_merge_commit_idx
    ON pull_requests (merge_commit_sha) WHERE merge_commit_sha IS NOT NULL;
-- Auto-merge candidates.
CREATE INDEX pull_requests_auto_merge_idx
    ON pull_requests (repo_id) WHERE auto_merge IS NOT NULL;

-- Check runs: requested actions and the app that owns the run.
ALTER TABLE check_runs ADD COLUMN actions JSONB NOT NULL DEFAULT '[]';
ALTER TABLE check_runs ADD COLUMN creator_id BIGINT REFERENCES users (id) ON DELETE SET NULL;
CREATE INDEX check_runs_repo_name_idx ON check_runs (repo_id, name, id DESC);

ALTER TABLE check_suites ADD COLUMN rerequestable BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE check_suites ADD COLUMN latest_check_runs_count BIGINT NOT NULL DEFAULT 0;
CREATE INDEX check_suites_app_sha_idx ON check_suites (repo_id, head_sha, app_slug);

CREATE TABLE check_run_annotations (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    check_run_id      BIGINT NOT NULL REFERENCES check_runs (id) ON DELETE CASCADE,
    path              TEXT NOT NULL,
    start_line        INTEGER NOT NULL,
    end_line          INTEGER NOT NULL,
    start_column      INTEGER,
    end_column        INTEGER,
    annotation_level  TEXT NOT NULL CHECK (annotation_level IN ('notice', 'warning', 'failure')),
    title             TEXT,
    message           TEXT NOT NULL,
    raw_details       TEXT,
    blob_href         TEXT NOT NULL DEFAULT '',
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX check_run_annotations_run_idx ON check_run_annotations (check_run_id, id);

-- Commit status lookups by creator are rare; the (repo_id, sha, context)
-- index from 0004 covers combined status.

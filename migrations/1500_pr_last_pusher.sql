-- P3: who pushed the current head of a pull request (for
-- `require_last_push_approval`). NULL = the PR author (pushed before
-- opening the PR).
ALTER TABLE pull_requests
    ADD COLUMN last_pusher_id BIGINT REFERENCES users (id) ON DELETE SET NULL;

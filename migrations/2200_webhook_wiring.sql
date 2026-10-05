-- P10 webhook wiring: `ci_activity` for Checks-API suites looks up who
-- pushed a commit (latest PushEvent whose head it is).
CREATE INDEX activity_events_push_head_idx
    ON activity_events (repo_id, (payload->>'head')) WHERE type = 'PushEvent';

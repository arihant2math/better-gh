-- P23: organization rulesets, push rulesets, legacy tag protections and
-- rule suites (recorded evaluations of pushes and merges).

-- Organization rulesets share the table (and id sequence) of repository
-- rulesets: exactly one of repo_id / org_id is set.
ALTER TABLE repo_rulesets ALTER COLUMN repo_id DROP NOT NULL;
ALTER TABLE repo_rulesets ADD COLUMN org_id BIGINT REFERENCES users (id) ON DELETE CASCADE;
ALTER TABLE repo_rulesets ADD CONSTRAINT repo_rulesets_source_check
    CHECK ((repo_id IS NULL) <> (org_id IS NULL));
-- Rulesets created through the legacy /tags/protection endpoints.
ALTER TABLE repo_rulesets ADD COLUMN tag_protection BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE repo_rulesets DROP CONSTRAINT repo_rulesets_target_check;
ALTER TABLE repo_rulesets ADD CONSTRAINT repo_rulesets_target_check
    CHECK (target IN ('branch', 'tag', 'push'));
CREATE UNIQUE INDEX org_rulesets_name_key ON repo_rulesets (org_id, lower(name))
    WHERE org_id IS NOT NULL;

-- One row per evaluated ref update (push) or pull request merge.
CREATE TABLE rule_suites (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id            BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    actor_id           BIGINT REFERENCES users (id) ON DELETE SET NULL,
    actor_name         TEXT,
    ref                TEXT NOT NULL,
    before_sha         TEXT NOT NULL,
    after_sha          TEXT NOT NULL,
    -- active rules only: pass | fail | bypass
    result             TEXT NOT NULL CHECK (result IN ('pass', 'fail', 'bypass')),
    -- active and evaluate rules
    evaluation_result  TEXT CHECK (evaluation_result IN ('pass', 'fail', 'bypass')),
    -- [{"rule_source": {...}, "enforcement", "result", "rule_type", "details"}]
    rule_evaluations   JSONB NOT NULL DEFAULT '[]',
    pushed_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX rule_suites_repo_idx ON rule_suites (repo_id, pushed_at DESC, id DESC);

-- P42: comment moderation (minimize/hide), issue deletion and edit history.

-- Minimized ("hidden") comments. `minimized_reason` uses GitHub's
-- GraphQL `minimizedReason` spelling (lowercase, `off-topic`).
ALTER TABLE comments
    ADD COLUMN minimized_reason TEXT CHECK (minimized_reason IN (
        'spam', 'abuse', 'off-topic', 'outdated', 'duplicate', 'resolved')),
    ADD COLUMN minimized_by_id BIGINT REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN minimized_at TIMESTAMPTZ;
ALTER TABLE pr_review_comments
    ADD COLUMN minimized_reason TEXT CHECK (minimized_reason IN (
        'spam', 'abuse', 'off-topic', 'outdated', 'duplicate', 'resolved')),
    ADD COLUMN minimized_by_id BIGINT REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN minimized_at TIMESTAMPTZ;
ALTER TABLE pr_reviews
    ADD COLUMN minimized_reason TEXT CHECK (minimized_reason IN (
        'spam', 'abuse', 'off-topic', 'outdated', 'duplicate', 'resolved')),
    ADD COLUMN minimized_by_id BIGINT REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN minimized_at TIMESTAMPTZ;
ALTER TABLE commit_comments
    ADD COLUMN minimized_reason TEXT CHECK (minimized_reason IN (
        'spam', 'abuse', 'off-topic', 'outdated', 'duplicate', 'resolved')),
    ADD COLUMN minimized_by_id BIGINT REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN minimized_at TIMESTAMPTZ;
CREATE INDEX comments_minimized_by_idx ON comments (minimized_by_id) WHERE minimized_by_id IS NOT NULL;
CREATE INDEX pr_review_comments_minimized_by_idx ON pr_review_comments (minimized_by_id) WHERE minimized_by_id IS NOT NULL;
CREATE INDEX pr_reviews_minimized_by_idx ON pr_reviews (minimized_by_id) WHERE minimized_by_id IS NOT NULL;
CREATE INDEX commit_comments_minimized_by_idx ON commit_comments (minimized_by_id) WHERE minimized_by_id IS NOT NULL;

-- Edit history of issue/PR bodies and every comment kind (GraphQL
-- `userContentEdits`). One row per edit, with the text before and after;
-- a deleted revision keeps its row with the texts cleared.
CREATE TABLE user_content_edits (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    repo_id        BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    target_type    TEXT NOT NULL CHECK (target_type IN (
                       'issue', 'comment', 'review', 'review_comment', 'commit_comment')),
    target_id      BIGINT NOT NULL,
    editor_id      BIGINT REFERENCES users (id) ON DELETE SET NULL,
    body           TEXT,
    previous_body  TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at     TIMESTAMPTZ,
    deleted_by_id  BIGINT REFERENCES users (id) ON DELETE SET NULL
);
CREATE INDEX user_content_edits_target_idx ON user_content_edits (target_type, target_id, id);
CREATE INDEX user_content_edits_repo_idx ON user_content_edits (repo_id);
CREATE INDEX user_content_edits_editor_idx ON user_content_edits (editor_id);
CREATE INDEX user_content_edits_deleted_by_idx ON user_content_edits (deleted_by_id)
    WHERE deleted_by_id IS NOT NULL;

-- The polymorphic target has no FK: drop a target's history with it
-- (also when it goes by cascade, e.g. comments of a deleted issue).
CREATE FUNCTION bgh_drop_content_edits() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM user_content_edits WHERE target_type = TG_ARGV[0] AND target_id = OLD.id;
    RETURN OLD;
END $$;
CREATE TRIGGER issues_drop_content_edits AFTER DELETE ON issues
    FOR EACH ROW EXECUTE FUNCTION bgh_drop_content_edits('issue');
CREATE TRIGGER comments_drop_content_edits AFTER DELETE ON comments
    FOR EACH ROW EXECUTE FUNCTION bgh_drop_content_edits('comment');
CREATE TRIGGER pr_reviews_drop_content_edits AFTER DELETE ON pr_reviews
    FOR EACH ROW EXECUTE FUNCTION bgh_drop_content_edits('review');
CREATE TRIGGER pr_review_comments_drop_content_edits AFTER DELETE ON pr_review_comments
    FOR EACH ROW EXECUTE FUNCTION bgh_drop_content_edits('review_comment');
CREATE TRIGGER commit_comments_drop_content_edits AFTER DELETE ON commit_comments
    FOR EACH ROW EXECUTE FUNCTION bgh_drop_content_edits('commit_comment');

-- Deleted issues: the number stays reserved (`next_issue_number` never
-- goes back) and `GET` answers 410 Gone instead of 404.
CREATE TABLE deleted_issues (
    repo_id        BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    number         BIGINT NOT NULL,
    deleted_by_id  BIGINT REFERENCES users (id) ON DELETE SET NULL,
    deleted_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, number)
);
CREATE INDEX deleted_issues_deleted_by_idx ON deleted_issues (deleted_by_id);

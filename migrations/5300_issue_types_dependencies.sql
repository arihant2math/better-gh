-- P41: issue types, issue dependencies (blocked by / blocking), close as duplicate.

-- Organization-defined issue types (GitHub `issue-type`).
CREATE TABLE issue_types (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    org_id      BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    description TEXT,
    color       TEXT CHECK (color IN ('gray', 'blue', 'green', 'yellow', 'orange', 'red', 'pink', 'purple')),
    is_enabled  BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX issue_types_org_name_key ON issue_types (org_id, lower(name));

-- GitHub's defaults for every organization (existing and new).
CREATE FUNCTION bgh_seed_issue_types(org BIGINT) RETURNS void LANGUAGE sql AS $$
    INSERT INTO issue_types (org_id, name, description, color)
    VALUES (org, 'Task', 'A specific piece of work', 'yellow'),
           (org, 'Bug', 'An unexpected problem or behavior', 'red'),
           (org, 'Feature', 'A request, idea, or new functionality', 'blue')
    ON CONFLICT DO NOTHING
$$;

CREATE FUNCTION bgh_seed_issue_types_trigger() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.type = 'Organization' THEN
        PERFORM bgh_seed_issue_types(NEW.id);
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER users_seed_issue_types AFTER INSERT ON users
    FOR EACH ROW EXECUTE FUNCTION bgh_seed_issue_types_trigger();

SELECT bgh_seed_issue_types(id) FROM users WHERE type = 'Organization';

ALTER TABLE issues
    ADD COLUMN issue_type_id BIGINT REFERENCES issue_types(id) ON DELETE SET NULL,
    ADD COLUMN duplicate_of_id BIGINT REFERENCES issues(id) ON DELETE SET NULL;
CREATE INDEX issues_issue_type_idx ON issues (issue_type_id) WHERE issue_type_id IS NOT NULL;
CREATE INDEX issues_duplicate_of_idx ON issues (duplicate_of_id) WHERE duplicate_of_id IS NOT NULL;

-- `blocked_id` is blocked by `blocking_id` (may be in another repository).
CREATE TABLE issue_dependencies (
    blocked_id  BIGINT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    blocking_id BIGINT NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (blocked_id, blocking_id),
    CHECK (blocked_id <> blocking_id)
);
CREATE INDEX issue_dependencies_blocking_idx ON issue_dependencies (blocking_id, blocked_id);

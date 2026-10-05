-- P13 (Projects v2 public API).

-- When an item was archived (REST `archived_at`).
ALTER TABLE project_items ADD COLUMN archived_at TIMESTAMPTZ;
UPDATE project_items SET archived_at = updated_at WHERE archived;

-- P13: teams linked to a project (`linkProjectV2ToTeam`).
CREATE TABLE project_linked_teams (
    project_id  BIGINT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    team_id     BIGINT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, team_id)
);
CREATE INDEX project_linked_teams_team_idx ON project_linked_teams (team_id);

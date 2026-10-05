-- Activity events (GitHub Events API): one row per public-timeline event,
-- recorded from domain events with a payload snapshot.

CREATE TABLE activity_events (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- GitHub event type: PushEvent, IssuesEvent, WatchEvent, ...
    type        TEXT NOT NULL,
    actor_id    BIGINT REFERENCES users (id) ON DELETE CASCADE,
    repo_id     BIGINT NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    -- `owner/name` at the time of the event.
    repo_name   TEXT NOT NULL,
    -- Owning organization, if any.
    org_id      BIGINT REFERENCES users (id) ON DELETE CASCADE,
    -- Repository was public when the event happened.
    public      BOOLEAN NOT NULL,
    payload     JSONB NOT NULL DEFAULT '{}',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX activity_events_actor_idx ON activity_events (actor_id, id DESC);
CREATE INDEX activity_events_repo_idx ON activity_events (repo_id, id DESC);
CREATE INDEX activity_events_org_idx ON activity_events (org_id, id DESC) WHERE org_id IS NOT NULL;
CREATE INDEX activity_events_public_idx ON activity_events (id DESC) WHERE public;
CREATE INDEX activity_events_created_idx ON activity_events (created_at);

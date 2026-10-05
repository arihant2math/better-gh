-- bgh-notify: notification threads, per-user settings, webhook deliveries.

-- Notification threads: what last bumped the thread (for subject.latest_comment_url)
-- and a non-numeric subject key (commit SHA, check suite head SHA).
ALTER TABLE notifications
    ADD COLUMN latest_comment_type TEXT,
    ADD COLUMN subject_key TEXT,
    ADD COLUMN last_actor_id BIGINT REFERENCES users (id) ON DELETE SET NULL;
CREATE INDEX notifications_user_unread_idx
    ON notifications (user_id, updated_at DESC, id DESC) WHERE unread AND NOT done;
CREATE INDEX notifications_subject_idx ON notifications (subject_type, subject_id);
CREATE INDEX notifications_last_actor_idx ON notifications (last_actor_id)
    WHERE last_actor_id IS NOT NULL;

CREATE INDEX thread_subscriptions_repo_idx ON thread_subscriptions (repo_id);

-- Users ignoring a repository (watches.ignored) are looked up per fan-out.
CREATE INDEX watches_repo_ignored_idx ON watches (repo_id) WHERE ignored;

-- Per-user notification delivery settings. Reasons listed in *_disabled
-- are not delivered on that channel (GitHub reasons: assign, author,
-- comment, mention, team_mention, review_requested, state_change,
-- subscribed, manual, ci_activity, security_alert, invitation).
CREATE TABLE notification_settings (
    user_id               BIGINT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    web_disabled          TEXT[] NOT NULL DEFAULT '{}',
    email_disabled        TEXT[] NOT NULL DEFAULT '{}',
    -- Master switch for notification emails.
    email_enabled         BOOLEAN NOT NULL DEFAULT true,
    -- Override address (must be one of the user's verified emails); NULL = primary.
    notification_email    TEXT,
    -- Also email about your own activity (GitHub "Include your own updates").
    own_activity_email    BOOLEAN NOT NULL DEFAULT false,
    updated_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Webhooks: who created them (audit / UI).
ALTER TABLE webhooks
    ADD COLUMN creator_id BIGINT REFERENCES users (id) ON DELETE SET NULL;

-- Deliveries: exact request body (signature is over these bytes), target
-- URL snapshot, retry bookkeeping.
ALTER TABLE webhook_deliveries
    ADD COLUMN url TEXT NOT NULL DEFAULT '',
    ADD COLUMN payload_raw TEXT NOT NULL DEFAULT '',
    ADD COLUMN content_type TEXT NOT NULL DEFAULT 'json',
    ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN error TEXT,
    ADD COLUMN throttled_at TIMESTAMPTZ;
CREATE INDEX webhook_deliveries_guid_idx ON webhook_deliveries (guid);

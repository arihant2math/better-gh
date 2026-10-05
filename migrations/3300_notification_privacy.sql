-- P21: notification privacy, retention and polling.

-- Whether user `uid` can read repository `rid` (raw permissions, no token
-- scopes): public, owner, site admin, direct collaborator, org admin / org
-- member with a base permission, or a team grant (child teams inherit their
-- parents' grants). The SQL twin of `bgh_core::perms::repo_permissions`
-- (>= read), for queries over many (user, repository) pairs: the
-- notification sync shape, retitles and access-loss pruning.
CREATE OR REPLACE FUNCTION bgh_can_read_repo(uid BIGINT, rid BIGINT) RETURNS BOOLEAN
    LANGUAGE sql STABLE PARALLEL SAFE
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM repositories r
         WHERE r.id = rid
           AND (r.visibility = 'public'
                OR r.owner_id = uid
                OR EXISTS (SELECT 1 FROM users u WHERE u.id = uid AND u.site_admin)
                OR EXISTS (SELECT 1 FROM collaborators c
                            WHERE c.repo_id = r.id AND c.user_id = uid)
                OR EXISTS (SELECT 1 FROM org_members m
                             LEFT JOIN org_settings s ON s.org_id = m.org_id
                            WHERE m.org_id = r.owner_id AND m.user_id = uid
                              AND (m.role = 'admin'
                                   OR s.default_repository_permission <> 'none'))
                OR EXISTS (
                    WITH RECURSIVE ut AS (
                        SELECT t.id, t.parent_id
                          FROM team_members tm JOIN teams t ON t.id = tm.team_id
                         WHERE tm.user_id = uid AND t.org_id = r.owner_id
                        UNION
                        SELECT p.id, p.parent_id FROM teams p JOIN ut ON p.id = ut.parent_id
                    )
                    SELECT 1 FROM team_repos tr
                     WHERE tr.repo_id = r.id AND tr.team_id IN (SELECT id FROM ut)))
    )
    $$;

-- Last change of a thread (any column: read state, done, title, bump), the
-- basis of `Last-Modified` / `If-Modified-Since` on `/notifications`.
ALTER TABLE notifications ADD COLUMN changed_at TIMESTAMPTZ NOT NULL DEFAULT now();

CREATE OR REPLACE FUNCTION bgh_notifications_touch() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    NEW.changed_at := now();
    RETURN NEW;
END $$;

CREATE TRIGGER notifications_touch BEFORE UPDATE ON notifications
    FOR EACH ROW EXECUTE FUNCTION bgh_notifications_touch();

CREATE INDEX notifications_user_changed_idx ON notifications (user_id, changed_at DESC);
-- Access-loss pruning by repository.
CREATE INDEX notifications_repo_idx ON notifications (repo_id, user_id);
-- Retention (oldest first).
CREATE INDEX notifications_updated_idx ON notifications (updated_at);
-- Retention: deliveries whose payload hasn't been stripped yet.
CREATE INDEX webhook_deliveries_payload_created_idx ON webhook_deliveries (created_at)
    WHERE payload_raw <> '';

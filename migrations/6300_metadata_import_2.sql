-- P51: metadata importer, part 2 (pull requests, reviews, wiki, GitLab
-- source, mannequin reclaim). Range 6300-6399.

-- GitLab (REST API v4) as an import source.
ALTER TABLE imports DROP CONSTRAINT IF EXISTS imports_kind_check;
ALTER TABLE imports ADD CONSTRAINT imports_kind_check CHECK (kind IN ('github', 'gitlab'));

-- A mannequin whose attribution moved to a real account (kept so old
-- links and the reclaim history resolve; it owns nothing afterwards).
ALTER TABLE users
    ADD COLUMN mannequin_reclaimed_by BIGINT REFERENCES users (id) ON DELETE SET NULL;

-- Reclaim invitations: an organization owner (or a site admin) proposes a
-- real account for a mannequin; the invitee accepts and every row
-- attributed to the mannequin moves to them.
CREATE TABLE mannequin_reclaims (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    mannequin_id  BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- The organization whose owner asked (NULL: a site administrator).
    org_id        BIGINT REFERENCES users (id) ON DELETE CASCADE,
    target_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    invited_by    BIGINT REFERENCES users (id) ON DELETE SET NULL,
    status        TEXT NOT NULL DEFAULT 'pending'
                  CHECK (status IN ('pending', 'accepted', 'declined', 'cancelled')),
    -- {"table.column": rows moved, ...} after acceptance.
    moved         JSONB NOT NULL DEFAULT '{}',
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at  TIMESTAMPTZ
);

CREATE UNIQUE INDEX mannequin_reclaims_pending_key
    ON mannequin_reclaims (mannequin_id) WHERE status = 'pending';
CREATE INDEX mannequin_reclaims_target_idx
    ON mannequin_reclaims (target_id, id DESC);
CREATE INDEX mannequin_reclaims_org_idx ON mannequin_reclaims (org_id, id DESC);
CREATE INDEX mannequin_reclaims_mannequin_idx ON mannequin_reclaims (mannequin_id, id DESC);

-- Mannequins of an organization's imports (user mappings by local id).
CREATE INDEX import_mappings_user_local_idx
    ON import_mappings (local_id) WHERE source_type = 'user';

-- Single-row marker: repositories were deleted since the last orphan-blob
-- GC. The next `search.index_repo` run collects orphaned `code_blobs`.
CREATE TABLE code_index_gc (
    id       BOOLEAN PRIMARY KEY DEFAULT true CHECK (id),
    pending  BOOLEAN NOT NULL DEFAULT false
);
INSERT INTO code_index_gc (id, pending) VALUES (true, false);

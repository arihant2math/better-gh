-- P39.2: the merge group ref built for each queue entry
-- (`refs/heads/gh-readonly-queue/{base}/pr-{n}-{head_sha}`) and the
-- commit it was built as. The queue merges exactly that commit (never
-- whatever the ref points at later), so it is kept here.
ALTER TABLE merge_queue_entries
    ADD COLUMN group_ref TEXT,
    ADD COLUMN group_sha TEXT;

CREATE INDEX merge_queue_entries_group_sha ON merge_queue_entries (repo_id, group_sha)
    WHERE group_sha IS NOT NULL;

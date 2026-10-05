-- P33: license detection. `license_blob_sha` is the blob of the detected
-- license file on the default branch ('' = scanned, no license file;
-- NULL = never scanned, picked up by the repos.license_backfill service).
ALTER TABLE repositories ADD COLUMN license_blob_sha TEXT;
CREATE INDEX repositories_license_unscanned_idx ON repositories (id)
    WHERE license_blob_sha IS NULL AND pushed_at IS NOT NULL;

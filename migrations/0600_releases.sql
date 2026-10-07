-- Releases: indexes for list/latest/tag lookups and asset storage GC.

-- `GET /releases/tags/{tag}` and tag uniqueness checks (drafts included).
CREATE INDEX releases_repo_tag_idx ON releases (repo_id, tag_name);
-- `GET /releases/latest`: newest published, non-prerelease release.
CREATE INDEX releases_repo_latest_idx ON releases (repo_id, created_at DESC, id DESC)
    WHERE NOT draft AND NOT prerelease;
CREATE INDEX releases_author_idx ON releases (author_id);

-- Assets are content-addressed by sha256 in the storage backend; this
-- index finds other references before a blob is deleted.
CREATE INDEX release_assets_sha_idx ON release_assets (sha256) WHERE sha256 IS NOT NULL;
CREATE INDEX release_assets_repo_idx ON release_assets (repo_id);
CREATE INDEX release_assets_uploader_idx ON release_assets (uploader_id);

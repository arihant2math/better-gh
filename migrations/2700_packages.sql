-- P15: container registry (OCI distribution) and GitHub Packages.

-- A package: `{owner}/{name}` in the registry, `packages/{type}/{name}` in
-- the REST API. Soft-deleted packages (`deleted_at`) can be restored for 30
-- days, then the GC purges them.
CREATE TABLE packages (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    package_type TEXT NOT NULL DEFAULT 'container'
        CHECK (package_type IN ('npm', 'maven', 'rubygems', 'docker', 'nuget', 'container')),
    visibility TEXT NOT NULL DEFAULT 'private'
        CHECK (visibility IN ('public', 'private', 'internal')),
    -- Linked repository: access is inherited from it.
    repo_id BIGINT REFERENCES repositories(id) ON DELETE SET NULL,
    -- Publisher of the first content (admin of an unlinked package).
    created_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
    -- Bytes: distinct linked blobs plus manifests (quota accounting).
    size BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX packages_owner_type_name_key
    ON packages (owner_id, package_type, lower(name)) WHERE deleted_at IS NULL;
CREATE INDEX packages_owner_idx ON packages (owner_id, updated_at DESC, id);
CREATE INDEX packages_repo_idx ON packages (repo_id) WHERE repo_id IS NOT NULL;
CREATE INDEX packages_deleted_idx ON packages (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX packages_created_by_idx ON packages (created_by);

-- Content-addressed blobs, stored once in {data_dir}/packages/blobs.
CREATE TABLE package_blobs (
    digest TEXT PRIMARY KEY,
    size BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Blobs visible in a package (uploaded or mounted into it).
CREATE TABLE package_blob_links (
    package_id BIGINT NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    digest TEXT NOT NULL REFERENCES package_blobs(digest) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (package_id, digest)
);
CREATE INDEX package_blob_links_digest_idx ON package_blob_links (digest);

-- Manifests (images, indexes, artifacts). One row per digest per package.
CREATE TABLE package_versions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    package_id BIGINT NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    digest TEXT NOT NULL,
    media_type TEXT NOT NULL,
    -- OCI 1.1: `artifactType` (or the config media type) and `subject`.
    artifact_type TEXT,
    subject_digest TEXT,
    annotations JSONB,
    manifest BYTEA NOT NULL,
    -- Bytes: the manifest plus everything it references.
    size BIGINT NOT NULL,
    platforms TEXT[] NOT NULL DEFAULT '{}',
    tags TEXT[] NOT NULL DEFAULT '{}',
    pushed_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ,
    UNIQUE (package_id, digest)
);
CREATE INDEX package_versions_list_idx ON package_versions (package_id, created_at DESC, id DESC);
CREATE INDEX package_versions_tags_idx ON package_versions USING gin (tags);
CREATE INDEX package_versions_subject_idx
    ON package_versions (package_id, subject_digest) WHERE subject_digest IS NOT NULL;
CREATE INDEX package_versions_deleted_idx ON package_versions (deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX package_versions_pushed_by_idx ON package_versions (pushed_by);

-- Blobs a version references (config + layers), for garbage collection.
CREATE TABLE package_version_blobs (
    version_id BIGINT NOT NULL REFERENCES package_versions(id) ON DELETE CASCADE,
    digest TEXT NOT NULL,
    PRIMARY KEY (version_id, digest)
);
CREATE INDEX package_version_blobs_digest_idx ON package_version_blobs (digest);

-- Blob upload sessions (data in {data_dir}/packages/uploads/{id}).
CREATE TABLE package_uploads (
    id UUID PRIMARY KEY,
    package_id BIGINT NOT NULL REFERENCES packages(id) ON DELETE CASCADE,
    user_id BIGINT REFERENCES users(id) ON DELETE CASCADE,
    size BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX package_uploads_package_idx ON package_uploads (package_id);
CREATE INDEX package_uploads_user_idx ON package_uploads (user_id);
CREATE INDEX package_uploads_updated_idx ON package_uploads (updated_at);

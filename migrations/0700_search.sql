-- Search: trigram + full-text indexes for issues, comments, repositories,
-- users, labels; sort indexes for issue search and the command palette.

CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Issues / pull requests (issues.search is the title/body tsvector).
CREATE INDEX issues_title_trgm_idx ON issues USING gin (lower(title) gin_trgm_ops);
CREATE INDEX issues_updated_idx ON issues (updated_at DESC, id DESC);
CREATE INDEX issues_created_idx ON issues (created_at DESC, id DESC);
CREATE INDEX issues_comments_count_idx ON issues (comments_count DESC, id DESC);
CREATE INDEX issues_closed_at_idx ON issues (closed_at) WHERE closed_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS issues_closed_by_idx ON issues (closed_by_id) WHERE closed_by_id IS NOT NULL;

-- `in:comments` and `commenter:`.
CREATE INDEX comments_search_idx ON comments USING gin (to_tsvector('english', body));
CREATE INDEX comments_author_issue_idx ON comments (author_id, issue_id);

-- Repositories: name/description full text, name trigram, sort by stars/forks.
CREATE INDEX repositories_search_idx ON repositories USING gin ((
    setweight(to_tsvector('simple', name), 'A') ||
    setweight(to_tsvector('english', coalesce(description, '')), 'B')));
CREATE INDEX repositories_name_trgm_gin_idx ON repositories USING gin (lower(name) gin_trgm_ops);
CREATE INDEX repositories_stars_idx ON repositories (stargazers_count DESC, id);
CREATE INDEX repositories_forks_idx ON repositories (forks_count DESC, id);
CREATE INDEX repositories_updated_idx ON repositories (updated_at DESC, id);
CREATE INDEX repositories_language_idx ON repositories (lower(language)) WHERE language IS NOT NULL;

-- Users / organizations.
CREATE INDEX users_login_trgm_idx ON users USING gin (lower(login) gin_trgm_ops);
CREATE INDEX users_name_trgm_idx ON users USING gin (lower(coalesce(name, '')) gin_trgm_ops);
CREATE INDEX users_created_idx ON users (created_at, id);

-- Labels.
CREATE INDEX labels_name_trgm_idx ON labels USING gin (lower(name) gin_trgm_ops);

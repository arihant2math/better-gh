-- Expression indexes for the single-field text predicates of issue and
-- repository search (`in:title`, `in:body`, `in:description`). The default
-- title+body search uses `issues.search` (issues_search_idx); these cover the
-- narrower forms, which otherwise scan the whole table. The expressions must
-- match the ones bgh-search emits exactly for the planner to use them.

CREATE INDEX issues_title_tsv_idx ON issues USING gin (to_tsvector('english', title));
CREATE INDEX issues_body_tsv_idx ON issues USING gin (to_tsvector('english', coalesce(body, '')));
CREATE INDEX repositories_description_tsv_idx
    ON repositories USING gin (to_tsvector('english', coalesce(description, '')));

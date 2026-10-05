-- Wikis (bgh-wiki): pages live in a git repository next to the main one
-- (`{data_dir}/repos/{xx}/{id}.wiki.git`); only settings are stored here.
ALTER TABLE repositories
    ADD COLUMN wiki_anyone_can_edit BOOLEAN NOT NULL DEFAULT false;

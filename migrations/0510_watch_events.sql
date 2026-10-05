-- Custom repository watching (GitHub's "Custom"): the event categories a
-- watcher receives besides participating notifications. NULL = all activity.
-- Values: issues, pulls, releases, discussions, security_alerts.
ALTER TABLE watches ADD COLUMN events TEXT[];

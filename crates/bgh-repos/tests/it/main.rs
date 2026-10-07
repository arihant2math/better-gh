//! All integration tests of `bgh-repos`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;
mod gitwork;

mod access_policy;
mod api;
mod bench;
mod branches;
mod browse;
mod collaborators;
mod commit_comments;
mod commits;
mod contents;
mod diff_lines;
mod download;
mod forks;
mod git_transport;
mod gitdb;
mod import;
mod insights;
mod keys;
mod lfs;
mod lifecycle;
mod maintenance;
mod markdown;
mod metadata;
mod org_rulesets;
mod protection;
mod push_hardening;
mod push_rules;
mod settings;
mod signatures;
mod slash_refs;
mod social;
mod ssh;

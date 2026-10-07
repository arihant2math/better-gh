//! All integration tests of `bgh-search`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod access_policy;
mod activity;
mod commit_node_ids;
mod palette;
mod search_code;
mod search_issues;
mod search_other;
mod smoke;

//! All integration tests of `bgh-pulls`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod checks;
mod diffview;
mod governance;
mod merge;
mod merge_queue;
mod merge_queue_service;
mod pulls;
mod required_deployments;
mod rest_compat;
mod review_flow;
mod reviews;
mod signatures;
mod templates;
mod web_client;

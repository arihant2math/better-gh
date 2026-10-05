//! All integration tests of `bgh-pulls`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod checks;
mod governance;
mod merge;
mod pulls;
mod review_flow;
mod reviews;
mod web_client;

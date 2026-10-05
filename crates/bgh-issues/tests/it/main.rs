//! All integration tests of `bgh-issues`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod conversation;
mod extras;
mod issues;
mod labels_milestones;
mod links;
mod smoke;
mod web_client;

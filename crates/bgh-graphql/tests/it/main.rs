//! All integration tests of `bgh-graphql`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod access_policy;
mod branch_protection;
mod cost_limits;
mod merge_queue;
mod moderation;
mod mutations;
mod parse_limits;
mod projects;
mod queries;
mod rulesets;
mod schema;
mod signatures;

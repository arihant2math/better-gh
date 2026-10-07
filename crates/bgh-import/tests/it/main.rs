//! All integration tests of `bgh-import`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below.

mod fake;
mod fake_api;
mod pr_source;

mod github;
mod gitlab;
mod pulls;
mod reclaim;
mod recorded;
mod self_import;

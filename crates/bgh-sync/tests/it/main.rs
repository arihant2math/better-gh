//! All integration tests of `bgh-sync`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod access_policy;
mod bench;
mod bootstrap;
mod commit_bench;
mod compact;
mod middleware;
mod order;
mod shapes;
mod ws;

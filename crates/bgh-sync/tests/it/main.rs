//! All integration tests of `bgh-sync`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod access_policy;
mod bench;
mod bootstrap;
mod compact;
mod middleware;
mod recheck;
mod recheck_bench;
mod shapes;
mod ws;

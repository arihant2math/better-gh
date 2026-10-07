//! All integration tests of `bgh-packages`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in `common` (see docs/BACKEND_PATTERNS.md).

mod common;
mod conformance;
mod renames;
mod rest;
mod tokens;
mod webhooks_gc;

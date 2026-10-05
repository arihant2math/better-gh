//! All integration tests of `bgh-security`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in `common` (see docs/BACKEND_PATTERNS.md).

mod alerts;
mod bench;
mod common;
mod custom_patterns;
mod push_protection;

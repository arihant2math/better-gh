//! All integration tests of `bgh-accounts`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod accounts;
mod auth;
mod boot;
mod oauth;
mod orgs;
mod sso_avatars_ratelimit;
mod teams;
mod users;

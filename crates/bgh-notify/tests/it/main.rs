//! All integration tests of `bgh-notify`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod support;

mod app_hooks;
mod commit_comments;
mod coverage;
mod email;
mod notifications;
mod payloads;
mod privacy;
mod webhooks;
mod wiring;

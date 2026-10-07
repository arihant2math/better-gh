//! All integration tests of `bgh-admin`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod audit_log;
mod ghes_users;
mod jobs_health_maintenance;
mod keys_hooks_stats;
mod manage;
mod settings;

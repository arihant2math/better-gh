//! All integration tests of `bgh-accounts`, linked into one test binary (`it`).
//! Add a new test file as `tests/it/<name>.rs` and declare it below;
//! shared helpers live in the helper modules (see docs/BACKEND_PATTERNS.md).

mod common;

mod accounts;
mod apps;
mod apps_p46;
mod auth;
mod boot;
mod email_verification;
mod fine_grained;
mod invitations;
mod ldap;
mod lifecycle;
mod oauth;
mod orgs;
mod root;
mod saml;
mod scim;
mod security;
mod signing_keys;
mod sso_avatars_ratelimit;
mod teams;
mod users;

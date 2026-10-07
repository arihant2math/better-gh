//! Input validation for logins, emails and passwords.

/// Paths and names that can't be used as user/org logins because they
/// collide with top-level routes or are otherwise special.
pub const RESERVED_LOGINS: &[&str] = &[
    "_bgh",
    "about",
    "account",
    "admin",
    "api",
    "apps",
    "assets",
    "avatars",
    "dashboard",
    "enterprise",
    "explore",
    "favicon.ico",
    "ghost",
    "github",
    "healthz",
    "issues",
    "join",
    "login",
    "logout",
    "marketplace",
    "new",
    "notifications",
    "organizations",
    "orgs",
    "pulls",
    "raw",
    "robots.txt",
    "search",
    "security",
    "sessions",
    "settings",
    "signup",
    "site",
    "stars",
    "static",
    "sw.js",
    "user",
    "users",
];

/// GitHub login rules: 1–39 ASCII alphanumerics or single hyphens, not
/// starting or ending with a hyphen.
pub fn is_valid_login(login: &str) -> bool {
    let b = login.as_bytes();
    !b.is_empty()
        && b.len() <= 39
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
        && !login.contains("--")
}

pub fn is_reserved_login(login: &str) -> bool {
    let l = login.to_ascii_lowercase();
    RESERVED_LOGINS.contains(&l.as_str())
}

/// Minimal sanity check; real verification happens by sending mail.
pub fn is_valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && email.len() <= 254
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_PASSWORD_LEN: usize = 1024;

pub fn is_valid_password(password: &str) -> bool {
    (MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&password.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logins() {
        assert!(is_valid_login("octo-cat"));
        assert!(is_valid_login("a"));
        assert!(!is_valid_login("-a"));
        assert!(!is_valid_login("a-"));
        assert!(!is_valid_login("a--b"));
        assert!(!is_valid_login("a_b"));
        assert!(!is_valid_login(&"a".repeat(40)));
        assert!(is_reserved_login("API"));
    }

    #[test]
    fn emails_and_passwords() {
        assert!(is_valid_email("a@b.co"));
        assert!(!is_valid_email("a@b"));
        assert!(!is_valid_email("a b@c.d"));
        assert!(is_valid_password("12345678"));
        assert!(!is_valid_password("short"));
    }
}

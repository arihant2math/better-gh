//! Thin LDAP client over `ldap3`: connection setup (plain, LDAPS,
//! StartTLS, custom CA), service bind, user lookup, password check and
//! group membership.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bgh_core::settings::LdapSettings;
use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, Scope, SearchEntry, ldap_escape};

/// Connect / operation timeout.
const TIMEOUT: Duration = Duration::from_secs(10);
/// LDAP result code for a wrong password / unknown bind DN.
const INVALID_CREDENTIALS: u32 = 49;

#[derive(Debug, thiserror::Error)]
pub enum LdapError {
    #[error("LDAP is not configured: {0}")]
    Config(String),
    #[error("LDAP server unavailable: {0}")]
    Unavailable(String),
    #[error("LDAP error: {0}")]
    Protocol(String),
}

impl From<ldap3::LdapError> for LdapError {
    fn from(e: ldap3::LdapError) -> Self {
        match e {
            ldap3::LdapError::Io { .. }
            | ldap3::LdapError::Timeout { .. }
            | ldap3::LdapError::EndOfStream
            | ldap3::LdapError::UrlParsing { .. } => Self::Unavailable(e.to_string()),
            other => Self::Protocol(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, LdapError>;

/// A user entry, mapped through the configured attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirUser {
    /// Entry DN as returned by the server.
    pub dn: String,
    pub uid: String,
    pub name: Option<String>,
    pub emails: Vec<String>,
    pub ssh_keys: Vec<String>,
    pub gpg_keys: Vec<String>,
    /// Locked / disabled in the directory (AD `userAccountControl` bit 2,
    /// `nsAccountLock`, `pwdAccountLockedTime`).
    pub disabled: bool,
}

/// Normalize a DN for comparison and storage: lower case, no spaces
/// around RDN separators and `=`.
pub fn normalize_dn(dn: &str) -> String {
    dn.split(',')
        .map(|rdn| {
            let (k, v) = rdn.split_once('=').unwrap_or((rdn, ""));
            format!("{}={}", k.trim(), v.trim())
        })
        .collect::<Vec<_>>()
        .join(",")
        .to_lowercase()
}

/// Entry attributes by lower-case name.
fn attrs(entry: SearchEntry) -> (String, HashMap<String, Vec<String>>) {
    let map = entry
        .attrs
        .into_iter()
        .map(|(k, v)| (k.to_lowercase(), v))
        .collect();
    (entry.dn, map)
}

fn first(map: &HashMap<String, Vec<String>>, key: &str) -> Option<String> {
    map.get(&key.to_lowercase())
        .and_then(|v| v.first())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn all(map: &HashMap<String, Vec<String>>, key: Option<&str>) -> Vec<String> {
    key.filter(|k| !k.is_empty())
        .and_then(|k| map.get(&k.to_lowercase()))
        .map(|v| {
            v.iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn disabled(map: &HashMap<String, Vec<String>>) -> bool {
    let uac = first(map, "userAccountControl")
        .and_then(|v| v.parse::<u32>().ok())
        .is_some_and(|v| v & 2 != 0);
    let ns_lock = first(map, "nsAccountLock").is_some_and(|v| v.eq_ignore_ascii_case("true"));
    uac || ns_lock || first(map, "pwdAccountLockedTime").is_some()
}

/// A connected (and service-bound) directory session.
pub struct Directory {
    cfg: LdapSettings,
    ldap: Ldap,
}

fn tls_config(cfg: &LdapSettings) -> Result<Option<Arc<rustls::ClientConfig>>> {
    let Some(pem) = cfg.ca_cert.as_deref().filter(|p| !p.trim().is_empty()) else {
        return Ok(None);
    };
    use rustls_pki_types::CertificateDer;
    use rustls_pki_types::pem::PemObject;
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let cert = cert.map_err(|e| LdapError::Config(format!("invalid CA certificate: {e}")))?;
        roots
            .add(cert)
            .map_err(|e| LdapError::Config(format!("invalid CA certificate: {e}")))?;
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| LdapError::Config(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Some(Arc::new(config)))
}

impl Directory {
    /// Connect to the configured server and bind with the service account
    /// (anonymously when no bind DN is set).
    pub async fn connect(cfg: &LdapSettings) -> Result<Self> {
        if cfg.host.trim().is_empty() {
            return Err(LdapError::Config("no host".into()));
        }
        let scheme = if cfg.encryption == "ldaps" {
            "ldaps"
        } else {
            "ldap"
        };
        let url = format!("{scheme}://{}:{}", cfg.host.trim(), cfg.port);
        let mut settings = LdapConnSettings::new()
            .set_conn_timeout(TIMEOUT)
            .set_starttls(cfg.encryption == "starttls")
            .set_no_tls_verify(!cfg.verify_certificate);
        if let Some(tls) = tls_config(cfg)? {
            settings = settings.set_config(tls);
        }
        let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &url).await?;
        ldap3::drive!(conn);
        ldap.with_timeout(TIMEOUT);
        let mut dir = Self {
            cfg: cfg.clone(),
            ldap,
        };
        dir.service_bind().await?;
        Ok(dir)
    }

    async fn service_bind(&mut self) -> Result<()> {
        let dn = self.cfg.bind_dn.clone().unwrap_or_default();
        let pw = self.cfg.bind_password.clone().unwrap_or_default();
        let res = self.ldap.simple_bind(&dn, &pw).await?;
        if res.rc != 0 {
            return Err(LdapError::Protocol(format!(
                "service bind failed (code {}): {}",
                res.rc, res.text
            )));
        }
        Ok(())
    }

    fn attr_list(&self) -> Vec<String> {
        let c = &self.cfg;
        let mut list = vec![
            c.uid_field.clone(),
            c.name_field.clone(),
            c.email_field.clone(),
            "userAccountControl".into(),
            "nsAccountLock".into(),
            "pwdAccountLockedTime".into(),
        ];
        list.extend(c.ssh_key_field.clone());
        list.extend(c.gpg_key_field.clone());
        list.retain(|a| !a.is_empty());
        list
    }

    fn user_filter(&self, inner: &str) -> String {
        match self
            .cfg
            .user_filter
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty())
        {
            Some(extra) if extra.starts_with('(') => format!("(&{inner}{extra})"),
            Some(extra) => format!("(&{inner}({extra}))"),
            None => inner.to_string(),
        }
    }

    fn to_user(&self, entry: SearchEntry) -> Option<DirUser> {
        let (dn, map) = attrs(entry);
        let c = &self.cfg;
        Some(DirUser {
            uid: first(&map, &c.uid_field)?,
            name: first(&map, &c.name_field),
            emails: all(&map, Some(&c.email_field)),
            ssh_keys: all(&map, c.ssh_key_field.as_deref()),
            gpg_keys: all(&map, c.gpg_key_field.as_deref()),
            disabled: disabled(&map),
            dn,
        })
    }

    /// Find a user by login (the `uid_field`) under the search bases.
    pub async fn find_user(&mut self, login: &str) -> Result<Option<DirUser>> {
        let filter = self.user_filter(&format!("({}={})", self.cfg.uid_field, ldap_escape(login)));
        let attrs = self.attr_list();
        for base in self.cfg.user_search_bases.clone() {
            let (entries, _) = self
                .ldap
                .search(&base, Scope::Subtree, &filter, &attrs)
                .await?
                .success()
                .map_err(LdapError::from)?;
            if let Some(user) = entries
                .into_iter()
                .map(SearchEntry::construct)
                .find_map(|e| self.to_user(e))
            {
                return Ok(Some(user));
            }
        }
        Ok(None)
    }

    /// Read a user entry by DN (`None` when it is gone or no longer
    /// matches the user filter).
    pub async fn user_by_dn(&mut self, dn: &str) -> Result<Option<DirUser>> {
        let filter = self.user_filter("(objectClass=*)");
        let attrs = self.attr_list();
        let res = self.ldap.search(dn, Scope::Base, &filter, &attrs).await?;
        // 32 = noSuchObject
        if res.1.rc == 32 {
            return Ok(None);
        }
        let (entries, _) = res.success().map_err(LdapError::from)?;
        Ok(entries
            .into_iter()
            .map(SearchEntry::construct)
            .find_map(|e| self.to_user(e)))
    }

    /// Check `password` for `dn` (a separate connection, so the service
    /// session stays bound). Empty passwords are refused (an empty simple
    /// bind is an anonymous bind).
    pub async fn check_password(&self, dn: &str, password: &str) -> Result<bool> {
        if password.is_empty() {
            return Ok(false);
        }
        let mut cfg = self.cfg.clone();
        cfg.bind_dn = None;
        cfg.bind_password = None;
        let mut user = Self::connect(&cfg).await?;
        let res = user.ldap.simple_bind(dn, password).await?;
        let _ = user.ldap.unbind().await;
        match res.rc {
            0 => Ok(true),
            INVALID_CREDENTIALS => Ok(false),
            rc => Err(LdapError::Protocol(format!(
                "bind failed (code {rc}): {}",
                res.text
            ))),
        }
    }

    /// Members of a group entry: normalized member DNs (`member`,
    /// `uniqueMember`) and member uids (`memberUid`). `None` when the
    /// group does not exist.
    pub async fn group_members(&mut self, group_dn: &str) -> Result<Option<GroupMembers>> {
        let res = self
            .ldap
            .search(
                group_dn,
                Scope::Base,
                "(objectClass=*)",
                vec!["member", "uniqueMember", "memberUid"],
            )
            .await?;
        if res.1.rc == 32 {
            return Ok(None);
        }
        let (entries, _) = res.success().map_err(LdapError::from)?;
        let Some(entry) = entries.into_iter().next() else {
            return Ok(None);
        };
        let (_, map) = attrs(SearchEntry::construct(entry));
        let mut members = GroupMembers::default();
        for key in ["member", "uniquemember"] {
            for dn in map.get(key).into_iter().flatten() {
                members.dns.push(normalize_dn(dn));
            }
        }
        for uid in map.get("memberuid").into_iter().flatten() {
            members.uids.push(uid.trim().to_lowercase());
        }
        Ok(Some(members))
    }

    pub async fn close(mut self) {
        let _ = self.ldap.unbind().await;
    }
}

/// Members of a directory group.
#[derive(Debug, Clone, Default)]
pub struct GroupMembers {
    pub dns: Vec<String>,
    pub uids: Vec<String>,
}

impl GroupMembers {
    pub fn contains(&self, user: &DirUser) -> bool {
        let dn = normalize_dn(&user.dn);
        self.dns.contains(&dn) || self.uids.contains(&user.uid.to_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_dns() {
        assert_eq!(
            normalize_dn("CN=Alice Smith, OU=People ,DC=Example,DC=com"),
            "cn=alice smith,ou=people,dc=example,dc=com"
        );
    }
}

//! In-process fake LDAP server for tests (feature `testing`), built on
//! `ldap3_proto`: simple binds, base / subtree searches with the common
//! filter forms (`&`, `|`, `!`, `=`, presence, substrings), unbind.
//!
//! ```ignore
//! let ldap = FakeLdap::start().await;
//! let dn = ldap.add_user("alice", "pw", &[("mail", "alice@example.com")]);
//! ldap.add_group("admins", &[&dn]);
//! let settings = ldap.settings();    // auth_providers.ldap pointing at it
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bgh_core::settings::LdapSettings;
use futures::{SinkExt, StreamExt};
use ldap3_proto::proto::{LdapFilter, LdapPartialAttribute, LdapSearchResultEntry};
use ldap3_proto::{LdapCodec, LdapResultCode, LdapSearchScope, ServerOps};
use tokio::net::TcpListener;
use tokio_util::codec::Framed;

use super::normalize_dn;

pub const BASE: &str = "dc=example,dc=com";
pub const PEOPLE: &str = "ou=people,dc=example,dc=com";
pub const GROUPS: &str = "ou=groups,dc=example,dc=com";
pub const SERVICE_DN: &str = "cn=service,dc=example,dc=com";
pub const SERVICE_PASSWORD: &str = "service-secret";

#[derive(Debug, Clone)]
struct Entry {
    dn: String,
    attrs: Vec<(String, Vec<String>)>,
}

impl Entry {
    fn values(&self, attr: &str) -> Vec<&str> {
        self.attrs
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(attr))
            .flat_map(|(_, v)| v.iter().map(String::as_str))
            .collect()
    }

    fn matches(&self, f: &LdapFilter) -> bool {
        match f {
            LdapFilter::And(fs) => fs.iter().all(|f| self.matches(f)),
            LdapFilter::Or(fs) => fs.iter().any(|f| self.matches(f)),
            LdapFilter::Not(f) => !self.matches(f),
            LdapFilter::Present(a) => !self.values(a).is_empty(),
            LdapFilter::Equality(a, v) => {
                let v = v.to_lowercase();
                self.values(a).iter().any(|x| x.to_lowercase() == v)
            }
            LdapFilter::Substring(a, s) => self.values(a).iter().any(|x| {
                let x = x.to_lowercase();
                let mut rest = x.as_str();
                if let Some(i) = &s.initial {
                    let Some(r) = rest.strip_prefix(&i.to_lowercase()) else {
                        return false;
                    };
                    rest = r;
                }
                for any in &s.any {
                    let any = any.to_lowercase();
                    let Some(pos) = rest.find(&any) else {
                        return false;
                    };
                    rest = &rest[pos + any.len()..];
                }
                s.final_
                    .as_ref()
                    .is_none_or(|f| rest.ends_with(&f.to_lowercase()))
            }),
            _ => false,
        }
    }
}

#[derive(Default)]
struct Dit {
    entries: Vec<Entry>,
    /// normalized DN → password
    passwords: HashMap<String, String>,
    binds: usize,
}

/// A running fake directory (stopped when dropped).
pub struct FakeLdap {
    port: u16,
    dit: Arc<Mutex<Dit>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeLdap {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeLdap {
    /// Listen on a random local port with the service account and the
    /// `ou=people` / `ou=groups` containers.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let dit = Arc::new(Mutex::new(Dit::default()));
        let server = Self {
            port,
            dit: dit.clone(),
            task: tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let dit = dit.clone();
                    tokio::spawn(async move {
                        let mut framed = Framed::new(stream, LdapCodec::default());
                        while let Some(Ok(msg)) = framed.next().await {
                            let Ok(op) = ServerOps::try_from(msg) else {
                                break;
                            };
                            let replies = handle(&dit, op);
                            let Some(replies) = replies else { break };
                            for r in replies {
                                if framed.send(r).await.is_err() {
                                    return;
                                }
                            }
                        }
                    });
                }
            }),
        };
        server.add(SERVICE_DN, &[("objectClass", "person"), ("cn", "service")]);
        server.set_password(SERVICE_DN, SERVICE_PASSWORD);
        server.add(PEOPLE, &[("objectClass", "organizationalUnit")]);
        server.add(GROUPS, &[("objectClass", "organizationalUnit")]);
        server
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Successful and failed binds so far.
    pub fn binds(&self) -> usize {
        self.dit.lock().expect("lock").binds
    }

    /// `auth_providers.ldap` settings for this server (service bind,
    /// `ou=people` base, `uid` logins, `sshPublicKey` keys).
    pub fn settings(&self) -> LdapSettings {
        LdapSettings {
            enabled: true,
            host: "127.0.0.1".into(),
            port: self.port,
            bind_dn: Some(SERVICE_DN.into()),
            bind_password: Some(SERVICE_PASSWORD.into()),
            user_search_bases: vec![PEOPLE.into()],
            user_filter: Some("(objectClass=person)".into()),
            ssh_key_field: Some("sshPublicKey".into()),
            ..LdapSettings::default()
        }
    }

    /// Add (or replace) an entry with single-valued attributes; repeated
    /// keys become multiple values.
    pub fn add(&self, dn: &str, attrs: &[(&str, &str)]) {
        let mut map: Vec<(String, Vec<String>)> = Vec::new();
        for (k, v) in attrs {
            match map.iter_mut().find(|(x, _)| x.eq_ignore_ascii_case(k)) {
                Some((_, vals)) => vals.push(v.to_string()),
                None => map.push((k.to_string(), vec![v.to_string()])),
            }
        }
        let mut dit = self.dit.lock().expect("lock");
        let norm = normalize_dn(dn);
        dit.entries.retain(|e| normalize_dn(&e.dn) != norm);
        dit.entries.push(Entry {
            dn: dn.to_string(),
            attrs: map,
        });
    }

    pub fn set_password(&self, dn: &str, password: &str) {
        self.dit
            .lock()
            .expect("lock")
            .passwords
            .insert(normalize_dn(dn), password.to_string());
    }

    /// Add a person under `ou=people` (`uid`, `cn`, `objectClass=person`
    /// plus `attrs`); returns its DN.
    pub fn add_user(&self, uid: &str, password: &str, attrs: &[(&str, &str)]) -> String {
        let dn = format!("uid={uid},{PEOPLE}");
        let mut all = vec![("objectClass", "person"), ("uid", uid)];
        if !attrs.iter().any(|(k, _)| k.eq_ignore_ascii_case("cn")) {
            all.push(("cn", uid));
        }
        all.extend_from_slice(attrs);
        self.add(&dn, &all);
        self.set_password(&dn, password);
        dn
    }

    /// Add a `groupOfNames` under `ou=groups`; returns its DN.
    pub fn add_group(&self, cn: &str, members: &[&str]) -> String {
        let dn = format!("cn={cn},{GROUPS}");
        let mut attrs = vec![("objectClass", "groupOfNames"), ("cn", cn)];
        attrs.extend(members.iter().map(|m| ("member", *m)));
        self.add(&dn, &attrs);
        dn
    }

    /// Replace the values of one attribute (empty = remove it).
    pub fn set_attr(&self, dn: &str, attr: &str, values: &[&str]) {
        let mut dit = self.dit.lock().expect("lock");
        let norm = normalize_dn(dn);
        if let Some(e) = dit.entries.iter_mut().find(|e| normalize_dn(&e.dn) == norm) {
            e.attrs.retain(|(k, _)| !k.eq_ignore_ascii_case(attr));
            if !values.is_empty() {
                e.attrs.push((
                    attr.to_string(),
                    values.iter().map(|v| v.to_string()).collect(),
                ));
            }
        }
    }

    /// Delete an entry.
    pub fn remove(&self, dn: &str) {
        let norm = normalize_dn(dn);
        let mut dit = self.dit.lock().expect("lock");
        dit.entries.retain(|e| normalize_dn(&e.dn) != norm);
        dit.passwords.remove(&norm);
    }
}

fn handle(dit: &Mutex<Dit>, op: ServerOps) -> Option<Vec<ldap3_proto::LdapMsg>> {
    let mut dit = dit.lock().expect("lock");
    Some(match op {
        ServerOps::SimpleBind(b) => {
            dit.binds += 1;
            let ok = (b.dn.is_empty() && b.pw.is_empty())
                || (!b.pw.is_empty() && dit.passwords.get(&normalize_dn(&b.dn)) == Some(&b.pw));
            vec![if ok {
                b.gen_success()
            } else {
                b.gen_invalid_cred()
            }]
        }
        ServerOps::Search(s) => {
            let base = normalize_dn(&s.base);
            let in_scope = |e: &Entry| {
                let dn = normalize_dn(&e.dn);
                match s.scope {
                    LdapSearchScope::Base => dn == base,
                    _ => dn == base || dn.ends_with(&format!(",{base}")),
                }
            };
            if !base.is_empty() && !dit.entries.iter().any(|e| normalize_dn(&e.dn) == base) {
                return Some(vec![
                    s.gen_error(LdapResultCode::NoSuchObject, "no such object".into()),
                ]);
            }
            let mut out: Vec<_> = dit
                .entries
                .iter()
                .filter(|e| in_scope(e) && e.matches(&s.filter))
                .map(|e| {
                    let wanted = |k: &str| {
                        s.attrs.is_empty()
                            || s.attrs
                                .iter()
                                .any(|a| a == "*" || a.eq_ignore_ascii_case(k))
                    };
                    s.gen_result_entry(LdapSearchResultEntry {
                        dn: e.dn.clone(),
                        attributes: e
                            .attrs
                            .iter()
                            .filter(|(k, _)| wanted(k))
                            .map(|(k, v)| LdapPartialAttribute {
                                atype: k.clone(),
                                vals: v.iter().map(|x| x.as_bytes().to_vec()).collect(),
                            })
                            .collect(),
                    })
                })
                .collect();
            out.push(s.gen_success());
            out
        }
        ServerOps::Unbind(_) => return None,
        ServerOps::Whoami(w) => vec![w.gen_success("")],
        ServerOps::Compare(c) => vec![c.gen_compare_false()],
    })
}

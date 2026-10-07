//! Sync scopes (`repo:{id}`, `org:{id}`, `user:{id}`) and who may read them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::str::FromStr;

use bgh_core::auth::AuthContext;
use bgh_core::models::db;
use bgh_core::perms::{self, Permission};
use sqlx::PgConnection;

/// A parsed sync scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    User(i64),
    Org(i64),
    Repo(i64),
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User(id) => write!(f, "user:{id}"),
            Self::Org(id) => write!(f, "org:{id}"),
            Self::Repo(id) => write!(f, "repo:{id}"),
        }
    }
}

impl FromStr for Scope {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let (kind, id) = s.split_once(':').ok_or(())?;
        let id: i64 = id.parse().map_err(|_| ())?;
        if id <= 0 {
            return Err(());
        }
        match kind {
            "user" => Ok(Self::User(id)),
            "org" => Ok(Self::Org(id)),
            "repo" => Ok(Self::Repo(id)),
            _ => Err(()),
        }
    }
}

/// Result of a permission check over a list of requested scopes.
#[derive(Debug, Default, Clone)]
pub struct Access {
    /// Readable scopes, sorted, without duplicates.
    pub allowed: Vec<Scope>,
    /// Requested scopes (as given) the viewer can't read or that don't parse.
    pub denied: Vec<String>,
    /// Effective permission per allowed repository.
    pub repo_perms: HashMap<i64, Permission>,
}

impl Access {
    pub fn repo_ids(&self) -> Vec<i64> {
        ids(&self.allowed, |s| match s {
            Scope::Repo(id) => Some(*id),
            _ => None,
        })
    }

    pub fn org_ids(&self) -> Vec<i64> {
        ids(&self.allowed, |s| match s {
            Scope::Org(id) => Some(*id),
            _ => None,
        })
    }

    pub fn has_user_scope(&self, user_id: i64) -> bool {
        self.allowed.contains(&Scope::User(user_id))
    }
}

fn ids(scopes: &[Scope], f: impl Fn(&Scope) -> Option<i64>) -> Vec<i64> {
    scopes.iter().filter_map(f).collect()
}

/// Check which of `requested` the viewer may read: `user:{viewer}` only,
/// `org:{id}` for members, `repo:{id}` with at least read permission
/// (token scopes applied). Nonexistent ids are denied like forbidden ones.
pub async fn check(
    conn: &mut PgConnection,
    auth: &AuthContext,
    requested: &[String],
) -> Result<Access, sqlx::Error> {
    let mut out = check_many(conn, &[(auth, requested)]).await?;
    Ok(out.pop().unwrap_or_default())
}

/// [`check`] for many viewers at once (one result per request, in order).
/// Org memberships and repository rows are loaded once for the union of
/// all requests, raw permissions once per distinct user, so the number of
/// queries is `2 + users` however many sockets and scopes there are.
pub async fn check_many(
    conn: &mut PgConnection,
    requests: &[(&AuthContext, &[String])],
) -> Result<Vec<Access>, sqlx::Error> {
    let mut parsed: Vec<(Access, BTreeSet<Scope>)> = Vec::with_capacity(requests.len());
    let mut users = BTreeSet::new();
    let mut org_ids = BTreeSet::new();
    // Repositories wanted per user (raw permissions are per user).
    let mut user_repos: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
    for (auth, requested) in requests {
        let mut access = Access::default();
        let mut set = BTreeSet::new();
        for raw in requested.iter() {
            match raw.parse::<Scope>() {
                Ok(s) => {
                    match s {
                        Scope::Org(id) => {
                            org_ids.insert(id);
                        }
                        Scope::Repo(id) => {
                            user_repos.entry(auth.user.id).or_default().insert(id);
                        }
                        Scope::User(_) => {}
                    }
                    set.insert(s);
                }
                Err(()) => access.denied.push(raw.clone()),
            }
        }
        users.insert(auth.user.id);
        parsed.push((access, set));
    }

    let member_of: BTreeSet<(i64, i64)> = if org_ids.is_empty() {
        BTreeSet::new()
    } else {
        let users: Vec<i64> = users.iter().copied().collect();
        let orgs: Vec<i64> = org_ids.into_iter().collect();
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT user_id, org_id FROM org_members WHERE user_id = ANY($1) AND org_id = ANY($2)",
        )
        .bind(&users)
        .bind(&orgs)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect()
    };

    let all_repos: Vec<i64> = user_repos
        .values()
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let repos: HashMap<i64, db::Repository> = if all_repos.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, db::Repository>(&format!(
            "SELECT {} FROM repositories WHERE id = ANY($1)",
            db::Repository::COLUMNS
        ))
        .bind(&all_repos)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .map(|r| (r.id, r))
        .collect()
    };
    let mut raw: HashMap<i64, HashMap<i64, Permission>> = HashMap::new();
    for (uid, ids) in &user_repos {
        let rows: Vec<db::Repository> =
            ids.iter().filter_map(|id| repos.get(id).cloned()).collect();
        if rows.is_empty() {
            continue;
        }
        raw.insert(
            *uid,
            perms::repo_permissions(&mut *conn, Some(*uid), &rows).await?,
        );
    }

    let mut out = Vec::with_capacity(parsed.len());
    for ((auth, _), (mut access, set)) in requests.iter().zip(parsed) {
        let uid = auth.user.id;
        let user_raw = raw.get(&uid);
        for scope in set {
            let ok = match scope {
                Scope::User(id) => id == uid,
                Scope::Org(id) => member_of.contains(&(uid, id)),
                Scope::Repo(id) => match (repos.get(&id), user_raw.and_then(|m| m.get(&id))) {
                    (Some(repo), Some(&p)) => {
                        let p = perms::effective(Some(*auth), repo, p);
                        if p >= Permission::Read {
                            access.repo_perms.insert(id, p);
                            true
                        } else {
                            false
                        }
                    }
                    _ => false,
                },
            };
            if ok {
                access.allowed.push(scope);
            } else {
                access.denied.push(scope.to_string());
            }
        }
        out.push(access);
    }
    Ok(out)
}

/// Effective permissions of the viewer on `repo_ids`; repositories they
/// can't read (or that don't exist) are absent from the map.
pub async fn repo_permissions(
    conn: &mut PgConnection,
    auth: &AuthContext,
    repo_ids: &[i64],
) -> Result<HashMap<i64, Permission>, sqlx::Error> {
    let scopes: Vec<String> = repo_ids
        .iter()
        .map(|id| Scope::Repo(*id).to_string())
        .collect();
    Ok(check(conn, auth, &scopes).await?.repo_perms)
}

/// The viewer's default scope set: `user:{viewer}`, every org they belong
/// to, and every repository they have explicit access to (owner,
/// collaborator, team grant incl. parent teams, org base permission or org
/// admin). Public repositories without explicit access are not included.
pub async fn default_scopes(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
    let orgs: Vec<i64> =
        sqlx::query_scalar("SELECT org_id FROM org_members WHERE user_id = $1 ORDER BY org_id")
            .bind(user_id)
            .fetch_all(&mut *conn)
            .await?;
    let repos: Vec<i64> = sqlx::query_scalar(
        "WITH RECURSIVE ut AS (
             SELECT tm.team_id AS id FROM team_members tm WHERE tm.user_id = $1
             UNION
             SELECT t.parent_id FROM teams t JOIN ut ON t.id = ut.id WHERE t.parent_id IS NOT NULL
         )
         SELECT id FROM repositories WHERE owner_id = $1
         UNION SELECT repo_id FROM collaborators WHERE user_id = $1
         UNION SELECT repo_id FROM team_repos WHERE team_id IN (SELECT id FROM ut)
         UNION SELECT r.id FROM repositories r
                 JOIN org_members m ON m.org_id = r.owner_id AND m.user_id = $1
                 LEFT JOIN org_settings s ON s.org_id = r.owner_id
                WHERE m.role = 'admin' OR coalesce(s.default_repository_permission, 'read') <> 'none'
         ORDER BY 1",
    )
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = vec![Scope::User(user_id).to_string()];
    out.extend(orgs.into_iter().map(|id| Scope::Org(id).to_string()));
    out.extend(repos.into_iter().map(|id| Scope::Repo(id).to_string()));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scopes() {
        assert_eq!("repo:12".parse(), Ok(Scope::Repo(12)));
        assert_eq!("org:3".parse(), Ok(Scope::Org(3)));
        assert_eq!("user:1".parse(), Ok(Scope::User(1)));
        assert!("repo:".parse::<Scope>().is_err());
        assert!("repo:-1".parse::<Scope>().is_err());
        assert!("team:1".parse::<Scope>().is_err());
        assert!("repo".parse::<Scope>().is_err());
        assert_eq!(Scope::Repo(5).to_string(), "repo:5");
    }
}

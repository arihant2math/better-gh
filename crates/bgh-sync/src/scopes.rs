//! Sync scopes (`repo:{id}`, `org:{id}`, `user:{id}`) and who may read them.

use std::collections::{BTreeSet, HashMap};
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
    let mut access = Access::default();
    let mut parsed = BTreeSet::new();
    for raw in requested {
        match raw.parse::<Scope>() {
            Ok(s) => {
                parsed.insert(s);
            }
            Err(()) => access.denied.push(raw.clone()),
        }
    }
    let uid = auth.user.id;
    let org_ids: Vec<i64> = parsed
        .iter()
        .filter_map(|s| match s {
            Scope::Org(id) => Some(*id),
            _ => None,
        })
        .collect();
    let repo_ids: Vec<i64> = parsed
        .iter()
        .filter_map(|s| match s {
            Scope::Repo(id) => Some(*id),
            _ => None,
        })
        .collect();

    let member_of: BTreeSet<i64> = if org_ids.is_empty() {
        BTreeSet::new()
    } else {
        sqlx::query_scalar::<_, i64>(
            "SELECT org_id FROM org_members WHERE user_id = $1 AND org_id = ANY($2)",
        )
        .bind(uid)
        .bind(&org_ids)
        .fetch_all(&mut *conn)
        .await?
        .into_iter()
        .collect()
    };
    let repo_perms = repo_permissions(conn, auth, &repo_ids).await?;

    for scope in parsed {
        let ok = match scope {
            Scope::User(id) => id == uid,
            Scope::Org(id) => member_of.contains(&id),
            Scope::Repo(id) => repo_perms.contains_key(&id),
        };
        if ok {
            access.allowed.push(scope);
        } else {
            access.denied.push(scope.to_string());
        }
    }
    access.repo_perms = repo_perms;
    Ok(access)
}

/// Effective permissions of the viewer on `repo_ids`; repositories they
/// can't read (or that don't exist) are absent from the map.
pub async fn repo_permissions(
    conn: &mut PgConnection,
    auth: &AuthContext,
    repo_ids: &[i64],
) -> Result<HashMap<i64, Permission>, sqlx::Error> {
    if repo_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let repos: Vec<db::Repository> = sqlx::query_as(&format!(
        "SELECT {} FROM repositories WHERE id = ANY($1)",
        db::Repository::COLUMNS
    ))
    .bind(repo_ids)
    .fetch_all(&mut *conn)
    .await?;
    let raw = perms::repo_permissions(&mut *conn, Some(auth.user.id), &repos).await?;
    Ok(repos
        .iter()
        .filter_map(|r| {
            let p = perms::effective(Some(auth), r, raw.get(&r.id).copied()?);
            (p >= Permission::Read).then_some((r.id, p))
        })
        .collect())
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

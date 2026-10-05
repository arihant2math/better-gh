//! Generic row snapshots of a repository for soft delete and restore.
//!
//! Deleting a `repositories` row cascades through every table that
//! references it (`ON DELETE CASCADE`, transitively). [`capture`] walks the
//! foreign-key graph from the catalog and dumps exactly those rows as JSON
//! before the delete, plus the `ON DELETE SET NULL` references other rows
//! hold to them (forks' `parent_id`, ...). [`restore`] re-inserts the rows
//! with their original ids in dependency order and re-links the nulled
//! references, so no domain crate has to know about soft deletion and new
//! tables are covered automatically.
//!
//! Restoring is best effort per row: a row whose other references are gone
//! meanwhile (an assignee's account deleted, a cross-repo link to a deleted
//! issue) is skipped, the way the cascade would have removed it; a
//! `SET NULL` reference to a row that no longer exists stays null (ghost).
//! Identifiers in the generated SQL come from `pg_catalog` only.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgConnection;

/// Derived data that is rebuilt after a restore (code search index, git
/// maintenance bookkeeping) or must not come back (pending transfers,
/// webhook delivery logs).
const EXCLUDED: &[&str] = &[
    "code_files",
    "code_index_state",
    "commit_index",
    "repo_maintenance",
    "repo_maintenance_runs",
    "repo_transfers",
    "webhook_deliveries",
];

const ROOT: &str = "repositories";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// Tables with the rows to re-insert (any order; restore sorts).
    pub tables: Vec<TableRows>,
    /// `SET NULL` references to snapshot rows held by other rows.
    #[serde(default)]
    pub relinks: Vec<Relink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableRows {
    pub table: String,
    pub rows: Vec<Value>,
}

/// Rows of `table` whose `column` (an FK to `parent.parent_column`) was
/// nulled by the delete: `rows` are `{pk: .., column: ..}` objects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relink {
    pub table: String,
    pub pk: String,
    pub column: String,
    pub parent: String,
    pub parent_column: String,
    pub rows: Vec<Value>,
}

impl Snapshot {
    /// Rows of one table.
    pub fn rows(&self, table: &str) -> &[Value] {
        self.tables
            .iter()
            .find(|t| t.table == table)
            .map(|t| t.rows.as_slice())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct Fk {
    child: String,
    parent: String,
    on_delete: String,
    child_cols: Vec<String>,
    parent_cols: Vec<String>,
}

fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

fn cols(cs: &[String], alias: &str) -> String {
    cs.iter()
        .map(|c| format!("{alias}.{}", ident(c)))
        .collect::<Vec<_>>()
        .join(", ")
}

async fn foreign_keys(conn: &mut PgConnection) -> Result<Vec<Fk>, sqlx::Error> {
    sqlx::query_as(
        "SELECT cl.relname::text AS child, pl.relname::text AS parent,
                c.confdeltype::text AS on_delete,
                ARRAY(SELECT a.attname::text FROM unnest(c.conkey) WITH ORDINALITY k(n, i)
                        JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.n
                       ORDER BY k.i) AS child_cols,
                ARRAY(SELECT a.attname::text FROM unnest(c.confkey) WITH ORDINALITY k(n, i)
                        JOIN pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.n
                       ORDER BY k.i) AS parent_cols
           FROM pg_constraint c
           JOIN pg_class cl ON cl.oid = c.conrelid
           JOIN pg_class pl ON pl.oid = c.confrelid
           JOIN pg_namespace n ON n.oid = cl.relnamespace
          WHERE c.contype = 'f' AND n.nspname = current_schema()
          ORDER BY 1, 2, 4",
    )
    .fetch_all(&mut *conn)
    .await
}

/// Single-column primary keys by table.
async fn primary_keys(conn: &mut PgConnection) -> Result<HashMap<String, String>, sqlx::Error> {
    let rows: Vec<(String, Vec<String>)> = sqlx::query_as(
        "SELECT cl.relname::text,
                ARRAY(SELECT a.attname::text FROM unnest(c.conkey) k(n)
                        JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.n)
           FROM pg_constraint c
           JOIN pg_class cl ON cl.oid = c.conrelid
           JOIN pg_namespace n ON n.oid = cl.relnamespace
          WHERE c.contype = 'p' AND n.nspname = current_schema()",
    )
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, c)| c.len() == 1)
        .map(|(t, mut c)| (t, c.remove(0)))
        .collect())
}

/// Tables whose rows cascade from `repositories` (the root included).
fn cascade_set(fks: &[Fk]) -> BTreeSet<String> {
    let mut set = BTreeSet::from([ROOT.to_string()]);
    loop {
        let before = set.len();
        for fk in fks {
            if fk.on_delete == "c"
                && set.contains(&fk.parent)
                && !EXCLUDED.contains(&fk.child.as_str())
            {
                set.insert(fk.child.clone());
            }
        }
        if set.len() == before {
            return set;
        }
    }
}

/// SQL predicate (over the bare table) selecting the rows of `table` that
/// cascade from repository `$1`.
fn predicate(
    table: &str,
    fks: &[Fk],
    set: &BTreeSet<String>,
    memo: &mut HashMap<String, String>,
    stack: &mut Vec<String>,
) -> String {
    if table == ROOT {
        return "\"id\" = $1".into();
    }
    if let Some(p) = memo.get(table) {
        return p.clone();
    }
    stack.push(table.to_string());
    let mut parts = Vec::new();
    let incoming: Vec<&Fk> = fks
        .iter()
        .filter(|fk| {
            fk.child == table
                && fk.on_delete == "c"
                && fk.parent != table
                && set.contains(&fk.parent)
                && !stack.contains(&fk.parent)
        })
        .collect();
    for fk in incoming {
        let inner = predicate(&fk.parent, fks, set, memo, stack);
        let child_cols = fk
            .child_cols
            .iter()
            .map(|c| ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "({child_cols}) IN (SELECT {} FROM {} p WHERE {inner})",
            cols(&fk.parent_cols, "p"),
            ident(&fk.parent),
        ));
    }
    stack.pop();
    let pred = if parts.is_empty() {
        "false".to_string()
    } else {
        format!("({})", parts.join(" OR "))
    };
    memo.insert(table.to_string(), pred.clone());
    pred
}

/// Dump every row that deleting repository `repo_id` would remove.
pub async fn capture(conn: &mut PgConnection, repo_id: i64) -> Result<Snapshot, sqlx::Error> {
    let fks = foreign_keys(conn).await?;
    let pks = primary_keys(conn).await?;
    let set = cascade_set(&fks);
    let mut memo = HashMap::new();
    let mut snapshot = Snapshot::default();
    for table in &set {
        let pred = predicate(table, &fks, &set, &mut memo, &mut Vec::new());
        let rows: Value = sqlx::query_scalar(&format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t)), '[]'::jsonb) FROM {} t WHERE {pred}",
            ident(table)
        ))
        .bind(repo_id)
        .fetch_one(&mut *conn)
        .await?;
        let rows = match rows {
            Value::Array(rows) => rows,
            _ => Vec::new(),
        };
        if !rows.is_empty() {
            snapshot.tables.push(TableRows {
                table: table.clone(),
                rows,
            });
        }
    }
    // References that the delete nulls (`SET NULL`), so restore can put
    // them back. Rows that are part of the snapshot are covered too: their
    // values are nulled on insert while the target doesn't exist yet.
    for fk in fks
        .iter()
        .filter(|fk| fk.on_delete == "n" && fk.child_cols.len() == 1 && set.contains(&fk.parent))
    {
        let Some(pk) = pks.get(&fk.child) else {
            continue;
        };
        let pred = predicate(&fk.parent, &fks, &set, &mut memo, &mut Vec::new());
        let col = &fk.child_cols[0];
        let rows: Value = sqlx::query_scalar(&format!(
            "SELECT coalesce(jsonb_agg(jsonb_build_object($2::text, c.{pk_i}, $3::text, c.{col_i})), '[]'::jsonb)
               FROM {child} c
              WHERE c.{col_i} IN (SELECT p.{pcol} FROM {parent} p WHERE {pred})",
            pk_i = ident(pk),
            col_i = ident(col),
            child = ident(&fk.child),
            pcol = ident(&fk.parent_cols[0]),
            parent = ident(&fk.parent),
        ))
        .bind(repo_id)
        .bind(pk)
        .bind(col)
        .fetch_one(&mut *conn)
        .await?;
        if let Value::Array(rows) = rows
            && !rows.is_empty()
        {
            snapshot.relinks.push(Relink {
                table: fk.child.clone(),
                pk: pk.clone(),
                column: col.clone(),
                parent: fk.parent.clone(),
                parent_column: fk.parent_cols[0].clone(),
                rows,
            });
        }
    }
    Ok(snapshot)
}

/// Insertable (non-generated) columns by table.
async fn insertable_columns(
    conn: &mut PgConnection,
) -> Result<HashMap<String, Vec<String>>, sqlx::Error> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT table_name::text, column_name::text FROM information_schema.columns
          WHERE table_schema = current_schema() AND is_generated = 'NEVER'
          ORDER BY table_name, ordinal_position",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for (t, c) in rows {
        out.entry(t).or_default().push(c);
    }
    Ok(out)
}

/// Snapshot tables ordered so referenced tables come first.
fn insert_order(tables: &[String], fks: &[Fk]) -> Vec<String> {
    let present: BTreeSet<&str> = tables.iter().map(String::as_str).collect();
    let mut deps: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for fk in fks {
        if fk.child != fk.parent
            && present.contains(fk.child.as_str())
            && present.contains(fk.parent.as_str())
        {
            deps.entry(fk.child.as_str())
                .or_default()
                .insert(fk.parent.as_str());
        }
    }
    fn visit<'a>(
        t: &'a str,
        deps: &BTreeMap<&'a str, BTreeSet<&'a str>>,
        done: &mut BTreeSet<&'a str>,
        stack: &mut Vec<&'a str>,
        out: &mut Vec<String>,
    ) {
        if done.contains(t) || stack.contains(&t) {
            return;
        }
        stack.push(t);
        if let Some(ps) = deps.get(t) {
            for p in ps {
                visit(p, deps, done, stack, out);
            }
        }
        stack.pop();
        done.insert(t);
        out.push(t.to_string());
    }
    let mut done = BTreeSet::new();
    let mut out = Vec::new();
    // The root first, then the rest by name for a stable order.
    for t in std::iter::once(ROOT).chain(present.iter().copied()) {
        if present.contains(t) {
            visit(t, &deps, &mut done, &mut Vec::new(), &mut out);
        }
    }
    out
}

/// Outcome of [`restore`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RestoreStats {
    pub inserted: usize,
    pub skipped: usize,
}

async fn savepoint<T>(
    conn: &mut PgConnection,
    run: impl AsyncFnOnce(&mut PgConnection) -> Result<T, sqlx::Error>,
) -> Result<Result<T, sqlx::Error>, sqlx::Error> {
    sqlx::query("SAVEPOINT bgh_restore")
        .execute(&mut *conn)
        .await?;
    match run(conn).await {
        Ok(v) => {
            sqlx::query("RELEASE SAVEPOINT bgh_restore")
                .execute(&mut *conn)
                .await?;
            Ok(Ok(v))
        }
        Err(e @ sqlx::Error::Database(_)) => {
            sqlx::query("ROLLBACK TO SAVEPOINT bgh_restore")
                .execute(&mut *conn)
                .await?;
            Ok(Err(e))
        }
        Err(e) => Err(e),
    }
}

/// Re-insert a snapshot (inside the caller's transaction).
pub async fn restore(
    conn: &mut PgConnection,
    snapshot: &Snapshot,
) -> Result<RestoreStats, sqlx::Error> {
    let fks = foreign_keys(conn).await?;
    let columns = insertable_columns(conn).await?;
    let names: Vec<String> = snapshot.tables.iter().map(|t| t.table.clone()).collect();
    let mut stats = RestoreStats::default();
    for table in insert_order(&names, &fks) {
        let rows = snapshot.rows(&table);
        let Some(table_cols) = columns.get(&table) else {
            stats.skipped += rows.len();
            continue; // table dropped since
        };
        // Columns the snapshot has (new columns since take their default).
        let keys: BTreeSet<&str> = rows
            .iter()
            .filter_map(Value::as_object)
            .flat_map(|o| o.keys().map(String::as_str))
            .collect();
        let insert_cols: Vec<&String> = table_cols
            .iter()
            .filter(|c| keys.contains(c.as_str()))
            .collect();
        if insert_cols.is_empty() {
            continue;
        }
        // `SET NULL` references keep their value only when the target
        // exists (otherwise null, as the delete of the target would have
        // left it); relinks below fill in targets inserted later.
        let exprs: Vec<String> = insert_cols
            .iter()
            .map(|c| {
                let set_null = fks.iter().find(|fk| {
                    fk.child == table
                        && fk.on_delete == "n"
                        && fk.child_cols.len() == 1
                        && &fk.child_cols[0] == *c
                });
                match set_null {
                    Some(fk) => format!(
                        "(SELECT p.{pc} FROM {parent} p WHERE p.{pc} = r.{c})",
                        pc = ident(&fk.parent_cols[0]),
                        parent = ident(&fk.parent),
                        c = ident(c),
                    ),
                    None => format!("r.{}", ident(c)),
                }
            })
            .collect();
        let sql = format!(
            "INSERT INTO {t} ({cols}) OVERRIDING SYSTEM VALUE
             SELECT {exprs} FROM jsonb_populate_recordset(NULL::{t}, $1) r",
            t = ident(&table),
            cols = insert_cols
                .iter()
                .map(|c| ident(c))
                .collect::<Vec<_>>()
                .join(", "),
            exprs = exprs.join(", "),
        );
        let all = Value::Array(rows.to_vec());
        let bulk = savepoint(conn, async |c| {
            sqlx::query(&sql).bind(&all).execute(&mut *c).await
        })
        .await?;
        if let Ok(done) = bulk {
            stats.inserted += done.rows_affected() as usize;
            continue;
        }
        // Row by row, repeating while rows that depend on rows of the same
        // table (self references) make progress.
        let mut pending: Vec<&Value> = rows.iter().collect();
        loop {
            let mut failed = Vec::new();
            for row in &pending {
                let one = Value::Array(vec![(*row).clone()]);
                match savepoint(conn, async |c| {
                    sqlx::query(&sql).bind(&one).execute(&mut *c).await
                })
                .await?
                {
                    Ok(_) => stats.inserted += 1,
                    Err(_) => failed.push(*row),
                }
            }
            if failed.is_empty() || failed.len() == pending.len() {
                if !failed.is_empty() {
                    tracing::warn!(table, skipped = failed.len(), "restore skipped rows");
                }
                stats.skipped += failed.len();
                break;
            }
            pending = failed;
        }
    }
    for link in &snapshot.relinks {
        if !columns.contains_key(&link.table) {
            continue;
        }
        let sql = format!(
            "UPDATE {t} c SET {col} = s.{col}
               FROM jsonb_populate_recordset(NULL::{t}, $1) s
              WHERE c.{pk} = s.{pk} AND c.{col} IS NULL AND s.{col} IS NOT NULL
                AND EXISTS (SELECT 1 FROM {parent} p WHERE p.{pcol} = s.{col})",
            t = ident(&link.table),
            col = ident(&link.column),
            pk = ident(&link.pk),
            parent = ident(&link.parent),
            pcol = ident(&link.parent_column),
        );
        let rows = Value::Array(link.rows.clone());
        // A relink that no longer fits the schema is not worth failing for.
        let _ = savepoint(conn, async |c| {
            sqlx::query(&sql).bind(&rows).execute(&mut *c).await
        })
        .await?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fk(child: &str, parent: &str, on_delete: &str) -> Fk {
        Fk {
            child: child.into(),
            parent: parent.into(),
            on_delete: on_delete.into(),
            child_cols: vec![format!("{parent}_id")],
            parent_cols: vec!["id".into()],
        }
    }

    #[test]
    fn cascade_set_follows_cascades_only() {
        let fks = vec![
            fk("issues", "repositories", "c"),
            fk("comments", "issues", "c"),
            fk("repositories", "repositories", "n"),
            fk("issues", "users", "n"),
            fk("code_files", "repositories", "c"),
            fk("audit", "repositories", "n"),
        ];
        let set = cascade_set(&fks);
        assert_eq!(
            set.into_iter().collect::<Vec<_>>(),
            vec!["comments", "issues", "repositories"]
        );
    }

    #[test]
    fn predicate_nests_through_parents() {
        let fks = vec![
            fk("issues", "repositories", "c"),
            fk("comments", "issues", "c"),
            fk("comments", "comments", "c"),
        ];
        let set = cascade_set(&fks);
        let p = predicate("comments", &fks, &set, &mut HashMap::new(), &mut Vec::new());
        assert_eq!(
            p,
            "((\"issues_id\") IN (SELECT p.\"id\" FROM \"issues\" p WHERE \
             ((\"repositories_id\") IN (SELECT p.\"id\" FROM \"repositories\" p WHERE \"id\" = $1))))"
        );
    }

    #[test]
    fn insert_order_puts_parents_first() {
        let fks = vec![
            fk("comments", "issues", "c"),
            fk("issues", "repositories", "c"),
            fk("issues", "milestones", "n"),
            fk("milestones", "repositories", "c"),
        ];
        let tables: Vec<String> = ["comments", "issues", "milestones", "repositories"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            insert_order(&tables, &fks),
            ["repositories", "milestones", "issues", "comments"].map(String::from)
        );
    }

    #[test]
    fn ident_quotes() {
        assert_eq!(ident("a\"b"), "\"a\"\"b\"");
    }
}

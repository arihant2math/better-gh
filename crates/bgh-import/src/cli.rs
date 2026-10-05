//! Helpers for `bgh import github` (the command itself lives in the
//! binary, which owns the job registry).

use std::collections::BTreeMap;
use std::time::Duration;

use bgh_core::AppState;

use crate::row::ImportRow;

/// Parse a login map file: one `source,local` (or `source=local`,
/// `source local`) per line, `#` comments. GitHub Enterprise Importer's
/// mannequin CSV (`mannequin-user,mannequin-id,target-user`) works too:
/// the first column is the source login, the last the local one.
pub fn parse_user_map(text: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line
            .split([',', '=', ' ', '\t'])
            .map(|c| c.trim().trim_matches('"'))
            .filter(|c| !c.is_empty())
            .collect();
        if cols.len() < 2 {
            anyhow::bail!("line {}: expected `source,local`", n + 1);
        }
        let (source, local) = (cols[0], cols[cols.len() - 1]);
        if matches!(source, "mannequin-user" | "source" | "source-login") {
            continue; // header
        }
        map.insert(source.to_string(), local.to_string());
    }
    Ok(map)
}

/// Print the import's log as it grows until the run ends; returns the
/// final row.
pub async fn follow(state: &AppState, id: i64) -> anyhow::Result<ImportRow> {
    let mut after = 0i64;
    loop {
        let entries: Vec<(i64, String, String)> = sqlx::query_as(
            "SELECT id, level, message FROM import_log WHERE import_id = $1 AND id > $2 ORDER BY id",
        )
        .bind(id)
        .bind(after)
        .fetch_all(&state.db)
        .await?;
        for (entry, level, message) in entries {
            after = entry;
            println!("[{level}] {message}");
        }
        let row = ImportRow::find(&state.db, id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("import {id} disappeared"))?;
        if !row.is_active() {
            return Ok(row);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::parse_user_map;

    #[test]
    fn user_maps() {
        let m = parse_user_map(
            "# comment\nocto,alice\nhubot = bob\nmona carol\n\
             mannequin-user,mannequin-id,target-user\n\"dev\",\"MDQ6\",\"dave\"\n",
        )
        .unwrap();
        assert_eq!(m["octo"], "alice");
        assert_eq!(m["hubot"], "bob");
        assert_eq!(m["mona"], "carol");
        assert_eq!(m["dev"], "dave");
        assert_eq!(m.len(), 4);
        assert!(parse_user_map("lonely\n").is_err());
    }
}

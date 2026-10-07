//! Turning `sync_actions` rows into client `delta` messages.

use std::collections::{BTreeSet, HashMap};

use bgh_core::seqlog;
use bgh_core::sync::shapes::{self, Filter, Model, Opts};
use bgh_core::sync::{SyncAction, SyncRecord};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Maximum items per `batch` message.
pub const BATCH_SIZE: i64 = 500;

#[derive(sqlx::FromRow)]
struct ActionRow {
    id: i64,
    scope: String,
    model: String,
    model_id: i64,
    action: String,
    data: Value,
    tx: Option<Uuid>,
}

impl From<ActionRow> for SyncRecord {
    fn from(r: ActionRow) -> Self {
        SyncRecord {
            id: r.id,
            scope: r.scope,
            model: r.model,
            model_id: r.model_id,
            action: match r.action.as_str() {
                "I" => SyncAction::Insert,
                "D" => SyncAction::Delete,
                _ => SyncAction::Update,
            },
            data: r.data,
            tx: r.tx,
        }
    }
}

const ACTION_COLUMNS: &str = "id, scope, model, model_id, action::text AS action, data, tx";

/// Actions in `scopes` with `after < id <= upto`, ascending, at most `limit`.
pub async fn fetch_scoped(
    db: &PgPool,
    scopes: &[String],
    after: i64,
    upto: i64,
    limit: i64,
) -> Result<Vec<SyncRecord>, sqlx::Error> {
    let rows: Vec<ActionRow> = sqlx::query_as(&format!(
        "SELECT {ACTION_COLUMNS} FROM sync_actions
          WHERE scope = ANY($1) AND id > $2 AND id <= $3 ORDER BY id LIMIT $4"
    ))
    .bind(scopes)
    .bind(after)
    .bind(upto)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// All actions with `after < id <= upto`, ascending, at most `limit`.
pub async fn fetch_range(
    db: &PgPool,
    after: i64,
    upto: i64,
    limit: i64,
) -> Result<Vec<SyncRecord>, sqlx::Error> {
    let rows: Vec<ActionRow> = sqlx::query_as(&format!(
        "SELECT {ACTION_COLUMNS} FROM sync_actions
          WHERE id > $1 AND id <= $2 AND scope <> '!gap' ORDER BY id LIMIT $3"
    ))
    .bind(after)
    .bind(upto)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(Into::into).collect())
}

/// Current head: the commit-order watermark. Every id `<=` it is
/// committed, so nothing can appear below it later (`bgh_core::seqlog`).
pub async fn head(db: &PgPool) -> Result<i64, sqlx::Error> {
    seqlog::advance(db, seqlog::Log::Sync).await
}

/// Lowest sync id guaranteed to still be in the log.
pub async fn min_retained_id(db: impl sqlx::PgExecutor<'_>) -> Result<i64, sqlx::Error> {
    Ok(
        sqlx::query_scalar::<_, i64>("SELECT min_retained_id FROM sync_meta")
            .fetch_optional(db)
            .await?
            .unwrap_or(0),
    )
}

/// Whether a client at `since` can resume from the log.
pub fn can_resume(since: i64, min_retained: i64) -> bool {
    since >= min_retained - 1
}

#[derive(Serialize)]
struct DeltaOut<'a> {
    t: &'static str,
    id: i64,
    scope: &'a str,
    model: &'a str,
    mid: i64,
    a: SyncAction,
    d: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    tx: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refs: Option<Refs<'a>>,
}

#[derive(Serialize)]
struct Refs<'a> {
    user: Vec<&'a Value>,
}

/// A delta ready to send: the serialized `{"t":"delta",...}` object.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: i64,
    pub scope: String,
    pub model: String,
    pub json: String,
}

/// Serialize records as delta messages, attaching the referenced users
/// (`refs.user`) loaded with one query.
pub async fn to_items(
    conn: &mut PgConnection,
    records: &[SyncRecord],
) -> Result<Vec<Item>, sqlx::Error> {
    let mut wanted = BTreeSet::new();
    for r in records {
        if r.action != SyncAction::Delete {
            shapes::referenced_users(&r.model, &r.data, &mut wanted);
        }
    }
    let users: HashMap<i64, Value> = if wanted.is_empty() {
        HashMap::new()
    } else {
        let ids: Vec<i64> = wanted.into_iter().collect();
        shapes::load(conn, Model::User, Filter::Ids(&ids), Opts::default())
            .await?
            .into_iter()
            .map(|r| (r.id, r.data))
            .collect()
    };
    Ok(records.iter().map(|r| item(r, &users)).collect())
}

fn item(r: &SyncRecord, users: &HashMap<i64, Value>) -> Item {
    let null = Value::Null;
    let refs = if r.action == SyncAction::Delete {
        None
    } else {
        let mut ids = BTreeSet::new();
        shapes::referenced_users(&r.model, &r.data, &mut ids);
        let user: Vec<&Value> = ids.iter().filter_map(|id| users.get(id)).collect();
        (!user.is_empty()).then_some(Refs { user })
    };
    let out = DeltaOut {
        t: "delta",
        id: r.id,
        scope: &r.scope,
        model: &r.model,
        mid: r.model_id,
        a: r.action,
        d: if r.action == SyncAction::Delete {
            &null
        } else {
            &r.data
        },
        tx: r.tx,
        refs,
    };
    Item {
        id: r.id,
        scope: r.scope.clone(),
        model: r.model.clone(),
        json: serde_json::to_string(&out).expect("serializable delta"),
    }
}

/// `{"t":"delta",...}` for one item, `{"t":"batch","id":N,"items":[...]}`
/// for several (ascending ids).
pub fn message<'a>(items: impl IntoIterator<Item = &'a Item>) -> Option<String> {
    let items: Vec<&Item> = items.into_iter().collect();
    match items.as_slice() {
        [] => None,
        [one] => Some(one.json.clone()),
        many => {
            let last = many.last().map(|i| i.id).unwrap_or_default();
            let len: usize = many.iter().map(|i| i.json.len() + 1).sum();
            let mut s = String::with_capacity(len + 48);
            s.push_str(&format!("{{\"t\":\"batch\",\"id\":{last},\"items\":["));
            for (n, i) in many.iter().enumerate() {
                if n > 0 {
                    s.push(',');
                }
                s.push_str(&i.json);
            }
            s.push_str("]}");
            Some(s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(id: i64, action: SyncAction) -> SyncRecord {
        SyncRecord {
            id,
            scope: "repo:1".into(),
            model: "issue".into(),
            model_id: 42,
            action,
            data: json!({"id": 42, "authorId": 7}),
            tx: None,
        }
    }

    #[test]
    fn delta_and_batch_shapes() {
        let users = HashMap::from([(7, json!({"id": 7, "login": "ada"}))]);
        let a = item(&rec(5, SyncAction::Update), &users);
        let v: Value = serde_json::from_str(&a.json).unwrap();
        assert_eq!(
            v,
            json!({"t": "delta", "id": 5, "scope": "repo:1", "model": "issue", "mid": 42,
                   "a": "U", "d": {"id": 42, "authorId": 7},
                   "refs": {"user": [{"id": 7, "login": "ada"}]}})
        );
        let b = item(&rec(6, SyncAction::Delete), &users);
        let v: Value = serde_json::from_str(&b.json).unwrap();
        assert_eq!(v["d"], Value::Null);
        assert!(v.get("refs").is_none() && v.get("tx").is_none());

        assert_eq!(message([&a]).unwrap(), a.json);
        let batch: Value = serde_json::from_str(&message([&a, &b]).unwrap()).unwrap();
        assert_eq!(batch["t"], "batch");
        assert_eq!(batch["id"], 6);
        assert_eq!(batch["items"].as_array().unwrap().len(), 2);
        assert!(message(std::iter::empty()).is_none());
    }

    #[test]
    fn resume_window() {
        assert!(can_resume(0, 0));
        assert!(can_resume(0, 1));
        assert!(can_resume(9, 10));
        assert!(!can_resume(8, 10));
    }
}

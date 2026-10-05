//! GitHub's default label set for new repositories.
//!
//! Created **inside the repository-creation transaction** (bgh-repos'
//! create and generate-from-template paths call [`create_defaults`]), so a
//! repository never exists without its labels and nothing races an
//! asynchronous listener. Forks start without labels.

use crate::db::Tx;
use crate::error::ApiResult;
use crate::sync::SyncAction;
use crate::sync::shapes::Model;

/// GitHub's default labels: (name, color, description).
pub const DEFAULT_LABELS: [(&str, &str, &str); 9] = [
    ("bug", "d73a4a", "Something isn't working"),
    (
        "documentation",
        "0075ca",
        "Improvements or additions to documentation",
    ),
    (
        "duplicate",
        "cfd3d7",
        "This issue or pull request already exists",
    ),
    ("enhancement", "a2eeef", "New feature or request"),
    ("good first issue", "7057ff", "Good for newcomers"),
    ("help wanted", "008672", "Extra attention is needed"),
    ("invalid", "e4e669", "This doesn't seem right"),
    ("question", "d876e3", "Further information is requested"),
    ("wontfix", "ffffff", "This will not be worked on"),
];

/// Insert the default labels of `repo_id` (idempotent: existing names are
/// kept) and record their `label` sync actions. Call it in the transaction
/// that creates the repository, right before its own sync records.
pub async fn create_defaults(tx: &mut Tx, repo_id: i64) -> ApiResult<Vec<i64>> {
    let (names, rest): (Vec<&str>, Vec<(&str, &str)>) = DEFAULT_LABELS
        .iter()
        .map(|(n, c, d)| (*n, (*c, *d)))
        .unzip();
    let (colors, descriptions): (Vec<&str>, Vec<&str>) = rest.into_iter().unzip();
    let ids: Vec<i64> = sqlx::query_scalar(
        "INSERT INTO labels (repo_id, name, color, description, is_default)
         SELECT $1, n, c, d, true
           FROM unnest($2::text[], $3::text[], $4::text[]) WITH ORDINALITY AS t(n, c, d, o)
          ORDER BY o
         ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(repo_id)
    .bind(&names)
    .bind(&colors)
    .bind(&descriptions)
    .fetch_all(&mut **tx)
    .await?;
    tx.sync_models(Model::Label, &ids, SyncAction::Insert)
        .await?;
    Ok(ids)
}

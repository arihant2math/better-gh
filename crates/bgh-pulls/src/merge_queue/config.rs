//! Queue settings: the parameters of the `merge_queue` ruleset rule
//! (validated and defaulted by `bgh_repos::rulesets` when stored).

use bgh_core::prelude::*;
use serde::{Serialize, Serializer};

use crate::merge::MergeMethod;
use crate::protection::{self, Source};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum GroupingStrategy {
    /// Every entry of a merge group must pass its checks.
    #[serde(rename = "ALLGREEN")]
    AllGreen,
    /// Only the group's head commit must pass.
    #[serde(rename = "HEADGREEN")]
    HeadGreen,
}

/// Settings of the merge queue of one branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueueConfig {
    /// The ruleset providing the rule.
    #[serde(skip)]
    pub ruleset_id: i64,
    #[serde(serialize_with = "upper_method")]
    pub merge_method: MergeMethod,
    pub max_entries_to_build: i64,
    pub min_entries_to_merge: i64,
    pub max_entries_to_merge: i64,
    pub grouping_strategy: GroupingStrategy,
    pub check_response_timeout_minutes: i64,
    pub min_entries_to_merge_wait_minutes: i64,
}

fn upper_method<S: Serializer>(m: &MergeMethod, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&m.as_str().to_ascii_uppercase())
}

impl QueueConfig {
    /// Parse a `merge_queue` rule's `parameters` (defaults as GitHub's).
    pub fn from_params(ruleset_id: i64, p: &serde_json::Value) -> Self {
        let int = |k: &str, d: i64| p[k].as_i64().unwrap_or(d);
        Self {
            ruleset_id,
            merge_method: p["merge_method"]
                .as_str()
                .and_then(|m| MergeMethod::parse(&m.to_ascii_lowercase()))
                .unwrap_or(MergeMethod::Merge),
            max_entries_to_build: int("max_entries_to_build", 5),
            min_entries_to_merge: int("min_entries_to_merge", 1),
            max_entries_to_merge: int("max_entries_to_merge", 5),
            grouping_strategy: match p["grouping_strategy"].as_str() {
                Some("HEADGREEN") => GroupingStrategy::HeadGreen,
                _ => GroupingStrategy::AllGreen,
            },
            check_response_timeout_minutes: int("check_response_timeout_minutes", 60),
            min_entries_to_merge_wait_minutes: int("min_entries_to_merge_wait_minutes", 5),
        }
    }

    /// The queue settings in effect per `rules` (the first active ruleset
    /// with a `merge_queue` rule); `None` = no merge queue.
    pub fn from_rules(rules: &protection::Rules) -> Option<Self> {
        rules.sources.iter().find_map(|s| match &s.source {
            Source::Ruleset(r) if s.merge_queue => r
                .find_rule("merge_queue")
                .map(|rule| Self::from_params(r.id, &rule["parameters"])),
            _ => None,
        })
    }
}

/// The merge queue settings of `repo_id`'s `branch`, `None` when no
/// active ruleset with a `merge_queue` rule targets it.
pub async fn config_for(
    db: &sqlx::PgPool,
    repo_id: i64,
    branch: &str,
) -> ApiResult<Option<QueueConfig>> {
    let rules = protection::rules_for(db, repo_id, branch).await?;
    Ok(QueueConfig::from_rules(&rules))
}

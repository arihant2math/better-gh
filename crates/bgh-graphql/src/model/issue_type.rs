//! Issue types (`Issue.issueType`) and dependency summaries
//! (`Issue.issueDependenciesSummary`), batch-loaded per issue id.

use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::dataloader::Loader;
use async_graphql::{Enum, ID, Object, SimpleObject};
use bgh_core::node_id::NodeType;
use bgh_core::prelude::*;
use bgh_issues::dependencies::Summary;
use bgh_issues::issue_types::IssueTypeRow;

use super::nid;

/// The possible color for an issue type.
#[derive(Enum, Copy, Clone, Eq, PartialEq, Debug)]
pub enum IssueTypeColor {
    Gray,
    Blue,
    Green,
    Yellow,
    Orange,
    Red,
    Pink,
    Purple,
}

impl IssueTypeColor {
    fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "gray" => Self::Gray,
            "blue" => Self::Blue,
            "green" => Self::Green,
            "yellow" => Self::Yellow,
            "orange" => Self::Orange,
            "red" => Self::Red,
            "pink" => Self::Pink,
            "purple" => Self::Purple,
            _ => return None,
        })
    }
}

/// Represents the type of Issue.
#[derive(Clone)]
pub struct IssueType(pub Arc<IssueTypeRow>);

#[Object]
impl IssueType {
    pub async fn id(&self) -> ID {
        nid(NodeType::IssueType, self.0.id)
    }
    pub async fn name(&self) -> &str {
        &self.0.name
    }
    pub async fn description(&self) -> Option<&str> {
        self.0.description.as_deref()
    }
    /// Gray when unset, like GitHub.
    pub async fn color(&self) -> IssueTypeColor {
        self.0
            .color
            .as_deref()
            .and_then(IssueTypeColor::from_db)
            .unwrap_or(IssueTypeColor::Gray)
    }
    pub async fn is_enabled(&self) -> bool {
        self.0.is_enabled
    }
}

/// Summary of the state of an issue's dependencies.
#[derive(SimpleObject, Clone, Copy, Default)]
pub struct IssueDependenciesSummary {
    /// Count of issues this issue is blocked by (open).
    pub blocked_by: i32,
    /// Count of issues this issue is blocking (open).
    pub blocking: i32,
    pub total_blocked_by: i32,
    pub total_blocking: i32,
}

impl From<Summary> for IssueDependenciesSummary {
    fn from(s: Summary) -> Self {
        Self {
            blocked_by: s.blocked_by as i32,
            blocking: s.blocking as i32,
            total_blocked_by: s.total_blocked_by as i32,
            total_blocking: s.total_blocking as i32,
        }
    }
}

/// Issue id → its issue type.
pub struct IssueTypeLoader(pub AppState);

impl Loader<i64> for IssueTypeLoader {
    type Value = Arc<IssueTypeRow>;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> Result<HashMap<i64, Self::Value>, Self::Error> {
        Ok(bgh_issues::issue_types::for_issues(&self.0.db, keys)
            .await
            .map_err(Arc::new)?
            .into_iter()
            .map(|(k, v)| (k, Arc::new(v)))
            .collect())
    }
}

/// Issue id → dependency summary.
pub struct DependencySummaryLoader(pub AppState);

impl Loader<i64> for DependencySummaryLoader {
    type Value = Summary;
    type Error = Arc<ApiError>;

    async fn load(&self, keys: &[i64]) -> Result<HashMap<i64, Self::Value>, Self::Error> {
        bgh_issues::dependencies::summaries(&self.0.db, keys)
            .await
            .map_err(Arc::new)
    }
}

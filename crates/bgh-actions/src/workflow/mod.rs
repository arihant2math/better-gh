//! GitHub Actions workflow-file model, parser and evaluation helpers.
//!
//! * [`parse_workflow`] turns a workflow YAML file into a validated
//!   [`Workflow`] (see `docs.github.com` "Workflow syntax for GitHub Actions").
//! * [`Triggers`] answers "does this event trigger the workflow?" (`on.rs`).
//! * [`filters`] implements the branch/tag/path filter pattern language.
//! * [`CronSchedule`] parses and evaluates POSIX cron expressions (UTC).
//! * [`expand_matrix`] expands an evaluated `strategy.matrix` object.
//!
//! Values that may contain `${{ }}` expressions are preserved raw; they are
//! evaluated elsewhere.

pub mod cron;
pub mod filters;
pub mod matrix;
pub mod model;
pub mod on;
mod parse;

#[cfg(test)]
mod tests;

pub use cron::CronSchedule;
pub use filters::{filter_matches, glob_match};
pub use matrix::{MAX_MATRIX_COMBINATIONS, expand_matrix};
pub use model::{
    Concurrency, Container, Defaults, EventTrigger, InputDef, Job, Permissions, RunDefaults, Step,
    Strategy, Triggers, Workflow,
};
pub use on::KNOWN_EVENTS;
pub use parse::parse_workflow;

use serde::{Deserialize, Serialize};

/// Error produced while parsing or validating a workflow (or one of its parts).
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkflowError {
    /// The document is not valid YAML.
    #[error("invalid YAML: {0}")]
    Yaml(String),
    /// The document is valid YAML but not a valid workflow.
    #[error("invalid workflow: {0}")]
    Invalid(String),
}

impl WorkflowError {
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        WorkflowError::Invalid(msg.into())
    }
}

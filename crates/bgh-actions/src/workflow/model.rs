//! Data model of a parsed workflow file. Every type is plain data
//! (Serialize + Deserialize) so it can be stored as JSON.

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A parsed, validated workflow file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workflow {
    pub name: Option<String>,
    pub run_name: Option<String>,
    pub on: Triggers,
    pub env: IndexMap<String, String>,
    pub defaults: Option<Defaults>,
    pub concurrency: Option<Concurrency>,
    pub permissions: Option<Permissions>,
    pub jobs: IndexMap<String, Job>,
}

/// The `on:` section: event name (lowercased) -> filters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Triggers {
    pub events: IndexMap<String, EventTrigger>,
}

/// Generic per-event filter set. Fields that an event does not use stay
/// empty / `None`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EventTrigger {
    pub types: Vec<String>,
    pub branches: Option<Vec<String>>,
    pub branches_ignore: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    pub tags_ignore: Option<Vec<String>>,
    pub paths: Option<Vec<String>>,
    pub paths_ignore: Option<Vec<String>>,
    /// `workflow_dispatch` / `workflow_call` inputs.
    pub inputs: IndexMap<String, InputDef>,
    /// `workflow_call` outputs (raw).
    pub outputs: IndexMap<String, Value>,
    /// `workflow_call` secrets (raw).
    pub secrets: IndexMap<String, Value>,
    /// `schedule` cron expressions, in declaration order.
    pub crons: Vec<String>,
    /// `workflow_run` workflow names (raw).
    pub workflows: Vec<String>,
}

/// A `workflow_dispatch` / `workflow_call` input definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputDef {
    pub description: Option<String>,
    pub required: bool,
    /// Default value, stringified (`true`, `3`, ...).
    pub default: Option<String>,
    /// `string` (default), `boolean`, `choice`, `number` or `environment`.
    pub r#type: String,
    /// Options of a `choice` input.
    pub options: Vec<String>,
}

impl Default for InputDef {
    fn default() -> Self {
        InputDef {
            description: None,
            required: false,
            default: None,
            r#type: "string".to_string(),
            options: Vec::new(),
        }
    }
}

/// A job. Either a "step job" (`runs-on` + `steps`) or a reusable workflow
/// call (`uses` + optional `with` / `secrets`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub name: Option<String>,
    pub needs: Vec<String>,
    pub r#if: Option<String>,
    /// String, array or `{group, labels}`; raw (may contain expressions).
    /// `Value::Null` for reusable-workflow jobs.
    pub runs_on: Value,
    /// String or `{name, url}`; raw.
    pub environment: Option<Value>,
    pub permissions: Option<Permissions>,
    pub concurrency: Option<Concurrency>,
    pub outputs: IndexMap<String, String>,
    pub env: IndexMap<String, String>,
    pub defaults: Option<Defaults>,
    /// Number or expression string; the caller applies the default of 360.
    pub timeout_minutes: Option<Value>,
    /// Bool or expression string.
    pub continue_on_error: Option<Value>,
    pub strategy: Option<Strategy>,
    pub container: Option<Container>,
    pub services: IndexMap<String, Container>,
    pub steps: Vec<Step>,
    /// Reusable workflow reference (`owner/repo/.github/workflows/x.yml@ref`
    /// or `./.github/workflows/x.yml`).
    pub uses: Option<String>,
    /// Inputs of the called workflow; raw YAML values (types are checked
    /// against `on.workflow_call.inputs`), strings may hold expressions.
    pub with: IndexMap<String, Value>,
    /// `"inherit"` or a mapping; raw.
    pub secrets: Option<Value>,
}

impl Job {
    /// True when the job calls a reusable workflow instead of running steps.
    pub fn is_reusable_call(&self) -> bool {
        self.uses.is_some()
    }
}

/// `strategy:`. All values raw (may contain expressions).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Strategy {
    /// Mapping or expression string.
    pub matrix: Option<Value>,
    /// Bool or expression string; GitHub's default is `true`.
    pub fail_fast: Option<Value>,
    /// Number or expression string.
    pub max_parallel: Option<Value>,
}

/// `container:` or a `services.<id>` entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Container {
    pub image: String,
    /// `{username, password}`; raw.
    pub credentials: Option<Value>,
    pub env: IndexMap<String, String>,
    pub ports: Vec<String>,
    pub volumes: Vec<String>,
    pub options: Option<String>,
}

/// A step of a job: exactly one of `run` / `uses` is set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Step {
    pub id: Option<String>,
    pub name: Option<String>,
    pub r#if: Option<String>,
    pub uses: Option<String>,
    pub run: Option<String>,
    pub shell: Option<String>,
    pub working_directory: Option<String>,
    pub with: IndexMap<String, String>,
    pub env: IndexMap<String, String>,
    /// Bool or expression string.
    pub continue_on_error: Option<Value>,
    /// Number or expression string.
    pub timeout_minutes: Option<Value>,
}

/// `defaults:` (workflow or job level).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    pub run: Option<RunDefaults>,
}

/// `defaults.run`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunDefaults {
    pub shell: Option<String>,
    pub working_directory: Option<String>,
}

/// `concurrency:`. The string form only sets `group`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Concurrency {
    pub group: String,
    /// Bool or expression string.
    pub cancel_in_progress: Option<Value>,
}

/// `permissions:`. Serializes like GitHub: `"read-all"`, `"write-all"` or a
/// `{scope: "read" | "write" | "none"}` map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Permissions {
    ReadAll,
    WriteAll,
    #[serde(untagged)]
    Map(IndexMap<String, String>),
}

impl Workflow {
    /// Jobs in a valid topological order of `needs`. Stable: at every point
    /// the earliest-declared job whose dependencies are all satisfied is
    /// emitted next, so a workflow that is already ordered keeps its order.
    /// Unknown `needs` are ignored; jobs stuck in a cycle (impossible after
    /// [`crate::workflow::parse_workflow`]) are appended in declaration order.
    pub fn job_order(&self) -> Vec<String> {
        let mut done: HashSet<&str> = HashSet::new();
        let mut order = Vec::with_capacity(self.jobs.len());
        loop {
            let next = self.jobs.iter().find(|(id, job)| {
                !done.contains(id.as_str())
                    && job
                        .needs
                        .iter()
                        .all(|n| done.contains(n.as_str()) || !self.jobs.contains_key(n))
            });
            match next {
                Some((id, _)) => {
                    done.insert(id.as_str());
                    order.push(id.clone());
                }
                None => break,
            }
        }
        for id in self.jobs.keys() {
            if !done.contains(id.as_str()) {
                order.push(id.clone());
            }
        }
        order
    }

    /// Finds a dependency cycle among jobs, returned as a path whose first
    /// and last element are the same job (`["a", "b", "a"]`).
    pub fn find_needs_cycle(&self) -> Option<Vec<String>> {
        // 0 = unvisited, 1 = on stack, 2 = done
        let mut state: HashMap<&str, u8> = HashMap::new();
        let mut stack: Vec<&str> = Vec::new();

        fn visit<'a>(
            wf: &'a Workflow,
            id: &'a str,
            state: &mut HashMap<&'a str, u8>,
            stack: &mut Vec<&'a str>,
        ) -> Option<Vec<String>> {
            match state.get(id).copied().unwrap_or(0) {
                2 => return None,
                1 => {
                    let start = stack.iter().position(|s| *s == id).unwrap_or(0);
                    let mut cycle: Vec<String> =
                        stack[start..].iter().map(|s| s.to_string()).collect();
                    cycle.push(id.to_string());
                    return Some(cycle);
                }
                _ => {}
            }
            state.insert(id, 1);
            stack.push(id);
            if let Some(job) = wf.jobs.get(id) {
                for need in &job.needs {
                    if let Some((key, _)) = wf.jobs.get_key_value(need.as_str())
                        && let Some(c) = visit(wf, key.as_str(), state, stack)
                    {
                        return Some(c);
                    }
                }
            }
            stack.pop();
            state.insert(id, 2);
            None
        }

        for id in self.jobs.keys() {
            if let Some(c) = visit(self, id.as_str(), &mut state, &mut stack) {
                return Some(c);
            }
        }
        None
    }
}

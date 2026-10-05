//! Job orchestration: set up, steps, post steps, completion.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::Utc;
use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use tokio::sync::Notify;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::commands::{parse_command, parse_file_commands, split_words};
use super::context::EvalContext;
use super::executor::{self, Exec, Paths};
use super::logger::Logger;
use super::mask::Masker;
use super::process::{ProcessOutcome, ProcessSpec, run_process};
use super::{ExecutorKind, RunnerConfig};
use crate::expr::{self, JobStatus};
use crate::protocol::{Annotation, Backend, JobCompletion, JobSpec, Step, StepState};

pub(super) type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// After a job was cancelled, `always()` steps get this long before they
/// are killed too.
const CANCEL_GRACE: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Conclusion {
    Success,
    Failure,
    Cancelled,
    Skipped,
}

impl Conclusion {
    pub fn as_str(self) -> &'static str {
        match self {
            Conclusion::Success => "success",
            Conclusion::Failure => "failure",
            Conclusion::Cancelled => "cancelled",
            Conclusion::Skipped => "skipped",
        }
    }
}

/// Per-step file-command files.
#[derive(Debug, Clone)]
pub(super) struct StepFiles {
    pub host: [PathBuf; 5],
    pub guest: [String; 5],
}

pub(super) const F_OUTPUT: usize = 0;
pub(super) const F_ENV: usize = 1;
pub(super) const F_PATH: usize = 2;
pub(super) const F_SUMMARY: usize = 3;
pub(super) const F_STATE: usize = 4;
const FILE_KINDS: [&str; 5] = [
    "set_output",
    "set_env",
    "add_path",
    "step_summary",
    "save_state",
];

impl StepFiles {
    fn create(paths: &Paths) -> std::io::Result<StepFiles> {
        let dir = "_temp/_runner_file_commands";
        std::fs::create_dir_all(paths.host(dir))?;
        let id = uuid::Uuid::new_v4();
        let mut host: [PathBuf; 5] = Default::default();
        let mut guest: [String; 5] = Default::default();
        for (i, kind) in FILE_KINDS.iter().enumerate() {
            let rel = format!("{dir}/{kind}_{id}");
            std::fs::write(paths.host(&rel), b"")?;
            host[i] = paths.host(&rel);
            guest[i] = paths.guest(&rel);
        }
        Ok(StepFiles { host, guest })
    }

    fn take(&self, i: usize) -> String {
        let s = std::fs::read_to_string(&self.host[i]).unwrap_or_default();
        let _ = std::fs::write(&self.host[i], b"");
        s
    }
}

/// Evaluation scope: the job itself or a composite action.
pub(super) struct Scope {
    pub inputs: Value,
    pub steps: Map<String, Value>,
    /// Env inherited from the enclosing composite step (empty at job level).
    pub env: IndexMap<String, String>,
    pub failed: bool,
    pub action_path: Option<String>,
    pub action_repository: Option<String>,
    pub depth: usize,
}

pub(super) struct StepResult {
    pub conclusion: Conclusion,
}

/// Outcome of a step's body (the `run` script or the action).
#[derive(Default)]
pub(super) struct Body {
    pub status: Option<Conclusion>,
    pub timed_out: bool,
    pub outputs: IndexMap<String, String>,
    pub state: IndexMap<String, String>,
    pub post: Option<PostStep>,
}

impl Body {
    pub fn success() -> Body {
        Body {
            status: Some(Conclusion::Success),
            ..Default::default()
        }
    }
    pub fn failure() -> Body {
        Body {
            status: Some(Conclusion::Failure),
            ..Default::default()
        }
    }
    pub fn from_outcome(o: &ProcessOutcome) -> Body {
        let mut b = Body::default();
        match o {
            ProcessOutcome::Exited(0) => b.status = Some(Conclusion::Success),
            ProcessOutcome::Exited(_) | ProcessOutcome::Failed(_) => {
                b.status = Some(Conclusion::Failure)
            }
            ProcessOutcome::Cancelled => b.status = Some(Conclusion::Cancelled),
            ProcessOutcome::TimedOut => {
                b.status = Some(Conclusion::Failure);
                b.timed_out = true;
            }
        }
        b
    }
}

/// A `post:` script of an action, run after all steps.
pub(super) struct PostStep {
    pub name: String,
    pub cond: String,
    pub kind: PostKind,
    /// Extra env (INPUT_*, GITHUB_ACTION_PATH, ...).
    pub env: IndexMap<String, String>,
    pub state: IndexMap<String, String>,
    pub action_name: String,
    pub action_path: Option<String>,
    pub action_repository: Option<String>,
    pub inputs: Value,
}

pub(super) enum PostKind {
    Node {
        script: String,
    },
    Docker {
        image: String,
        entrypoint: Option<String>,
        args: Vec<String>,
    },
}

/// Everything about the step currently executing.
pub(super) struct StepRun {
    pub display: String,
    pub action_name: String,
    /// Env without the step's own `env:`.
    pub job_env: IndexMap<String, String>,
    pub step_env: IndexMap<String, String>,
    pub files: StepFiles,
    pub log_step: i64,
    pub deadline: Option<Instant>,
    pub cancel: CancellationToken,
}

impl StepRun {
    /// job env + step env.
    pub fn env(&self) -> IndexMap<String, String> {
        let mut e = self.job_env.clone();
        e.extend(self.step_env.clone());
        e
    }
}

/// Step states shared with the heartbeat task.
pub(super) struct Reporter {
    steps: std::sync::Mutex<Vec<StepState>>,
    notify: Notify,
}

impl Reporter {
    fn update(&self, f: impl FnOnce(&mut Vec<StepState>)) {
        {
            let mut s = self.steps.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut s);
        }
        self.notify.notify_one();
    }

    fn snapshot(&self) -> Vec<StepState> {
        self.steps.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn start(&self, number: i64, name: &str) {
        self.update(|s| {
            if let Some(st) = s.iter_mut().find(|s| s.number == number) {
                st.name = name.to_string();
                st.status = "in_progress".into();
                st.started_at = Some(Utc::now());
            }
        });
    }

    fn finish(&self, number: i64, name: Option<&str>, c: Conclusion) {
        self.update(|s| {
            if let Some(st) = s.iter_mut().find(|s| s.number == number) {
                if let Some(n) = name {
                    st.name = n.to_string();
                }
                let now = Utc::now();
                st.status = "completed".into();
                st.conclusion = Some(c.as_str().into());
                st.started_at.get_or_insert(now);
                st.completed_at = Some(now);
            }
        });
    }

    /// Insert a step before the last one ("Complete job"); returns its number.
    fn insert_before_last(&self, name: &str) -> i64 {
        let mut number = 0;
        self.update(|s| {
            let last = s.pop();
            number = s.len() as i64 + 1;
            s.push(queued(number, name));
            if let Some(mut l) = last {
                l.number = number + 1;
                s.push(l);
            }
        });
        number
    }

    fn last_number(&self) -> i64 {
        self.steps.lock().unwrap_or_else(|e| e.into_inner()).len() as i64
    }
}

fn queued(number: i64, name: &str) -> StepState {
    StepState {
        number,
        name: name.to_string(),
        status: "queued".into(),
        conclusion: None,
        started_at: None,
        completed_at: None,
    }
}

/// Collects state from workflow commands in a process's output.
pub(super) struct LineState {
    log_step: i64,
    stop_token: Option<String>,
    echo: bool,
    debug: bool,
    pub outputs: IndexMap<String, String>,
    pub state: IndexMap<String, String>,
    pub annotations: Vec<Annotation>,
    pub path_adds: Vec<String>,
}

impl LineState {
    pub fn new(log_step: i64, debug: bool) -> Self {
        LineState {
            log_step,
            stop_token: None,
            echo: false,
            debug,
            outputs: IndexMap::new(),
            state: IndexMap::new(),
            annotations: Vec::new(),
            path_adds: Vec::new(),
        }
    }

    pub fn handle(&mut self, line: &str, logger: &Logger) {
        let step = self.log_step;
        if let Some(tok) = &self.stop_token {
            if line.trim() == format!("::{tok}::") {
                self.stop_token = None;
            } else {
                logger.log(step, line);
            }
            return;
        }
        let Some(cmd) = parse_command(line) else {
            logger.log(step, line);
            return;
        };
        let echo = |logger: &Logger| logger.log(step, line);
        match cmd.name.as_str() {
            "add-mask" => {
                logger.masker().add(&cmd.data);
                if self.echo {
                    logger.log(step, "::add-mask::***");
                }
            }
            "set-output" => {
                if self.echo {
                    echo(logger);
                }
                if let Some(name) = cmd.props.get("name") {
                    self.outputs.insert(name.clone(), cmd.data);
                }
            }
            "save-state" => {
                if self.echo {
                    echo(logger);
                }
                if let Some(name) = cmd.props.get("name") {
                    self.state.insert(name.clone(), cmd.data);
                }
            }
            "add-path" => {
                if self.echo {
                    echo(logger);
                }
                self.path_adds.push(cmd.data);
            }
            "debug" => {
                if self.debug {
                    logger.log(step, &format!("##[debug]{}", cmd.data));
                }
            }
            "group" => logger.log(step, &format!("##[group]{}", cmd.data)),
            "endgroup" => logger.log(step, "##[endgroup]"),
            "echo" => match cmd.data.trim() {
                "on" => self.echo = true,
                "off" => self.echo = false,
                _ => logger.log(step, line),
            },
            "stop-commands" => {
                if self.echo {
                    echo(logger);
                }
                if !cmd.data.is_empty() {
                    self.stop_token = Some(cmd.data);
                }
            }
            "error" | "warning" | "notice" => {
                if self.echo {
                    echo(logger);
                }
                logger.log(step, &format!("##[{}]{}", cmd.name, cmd.data));
                let num = |k: &str| cmd.props.get(k).and_then(|v| v.trim().parse::<i64>().ok());
                let start_line = num("line");
                self.annotations.push(Annotation {
                    level: if cmd.name == "error" {
                        "failure".into()
                    } else {
                        cmd.name.clone()
                    },
                    message: logger.masker().mask(&cmd.data),
                    title: cmd.props.get("title").map(|t| logger.masker().mask(t)),
                    path: cmd.props.get("file").cloned(),
                    start_line,
                    end_line: num("endLine").or(start_line),
                    start_column: num("col"),
                    end_column: num("endColumn"),
                });
            }
            _ => logger.log(step, line),
        }
    }
}

pub(super) struct JobRunner {
    pub backend: Arc<dyn Backend>,
    pub cfg: Arc<RunnerConfig>,
    pub spec: JobSpec,
    pub logger: Arc<Logger>,
    pub masker: Arc<Masker>,
    pub paths: Paths,
    pub exec: Option<Exec>,
    pub cancel: CancellationToken,
    pub force_cancel: CancellationToken,
    pub timed_out: Arc<AtomicBool>,
    pub reporter: Arc<Reporter>,
    pub github_base: Map<String, Value>,
    pub github_env: IndexMap<String, String>,
    /// Most recent first.
    pub path_adds: Vec<String>,
    pub job_failed: bool,
    pub annotations: Vec<Annotation>,
    pub summary: String,
    pub posts: Vec<PostStep>,
    pub debug: bool,
    action_counts: HashMap<String, usize>,
}

/// Run a job to completion.
pub(super) async fn run(
    backend: Arc<dyn Backend>,
    cfg: Arc<RunnerConfig>,
    spec: JobSpec,
    external_cancel: CancellationToken,
) -> JobCompletion {
    let masker = Arc::new(Masker::new());
    for v in spec.secrets.values() {
        masker.add(v);
    }
    if !spec.token.is_empty() {
        masker.add(&spec.token);
        masker.add(&basic_auth(&spec.token));
    }
    let logger = Arc::new(Logger::new(backend.clone(), spec.job_id, masker.clone()));

    let work_dir = std::path::absolute(&cfg.work_dir).unwrap_or_else(|_| cfg.work_dir.clone());
    let host_root = work_dir.join(spec.job_id.to_string());
    let guest_root = match cfg.executor {
        ExecutorKind::Docker => executor::CONTAINER_ROOT.to_string(),
        ExecutorKind::Shell => host_root.to_string_lossy().into_owned(),
    };
    let repo_name = spec
        .repository
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("repo")
        .to_string();
    let paths = Paths {
        host_root,
        guest_root,
        repo_name,
    };

    // Step list: Set up job, steps, Complete job.
    let mut states = vec![queued(1, "Set up job")];
    let reporter = Arc::new(Reporter {
        steps: std::sync::Mutex::new(Vec::new()),
        notify: Notify::new(),
    });
    let cancel = external_cancel.child_token();
    let debug = spec
        .secrets
        .get("ACTIONS_STEP_DEBUG")
        .map(|s| s == "true")
        .unwrap_or(false)
        || spec.vars.get("ACTIONS_STEP_DEBUG").and_then(Value::as_str) == Some("true");

    let mut runner = JobRunner {
        backend: backend.clone(),
        cfg: cfg.clone(),
        logger: logger.clone(),
        masker,
        paths,
        exec: None,
        cancel: cancel.clone(),
        force_cancel: CancellationToken::new(),
        timed_out: Arc::new(AtomicBool::new(false)),
        reporter: reporter.clone(),
        github_base: Map::new(),
        github_env: IndexMap::new(),
        path_adds: Vec::new(),
        job_failed: false,
        annotations: Vec::new(),
        summary: String::new(),
        posts: Vec::new(),
        debug,
        action_counts: HashMap::new(),
        spec,
    };
    runner.github_base = runner.build_github_base();
    {
        let scope = runner.top_scope();
        let job_env = runner.job_env(&scope);
        let ctx = runner.eval_ctx(&scope, &job_env, "", None);
        for (i, step) in runner.spec.steps.iter().enumerate() {
            states.push(queued(i as i64 + 2, &display_name(step, &ctx)));
        }
    }
    states.push(queued(states.len() as i64 + 1, "Complete job"));
    reporter.update(|s| *s = states);

    // Heartbeat.
    let hb_stop = CancellationToken::new();
    let heartbeat = {
        let backend = backend.clone();
        let reporter = reporter.clone();
        let stop = hb_stop.clone();
        let cancel = cancel.clone();
        let job_id = runner.spec.job_id;
        let interval = cfg.heartbeat_interval;
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = reporter.notify.notified() => {}
                    _ = tokio::time::sleep(interval) => {}
                    _ = stop.cancelled() => return,
                }
                let snapshot = reporter.snapshot();
                match backend.update_steps(job_id, &snapshot).await {
                    Ok(hb) if hb.cancel => cancel.cancel(),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(job_id, "step update failed: {e:#}"),
                }
            }
        })
    };
    // Job timeout and cancellation grace.
    let timeout = cfg.job_timeout.unwrap_or_else(|| {
        let m = if runner.spec.timeout_minutes == 0 {
            360
        } else {
            runner.spec.timeout_minutes
        };
        Duration::from_secs(m * 60)
    });
    let watchdog = {
        let cancel = cancel.clone();
        let force = runner.force_cancel.clone();
        let timed_out = runner.timed_out.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(timeout) => {
                    timed_out.store(true, Ordering::SeqCst);
                    cancel.cancel();
                }
                _ = cancel.cancelled() => {}
            }
            tokio::time::sleep(CANCEL_GRACE).await;
            force.cancel();
        })
    };

    let completion = runner.run_all(timeout).await;

    watchdog.abort();
    hb_stop.cancel();
    let _ = heartbeat.await;
    logger.close().await;
    if let Err(e) = backend
        .update_steps(runner.spec.job_id, &completion.steps)
        .await
    {
        tracing::warn!(
            job_id = runner.spec.job_id,
            "final step update failed: {e:#}"
        );
    }
    completion
}

pub(super) fn basic_auth(token: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}"))
}

/// Step display name: `name`, else `Run <first line of run>` / `Run <uses>`.
pub(super) fn display_name(step: &Step, ctx: &dyn expr::Context) -> String {
    if let Some(n) = step.name.as_deref().filter(|n| !n.trim().is_empty()) {
        return expr::interpolate(n, ctx).unwrap_or_else(|_| n.to_string());
    }
    if let Some(run) = &step.run {
        let first = run
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        let first = expr::interpolate(first, ctx).unwrap_or_else(|_| first.to_string());
        return format!("Run {first}");
    }
    if let Some(u) = &step.uses {
        return format!("Run {u}");
    }
    "Run".to_string()
}

fn value_str(v: &Value) -> String {
    expr::to_display_string(v)
}

impl JobRunner {
    fn build_github_base(&self) -> Map<String, Value> {
        let s = &self.spec;
        let mut g = s.github.as_object().cloned().unwrap_or_default();
        let mut def = |k: &str, v: Value| {
            let missing = match g.get(k) {
                None | Some(Value::Null) => true,
                Some(Value::String(x)) => x.is_empty(),
                _ => false,
            };
            if missing {
                g.insert(k.to_string(), v);
            }
        };
        def("repository", json!(s.repository));
        def("repository_id", json!(s.repository_id.to_string()));
        def("repository_owner", json!(s.repository_owner));
        def("server_url", json!(s.server_url));
        def("api_url", json!(s.api_url));
        def("graphql_url", json!(format!("{}/graphql", s.api_url)));
        def("run_id", json!(s.run_id.to_string()));
        def("run_number", json!(s.run_number.to_string()));
        def("run_attempt", json!(s.run_attempt.to_string()));
        def("job", json!(s.job_key));
        def("workflow", json!(s.workflow_name));
        def("retention_days", json!("90"));
        g.insert("token".into(), json!(s.token));
        g.insert("workspace".into(), json!(self.paths.guest_workspace()));
        g.insert(
            "event_path".into(),
            json!(self.paths.guest("_temp/_github_workflow/event.json")),
        );
        g
    }

    pub(super) fn top_scope(&self) -> Scope {
        Scope {
            inputs: self.spec.inputs.clone(),
            steps: Map::new(),
            env: IndexMap::new(),
            failed: false,
            action_path: None,
            action_repository: None,
            depth: 0,
        }
    }

    pub(super) fn status(&self, scope: &Scope) -> JobStatus {
        if self.cancel.is_cancelled() {
            JobStatus::Cancelled
        } else if self.job_failed || scope.failed {
            JobStatus::Failure
        } else {
            JobStatus::Success
        }
    }

    /// Workflow/job env + GITHUB_ENV + composite env.
    pub(super) fn job_env(&self, scope: &Scope) -> IndexMap<String, String> {
        let mut e = self.spec.env.clone();
        e.extend(self.github_env.clone());
        e.extend(scope.env.clone());
        e
    }

    pub(super) fn eval_ctx(
        &self,
        scope: &Scope,
        env: &IndexMap<String, String>,
        action_name: &str,
        files: Option<&StepFiles>,
    ) -> EvalContext {
        let mut github = self.github_base.clone();
        github.insert("action".into(), json!(action_name));
        github.insert(
            "action_path".into(),
            json!(scope.action_path.clone().unwrap_or_default()),
        );
        github.insert(
            "action_repository".into(),
            json!(scope.action_repository.clone().unwrap_or_default()),
        );
        if let Some(f) = files {
            github.insert("path".into(), json!(f.guest[F_PATH]));
            github.insert("env".into(), json!(f.guest[F_ENV]));
            github.insert("output".into(), json!(f.guest[F_OUTPUT]));
            github.insert("state".into(), json!(f.guest[F_STATE]));
            github.insert("step_summary".into(), json!(f.guest[F_SUMMARY]));
        }
        let mut secrets: Map<String, Value> = self
            .spec
            .secrets
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect();
        secrets.insert("GITHUB_TOKEN".into(), json!(self.spec.token));
        let status = self.status(scope);
        let job = json!({
            "status": match status {
                JobStatus::Success => "success",
                JobStatus::Failure => "failure",
                JobStatus::Cancelled => "cancelled",
            },
            "container": self.exec.as_ref().map(|e| e.container_context()).unwrap_or(json!({"id": "", "network": ""})),
            "services": self.exec.as_ref().map(|e| Value::Object(e.services.clone())).unwrap_or(json!({})),
        });
        let mut c = Map::new();
        c.insert("github".into(), Value::Object(github));
        c.insert(
            "env".into(),
            Value::Object(env.iter().map(|(k, v)| (k.clone(), json!(v))).collect()),
        );
        c.insert("vars".into(), self.spec.vars.clone());
        c.insert("secrets".into(), Value::Object(secrets));
        c.insert("matrix".into(), self.spec.matrix.clone());
        c.insert("needs".into(), self.spec.needs.clone());
        c.insert("strategy".into(), self.spec.strategy.clone());
        c.insert("inputs".into(), scope.inputs.clone());
        c.insert("steps".into(), Value::Object(scope.steps.clone()));
        c.insert("runner".into(), self.runner_context());
        c.insert("job".into(), job);
        EvalContext {
            contexts: c,
            status,
            host_workspace: self.paths.host_workspace(),
            guest_workspace: self.paths.guest_workspace(),
        }
    }

    fn runner_context(&self) -> Value {
        json!({
            "name": self.cfg.name,
            "os": "Linux",
            "arch": "X64",
            "temp": self.paths.guest_temp(),
            "tool_cache": self.paths.guest("_tool"),
            "workspace": self.paths.guest_runner_workspace(),
            "debug": if self.debug { "1" } else { "" },
            "environment": "self-hosted",
        })
    }

    /// CI / GITHUB_* / RUNNER_* variables of a step.
    pub(super) fn base_env(
        &self,
        scope: &Scope,
        action_name: &str,
        files: &StepFiles,
    ) -> IndexMap<String, String> {
        let mut e = IndexMap::new();
        e.insert("CI".to_string(), "true".to_string());
        e.insert("GITHUB_ACTIONS".to_string(), "true".to_string());
        let g = &self.github_base;
        let gs = |k: &str| g.get(k).map(value_str).unwrap_or_default();
        for k in [
            "actor",
            "actor_id",
            "api_url",
            "base_ref",
            "event_name",
            "graphql_url",
            "head_ref",
            "job",
            "ref",
            "ref_name",
            "ref_protected",
            "ref_type",
            "repository",
            "repository_id",
            "repository_owner",
            "repository_owner_id",
            "retention_days",
            "run_attempt",
            "run_id",
            "run_number",
            "server_url",
            "sha",
            "triggering_actor",
            "workflow",
            "workflow_ref",
            "workflow_sha",
        ] {
            let mut v = gs(k);
            if k == "ref_protected" && v.is_empty() {
                v = "false".into();
            }
            if k == "triggering_actor" && v.is_empty() {
                v = gs("actor");
            }
            e.insert(format!("GITHUB_{}", k.to_ascii_uppercase()), v);
        }
        e.insert(
            "GITHUB_EVENT_PATH".into(),
            self.paths.guest("_temp/_github_workflow/event.json"),
        );
        e.insert("GITHUB_WORKSPACE".into(), self.paths.guest_workspace());
        e.insert("GITHUB_ACTION".into(), action_name.to_string());
        if let Some(p) = &scope.action_path {
            e.insert("GITHUB_ACTION_PATH".into(), p.clone());
        }
        if let Some(r) = &scope.action_repository {
            e.insert("GITHUB_ACTION_REPOSITORY".into(), r.clone());
        }
        e.insert("GITHUB_OUTPUT".into(), files.guest[F_OUTPUT].clone());
        e.insert("GITHUB_ENV".into(), files.guest[F_ENV].clone());
        e.insert("GITHUB_PATH".into(), files.guest[F_PATH].clone());
        e.insert("GITHUB_STEP_SUMMARY".into(), files.guest[F_SUMMARY].clone());
        e.insert("GITHUB_STATE".into(), files.guest[F_STATE].clone());
        e.insert("RUNNER_NAME".into(), self.cfg.name.clone());
        e.insert("RUNNER_OS".into(), "Linux".into());
        e.insert("RUNNER_ARCH".into(), "X64".into());
        e.insert("RUNNER_TEMP".into(), self.paths.guest_temp());
        e.insert("RUNNER_TOOL_CACHE".into(), self.paths.guest("_tool"));
        e.insert(
            "RUNNER_WORKSPACE".into(),
            self.paths.guest_runner_workspace(),
        );
        e.insert("RUNNER_ENVIRONMENT".into(), "self-hosted".into());
        if self.debug {
            e.insert("RUNNER_DEBUG".into(), "1".into());
        }
        e
    }

    /// Full process environment of a step: base, job env, step env, extra,
    /// PATH with GITHUB_PATH additions.
    pub(super) fn process_env(
        &self,
        scope: &Scope,
        run: &StepRun,
        extra: &IndexMap<String, String>,
    ) -> IndexMap<String, String> {
        let mut e = self.base_env(scope, &run.action_name, &run.files);
        e.extend(run.job_env.clone());
        e.extend(run.step_env.clone());
        e.extend(extra.clone());
        if !self.path_adds.is_empty() {
            let base = e.get("PATH").cloned().unwrap_or_else(|| {
                self.exec
                    .as_ref()
                    .map(|x| x.default_path.clone())
                    .unwrap_or_default()
            });
            let mut p = self.path_adds.join(":");
            if !base.is_empty() {
                p.push(':');
                p.push_str(&base);
            }
            e.insert("PATH".into(), p);
        }
        e
    }

    pub(super) fn step_cancel_token(&self) -> CancellationToken {
        if self.cancel.is_cancelled() {
            self.force_cancel.clone()
        } else {
            self.cancel.clone()
        }
    }

    fn action_name(&mut self, step: &Step) -> String {
        if let Some(id) = step.id.as_deref().filter(|s| !s.is_empty()) {
            return id.to_string();
        }
        let base = match &step.uses {
            Some(u) => {
                let repo = u.split('@').next().unwrap_or(u);
                let s: String = repo
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect();
                format!("__{}", s.trim_matches('_'))
            }
            None => "__run".to_string(),
        };
        let n = self.action_counts.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 { base } else { format!("{base}_{n}") }
    }

    pub(super) fn log(&self, step: i64, text: &str) {
        self.logger.log(step, text);
    }

    /// Run a process in the job environment, handling workflow commands.
    pub(super) async fn exec_process(
        &mut self,
        spec: &ProcessSpec,
        log_step: i64,
        deadline: Option<Instant>,
        cancel: &CancellationToken,
    ) -> (ProcessOutcome, LineState) {
        let mut ls = LineState::new(log_step, self.debug);
        let logger = self.logger.clone();
        let outcome = {
            let mut on_line = |l: &str| ls.handle(l, &logger);
            run_process(spec, &mut on_line, cancel, deadline).await
        };
        match &outcome {
            ProcessOutcome::Exited(0) => {}
            ProcessOutcome::Exited(code) => {
                self.log(
                    log_step,
                    &format!("##[error]Process completed with exit code {code}."),
                );
            }
            ProcessOutcome::Cancelled => self.log(log_step, "##[error]The operation was canceled."),
            ProcessOutcome::TimedOut => {}
            ProcessOutcome::Failed(msg) => self.log(log_step, &format!("##[error]{msg}")),
        }
        self.annotations.append(&mut ls.annotations);
        for p in ls.path_adds.drain(..) {
            self.path_adds.insert(0, p);
        }
        (outcome, ls)
    }

    /// Run a script with a shell (a `run:` step or composite `run`).
    pub(super) async fn run_script(
        &mut self,
        scope: &Scope,
        run: &StepRun,
        script: &str,
        shell: Option<&str>,
        working_directory: Option<&str>,
    ) -> Body {
        let has_bash = self.exec.as_ref().map(|e| e.has_bash).unwrap_or(false);
        let (tpl, ext) = match shell_template(shell, has_bash) {
            Ok(t) => t,
            Err(e) => {
                self.log(run.log_step, &format!("##[error]{e}"));
                return Body::failure();
            }
        };
        // Header.
        let first = script
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        let mut header = format!("##[group]Run {first}\n");
        for l in script.trim_end_matches('\n').lines() {
            header.push_str(l);
            header.push('\n');
        }
        header.push_str(&format!("shell: {tpl}\n"));
        let user_env = run.env();
        if !user_env.is_empty() {
            header.push_str("env:\n");
            for (k, v) in &user_env {
                header.push_str(&format!("  {k}: {v}\n"));
            }
        }
        header.push_str("##[endgroup]");
        self.log(run.log_step, &header);

        let name = format!("{}{ext}", uuid::Uuid::new_v4());
        let host_script = self.paths.host_temp().join(&name);
        let guest_script = format!("{}/{name}", self.paths.guest_temp());
        if let Err(e) = std::fs::write(&host_script, script) {
            self.log(
                run.log_step,
                &format!("##[error]Failed to write the script: {e}"),
            );
            return Body::failure();
        }
        let argv: Vec<String> = split_words(&tpl)
            .into_iter()
            .map(|w| w.replace("{0}", &guest_script))
            .collect();
        let ws = self.paths.guest_workspace();
        let cwd = match working_directory.filter(|w| !w.trim().is_empty()).or(self
            .spec
            .defaults
            .working_directory
            .as_deref())
        {
            Some(w) => self.paths.resolve(&ws, w),
            None => ws,
        };
        let env = self.process_env(scope, run, &IndexMap::new());
        let Some(exec) = self.exec.as_ref() else {
            return Body::failure();
        };
        let pspec = exec.command(&argv, &env, &cwd, &self.paths);
        let (outcome, ls) = self
            .exec_process(&pspec, run.log_step, run.deadline, &run.cancel)
            .await;
        let _ = std::fs::remove_file(&host_script);
        let mut body = Body::from_outcome(&outcome);
        body.outputs = ls.outputs;
        body.state = ls.state;
        body
    }

    /// Read file commands after a step.
    fn process_files(&mut self, run: &StepRun, body: &mut Body) -> bool {
        let mut ok = true;
        let files = &run.files;
        for (i, what) in [
            (F_OUTPUT, "GITHUB_OUTPUT"),
            (F_ENV, "GITHUB_ENV"),
            (F_STATE, "GITHUB_STATE"),
        ] {
            let content = files.take(i);
            if content.is_empty() {
                continue;
            }
            match parse_file_commands(&content) {
                Ok(pairs) => {
                    for (k, v) in pairs {
                        match i {
                            F_OUTPUT => {
                                body.outputs.insert(k, v);
                            }
                            F_ENV => {
                                self.github_env.insert(k, v);
                            }
                            _ => {
                                body.state.insert(k, v);
                            }
                        }
                    }
                }
                Err(e) => {
                    self.log(
                        run.log_step,
                        &format!("##[error]Unable to process file command '{what}': {e}"),
                    );
                    ok = false;
                }
            }
        }
        for line in files.take(F_PATH).lines() {
            let l = line.trim();
            if !l.is_empty() {
                self.path_adds.insert(0, l.to_string());
            }
        }
        let summary = files.take(F_SUMMARY);
        if !summary.trim().is_empty() {
            if !self.summary.is_empty() && !self.summary.ends_with('\n') {
                self.summary.push('\n');
            }
            self.summary.push_str(&self.masker.mask(&summary));
        }
        ok
    }

    /// Evaluate and execute one step (job level when `state_no` is set,
    /// composite sub-step otherwise).
    pub(super) fn run_step<'a>(
        &'a mut self,
        scope: &'a mut Scope,
        step: &'a Step,
        log_step: i64,
        state_no: Option<i64>,
    ) -> BoxFut<'a, StepResult> {
        Box::pin(async move {
            let action_name = self.action_name(step);
            let job_env = self.job_env(scope);
            let (cond, display) = {
                let ctx = self.eval_ctx(scope, &job_env, &action_name, None);
                let cond_src = step.r#if.clone().unwrap_or_default();
                (
                    expr::evaluate_condition(&cond_src, &ctx),
                    display_name(step, &ctx),
                )
            };
            let run_it = match cond {
                Ok(b) => b,
                Err(e) => {
                    if let Some(no) = state_no {
                        self.reporter.start(no, &display);
                    }
                    self.log(
                        log_step,
                        &format!(
                            "##[error]The step condition '{}' is invalid: {e}",
                            step.r#if.clone().unwrap_or_default()
                        ),
                    );
                    return self
                        .finish_step(
                            scope,
                            step,
                            state_no,
                            &display,
                            Conclusion::Failure,
                            Conclusion::Failure,
                            IndexMap::new(),
                        )
                        .await;
                }
            };
            if !run_it {
                if let Some(no) = state_no {
                    self.reporter
                        .finish(no, Some(&display), Conclusion::Skipped);
                }
                if let Some(id) = step.id.as_deref().filter(|s| !s.is_empty()) {
                    scope.steps.insert(
                        id.to_string(),
                        json!({"outputs": {}, "outcome": "skipped", "conclusion": "skipped"}),
                    );
                }
                return StepResult {
                    conclusion: Conclusion::Skipped,
                };
            }
            if let Some(no) = state_no {
                self.reporter.start(no, &display);
            }
            let cancel = self.step_cancel_token();
            let files = match StepFiles::create(&self.paths) {
                Ok(f) => f,
                Err(e) => {
                    self.log(
                        log_step,
                        &format!("##[error]Failed to prepare the step: {e}"),
                    );
                    return self
                        .finish_step(
                            scope,
                            step,
                            state_no,
                            &display,
                            Conclusion::Failure,
                            Conclusion::Failure,
                            IndexMap::new(),
                        )
                        .await;
                }
            };
            // continue-on-error, timeout-minutes and step env.
            let (coe, timeout, step_env) = {
                let ctx = self.eval_ctx(scope, &job_env, &action_name, Some(&files));
                let coe = match &step.continue_on_error {
                    None => Ok(false),
                    Some(Value::String(s)) => {
                        expr::evaluate_template(s, &ctx).map(|v| expr::truthy(&v))
                    }
                    Some(v) => Ok(expr::truthy(v)),
                };
                let timeout = match &step.timeout_minutes {
                    None => Ok(None),
                    Some(Value::String(s)) => expr::evaluate_template(s, &ctx).map(|v| minutes(&v)),
                    Some(v) => Ok(minutes(v)),
                };
                let mut step_env = IndexMap::new();
                let mut env_err = None;
                for (k, v) in &step.env {
                    match expr::interpolate(v, &ctx) {
                        Ok(v) => {
                            step_env.insert(k.clone(), v);
                        }
                        Err(e) => env_err = Some(e),
                    }
                }
                (coe, timeout, env_err.map_or(Ok(step_env), Err))
            };
            let (coe, timeout, step_env) = match (coe, timeout, step_env) {
                (Ok(a), Ok(b), Ok(c)) => (a, b, c),
                (a, b, c) => {
                    let err = [a.err(), b.err(), c.err()].into_iter().flatten().next();
                    if let Some(e) = err {
                        self.log(log_step, &format!("##[error]{e}"));
                    }
                    return self
                        .finish_step(
                            scope,
                            step,
                            state_no,
                            &display,
                            Conclusion::Failure,
                            Conclusion::Failure,
                            IndexMap::new(),
                        )
                        .await;
                }
            };
            let deadline = timeout.map(|d| Instant::now() + d);
            let run = StepRun {
                display: display.clone(),
                action_name: action_name.clone(),
                job_env,
                step_env,
                files,
                log_step,
                deadline,
                cancel,
            };

            let mut body = if let Some(script) = &step.run {
                let (script, wd) = {
                    let ctx = self.eval_ctx(scope, &run.env(), &action_name, Some(&run.files));
                    let script = expr::interpolate(script, &ctx);
                    let wd = step
                        .working_directory
                        .as_deref()
                        .map(|w| expr::interpolate(w, &ctx))
                        .transpose();
                    (script, wd)
                };
                match (script, wd) {
                    (Ok(script), Ok(wd)) => {
                        let shell = step
                            .shell
                            .clone()
                            .or_else(|| self.spec.defaults.shell.clone());
                        self.run_script(scope, &run, &script, shell.as_deref(), wd.as_deref())
                            .await
                    }
                    (Err(e), _) | (_, Err(e)) => {
                        self.log(log_step, &format!("##[error]{e}"));
                        Body::failure()
                    }
                }
            } else if let Some(uses) = &step.uses {
                self.run_uses(scope, step, uses, &run).await
            } else {
                self.log(log_step, "##[error]Step has neither `run` nor `uses`.");
                Body::failure()
            };

            if !self.process_files(&run, &mut body) && body.status == Some(Conclusion::Success) {
                body.status = Some(Conclusion::Failure);
            }
            if body.timed_out {
                let mins = timeout.map(|d| d.as_secs_f64() / 60.0).unwrap_or(0.0);
                self.log(
                    log_step,
                    &format!(
                        "##[error]The action '{}' has timed out after {} minutes.",
                        display,
                        fmt_minutes(mins)
                    ),
                );
            }
            let mut outcome = body.status.unwrap_or(Conclusion::Failure);
            if outcome == Conclusion::Success && run.cancel.is_cancelled() {
                outcome = Conclusion::Cancelled;
            }
            if let Some(mut post) = body.post.take() {
                post.state.extend(body.state.clone());
                self.posts.push(post);
            }
            let conclusion = if outcome == Conclusion::Failure && coe {
                self.log(
                    log_step,
                    "##[warning]The step failed but `continue-on-error` is set; continuing.",
                );
                Conclusion::Success
            } else {
                outcome
            };
            self.finish_step(
                scope,
                step,
                state_no,
                &display,
                outcome,
                conclusion,
                body.outputs,
            )
            .await
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_step(
        &mut self,
        scope: &mut Scope,
        step: &Step,
        state_no: Option<i64>,
        display: &str,
        outcome: Conclusion,
        conclusion: Conclusion,
        outputs: IndexMap<String, String>,
    ) -> StepResult {
        if conclusion == Conclusion::Failure {
            scope.failed = true;
            if state_no.is_some() {
                self.job_failed = true;
            }
        }
        if let Some(id) = step.id.as_deref().filter(|s| !s.is_empty()) {
            scope.steps.insert(
                id.to_string(),
                json!({
                    "outputs": outputs,
                    "outcome": outcome.as_str(),
                    "conclusion": conclusion.as_str(),
                }),
            );
        }
        if let Some(no) = state_no {
            self.logger.flush().await;
            self.reporter.finish(no, Some(display), conclusion);
        }
        StepResult { conclusion }
    }

    async fn set_up(&mut self) -> anyhow::Result<()> {
        let s = 1;
        self.log(
            s,
            &format!(
                "Current runner version: '{}'\nRunner name: '{}'\nOperating System: Linux",
                env!("CARGO_PKG_VERSION"),
                self.cfg.name
            ),
        );
        self.log(s, "Prepare workflow directory");
        for d in [
            "_temp/_github_workflow",
            "_temp/_github_home",
            "_actions",
            "_tool",
        ] {
            std::fs::create_dir_all(self.paths.host(d))?;
        }
        std::fs::create_dir_all(self.paths.host_workspace())?;
        let event = self
            .spec
            .github
            .get("event")
            .cloned()
            .unwrap_or_else(|| json!({}));
        std::fs::write(
            self.paths.host("_temp/_github_workflow/event.json"),
            serde_json::to_vec_pretty(&event)?,
        )?;
        let logger = self.logger.clone();
        let log = move |l: &str| logger.log(1, l);
        let exec = executor::setup(&self.cfg, &self.spec, &self.paths, &log, &self.cancel).await?;
        self.exec = Some(exec);
        Ok(())
    }

    async fn run_all(&mut self, timeout: Duration) -> JobCompletion {
        // 1: Set up job
        self.reporter.start(1, "Set up job");
        let setup_ok = match self.set_up().await {
            Ok(()) => {
                self.reporter.finish(1, None, Conclusion::Success);
                true
            }
            Err(e) => {
                self.log(1, &format!("##[error]{e:#}"));
                self.job_failed = true;
                self.reporter.finish(1, None, Conclusion::Failure);
                false
            }
        };
        self.logger.flush().await;

        let mut timeout_logged = false;
        let mut scope = self.top_scope();
        let steps = self.spec.steps.clone();
        for (i, step) in steps.iter().enumerate() {
            let no = i as i64 + 2;
            if !setup_ok {
                self.reporter.finish(no, None, Conclusion::Skipped);
                continue;
            }
            self.run_step(&mut scope, step, no, Some(no)).await;
            if self.timed_out.load(Ordering::SeqCst) && !timeout_logged {
                timeout_logged = true;
                self.log_timeout(no, timeout);
            }
        }
        // Post steps, in reverse order.
        let posts: Vec<PostStep> = std::mem::take(&mut self.posts);
        for post in posts.into_iter().rev() {
            let name = format!("Post {}", post.name);
            let no = self.reporter.insert_before_last(&name);
            self.run_post(&mut scope, post, no, &name).await;
        }
        // Complete job.
        let last = self.reporter.last_number();
        self.reporter.start(last, "Complete job");
        if self.timed_out.load(Ordering::SeqCst) && !timeout_logged {
            self.log_timeout(last, timeout);
        }
        let outputs = self.job_outputs(&scope, last);
        if let Some(mut exec) = self.exec.take() {
            let logger = self.logger.clone();
            let log = move |l: &str| logger.log(last, l);
            if exec.container.is_some() {
                // Files written by root inside the container must be removable.
                let _ = tokio::process::Command::new(&exec.docker)
                    .args([
                        "exec",
                        exec.container.as_deref().unwrap_or_default(),
                        "sh",
                        "-c",
                        "chmod -R a+rwX /__w 2>/dev/null; true",
                    ])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;
            }
            exec.cleanup.run(&log).await;
        }
        self.log(last, "Cleaning up orphan processes");
        if let Err(e) = std::fs::remove_dir_all(&self.paths.host_root)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            self.log(
                last,
                &format!("##[warning]Failed to remove the job directory: {e}"),
            );
        }
        self.logger.flush().await;
        self.reporter.finish(last, None, Conclusion::Success);

        let conclusion = if self.cancel.is_cancelled() {
            Conclusion::Cancelled
        } else if self.job_failed {
            Conclusion::Failure
        } else {
            Conclusion::Success
        };
        JobCompletion {
            conclusion: conclusion.as_str().to_string(),
            outputs,
            steps: self.reporter.snapshot(),
            annotations: std::mem::take(&mut self.annotations),
            summary: (!self.summary.is_empty()).then(|| std::mem::take(&mut self.summary)),
        }
    }

    fn log_timeout(&self, step: i64, timeout: Duration) {
        let mins = timeout.as_secs_f64() / 60.0;
        self.log(
            step,
            &format!(
                "##[error]The job running on runner {} has exceeded the maximum execution time of {} minutes.",
                self.cfg.name,
                fmt_minutes(mins)
            ),
        );
    }

    fn job_outputs(&self, scope: &Scope, log_step: i64) -> IndexMap<String, String> {
        let mut out = IndexMap::new();
        if self.spec.outputs.is_empty() {
            return out;
        }
        let env = self.job_env(scope);
        let ctx = self.eval_ctx(scope, &env, "", None);
        for (k, src) in &self.spec.outputs {
            match expr::interpolate(src, &ctx) {
                Ok(v) => {
                    if self.masker.contains_secret(&v) {
                        self.log(
                            log_step,
                            &format!("##[warning]Skip output '{k}' since it may contain secret."),
                        );
                    } else {
                        out.insert(k.clone(), v);
                    }
                }
                Err(e) => self.log(
                    log_step,
                    &format!("##[warning]Failed to evaluate output '{k}': {e}"),
                ),
            }
        }
        out
    }

    async fn run_post(&mut self, scope: &mut Scope, post: PostStep, no: i64, name: &str) {
        let cond = {
            let mut s = Scope {
                inputs: post.inputs.clone(),
                steps: scope.steps.clone(),
                env: IndexMap::new(),
                failed: scope.failed,
                action_path: post.action_path.clone(),
                action_repository: post.action_repository.clone(),
                depth: 0,
            };
            s.failed = scope.failed;
            let env = self.job_env(&s);
            let ctx = self.eval_ctx(&s, &env, &post.action_name, None);
            expr::evaluate_condition(&post.cond, &ctx)
        };
        match cond {
            Ok(true) => {}
            Ok(false) => {
                self.reporter.finish(no, None, Conclusion::Skipped);
                return;
            }
            Err(e) => {
                self.reporter.start(no, name);
                self.log(no, &format!("##[error]Invalid post-if condition: {e}"));
                self.job_failed = true;
                self.logger.flush().await;
                self.reporter.finish(no, None, Conclusion::Failure);
                return;
            }
        }
        self.reporter.start(no, name);
        let post_scope = Scope {
            inputs: post.inputs.clone(),
            steps: Map::new(),
            env: IndexMap::new(),
            failed: false,
            action_path: post.action_path.clone(),
            action_repository: post.action_repository.clone(),
            depth: 0,
        };
        let files = match StepFiles::create(&self.paths) {
            Ok(f) => f,
            Err(e) => {
                self.log(no, &format!("##[error]{e}"));
                self.job_failed = true;
                self.reporter.finish(no, None, Conclusion::Failure);
                return;
            }
        };
        let run = StepRun {
            display: name.to_string(),
            action_name: post.action_name.clone(),
            job_env: self.job_env(&post_scope),
            step_env: IndexMap::new(),
            files,
            log_step: no,
            deadline: None,
            cancel: self.step_cancel_token(),
        };
        let mut extra = post.env.clone();
        for (k, v) in &post.state {
            extra.insert(format!("STATE_{k}"), v.clone());
        }
        let mut body = match &post.kind {
            PostKind::Node { script } => self.run_node(&post_scope, &run, script, &extra).await,
            PostKind::Docker {
                image,
                entrypoint,
                args,
            } => {
                self.run_container(
                    &post_scope,
                    &run,
                    image,
                    entrypoint.as_deref(),
                    args,
                    &extra,
                )
                .await
            }
        };
        self.process_files(&run, &mut body);
        let c = body.status.unwrap_or(Conclusion::Failure);
        if c == Conclusion::Failure {
            self.job_failed = true;
        }
        self.logger.flush().await;
        self.reporter.finish(no, None, c);
    }
}

/// `timeout-minutes` value → duration (fractional minutes allowed).
fn minutes(v: &Value) -> Option<Duration> {
    let m = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }?;
    (m > 0.0 && m.is_finite()).then(|| Duration::from_secs_f64(m * 60.0))
}

fn fmt_minutes(m: f64) -> String {
    if (m - m.round()).abs() < 1e-9 {
        format!("{}", m.round() as i64)
    } else {
        format!("{m:.2}")
    }
}

/// Shell command template and script extension.
pub(super) fn shell_template(
    shell: Option<&str>,
    has_bash: bool,
) -> Result<(String, &'static str), String> {
    let shell = shell.map(str::trim).unwrap_or("");
    Ok(match shell {
        "" if has_bash => ("bash --noprofile --norc -eo pipefail {0}".into(), ".sh"),
        "" => ("sh -e {0}".into(), ".sh"),
        "bash" => ("bash --noprofile --norc -eo pipefail {0}".into(), ".sh"),
        "sh" => ("sh -e {0}".into(), ".sh"),
        "python" => ("python {0}".into(), ".py"),
        "pwsh" => ("pwsh -command \". '{0}'\"".into(), ".ps1"),
        "powershell" => ("powershell -command \". '{0}'\"".into(), ".ps1"),
        custom if custom.contains("{0}") => {
            let first = split_words(custom).into_iter().next().unwrap_or_default();
            let base = first.rsplit('/').next().unwrap_or("").to_string();
            let ext = if base.starts_with("python") {
                ".py"
            } else if base.starts_with("pwsh") || base.starts_with("powershell") {
                ".ps1"
            } else {
                ".sh"
            };
            (custom.to_string(), ext)
        }
        other => {
            return Err(format!(
                "Invalid shell option '{other}'. Shell must be a valid built-in (bash, sh, python, pwsh, powershell) or a format string containing '{{0}}'"
            ));
        }
    })
}

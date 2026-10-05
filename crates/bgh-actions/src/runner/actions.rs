//! `uses:` steps: native actions, `docker://`, local and remote actions
//! (node, composite, docker).

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::executor::{CONTAINER_ROOT, docker_env_args, ensure_image};
use super::job::{Body, Conclusion, JobRunner, PostKind, PostStep, Scope, StepRun, basic_auth};
use super::process::{KillMode, ProcessOutcome, ProcessSpec};
use crate::expr;
use crate::protocol::Step;

/// A resolved action: its directory and parsed metadata.
pub(super) struct ActionDef {
    pub host_dir: PathBuf,
    pub guest_dir: String,
    /// `owner/repo` for remote actions.
    pub repository: Option<String>,
    pub meta: Value,
}

/// Parsed `owner/repo[/path]@ref`.
struct RemoteRef {
    owner: String,
    repo: String,
    path: String,
    git_ref: String,
}

fn parse_remote(uses: &str) -> Option<RemoteRef> {
    let (path, git_ref) = uses.rsplit_once('@')?;
    let mut parts = path.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    let rest: Vec<&str> = parts.collect();
    if owner.is_empty() || repo.is_empty() || git_ref.is_empty() {
        return None;
    }
    Some(RemoteRef {
        owner,
        repo,
        path: rest.join("/"),
        git_ref: git_ref.to_string(),
    })
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Convert a composite action's `runs.steps` entry into a [`Step`].
fn parse_step(v: &Value) -> Result<Step, String> {
    let obj = v.as_object().ok_or("each step must be a mapping")?;
    let s = |k: &str| -> Option<String> {
        obj.get(k).and_then(|v| match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(expr::to_display_string(other)),
        })
    };
    let map = |k: &str| -> IndexMap<String, String> {
        obj.get(k)
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        let v = match v {
                            Value::String(s) => s.clone(),
                            Value::Null => String::new(),
                            other => expr::to_display_string(other),
                        };
                        (k.clone(), v)
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let r#if = obj.get("if").and_then(|v| match v {
        Value::Null => None,
        Value::Bool(b) => Some(b.to_string()),
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    });
    let step = Step {
        id: s("id"),
        name: s("name"),
        r#if,
        uses: s("uses"),
        run: s("run"),
        shell: s("shell"),
        working_directory: s("working-directory"),
        with: map("with"),
        env: map("env"),
        continue_on_error: obj.get("continue-on-error").cloned(),
        timeout_minutes: obj.get("timeout-minutes").cloned(),
    };
    if step.run.is_none() && step.uses.is_none() {
        return Err("each composite step needs `run` or `uses`".into());
    }
    Ok(step)
}

impl JobRunner {
    /// Evaluate a step's `with:` inputs.
    fn eval_with(
        &self,
        scope: &Scope,
        step: &Step,
        run: &StepRun,
    ) -> Result<IndexMap<String, String>, String> {
        let ctx = self.eval_ctx(scope, &run.env(), &run.action_name, Some(&run.files));
        let mut out = IndexMap::new();
        for (k, v) in &step.with {
            out.insert(
                k.clone(),
                expr::interpolate(v, &ctx).map_err(|e| e.to_string())?,
            );
        }
        Ok(out)
    }

    pub(super) async fn run_uses(
        &mut self,
        scope: &mut Scope,
        step: &Step,
        uses: &str,
        run: &StepRun,
    ) -> Body {
        let with = match self.eval_with(scope, step, run) {
            Ok(w) => w,
            Err(e) => {
                self.log(run.log_step, &format!("##[error]{e}"));
                return Body::failure();
            }
        };
        let mut header = format!("##[group]Run {uses}\n");
        if !with.is_empty() {
            header.push_str("with:\n");
            for (k, v) in &with {
                header.push_str(&format!("  {k}: {v}\n"));
            }
        }
        let env = run.env();
        if !env.is_empty() {
            header.push_str("env:\n");
            for (k, v) in &env {
                header.push_str(&format!("  {k}: {v}\n"));
            }
        }
        header.push_str("##[endgroup]");
        self.log(run.log_step, &header);

        let lower = uses.trim().to_ascii_lowercase();
        let name = lower.split('@').next().unwrap_or("");
        if lower.contains('@') && !lower.starts_with("docker://") {
            match name {
                "actions/checkout" => return self.checkout(run, &with).await,
                "actions/upload-artifact" => return self.upload_artifact(run, &with).await,
                "actions/download-artifact" => return self.download_artifact(run, &with).await,
                "actions/cache" => {
                    return self
                        .cache_action(scope, run, &with, super::cache::CacheMode::Main)
                        .await;
                }
                "actions/cache/restore" => {
                    return self
                        .cache_action(scope, run, &with, super::cache::CacheMode::Restore)
                        .await;
                }
                "actions/cache/save" => {
                    return self
                        .cache_action(scope, run, &with, super::cache::CacheMode::Save)
                        .await;
                }
                _ => {}
            }
        }
        if let Some(image) = uses.trim().strip_prefix("docker://") {
            let args = with
                .get("args")
                .map(|a| super::commands::split_words(a))
                .unwrap_or_default();
            let entrypoint = with.get("entrypoint").filter(|e| !e.is_empty()).cloned();
            let extra = IndexMap::new();
            return self
                .run_container(scope, run, image, entrypoint.as_deref(), &args, &extra)
                .await;
        }
        let def = match self.resolve_action(uses.trim(), run).await {
            Ok(d) => d,
            Err(e) => {
                self.log(run.log_step, &format!("##[error]{e}"));
                return Body::failure();
            }
        };
        self.run_action(scope, step, run, def, with).await
    }

    /// Resolve inputs: `with` values, else defaults (evaluated).
    fn action_inputs(
        &self,
        scope: &Scope,
        run: &StepRun,
        meta: &Value,
        with: &IndexMap<String, String>,
    ) -> IndexMap<String, String> {
        let mut inputs = IndexMap::new();
        let defs = meta.get("inputs").and_then(Value::as_object);
        let ctx = self.eval_ctx(scope, &run.env(), &run.action_name, Some(&run.files));
        if let Some(defs) = defs {
            for (name, def) in defs {
                let given = with
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v.clone());
                let value = match given {
                    Some(v) => v,
                    None => {
                        let required = def.get("required").map(expr::truthy).unwrap_or(false);
                        match def.get("default") {
                            Some(Value::String(d)) => expr::interpolate(d, &ctx)
                                .unwrap_or_else(|e| {
                                    self.log(
                                        run.log_step,
                                        &format!("##[warning]Failed to evaluate the default of input '{name}': {e}"),
                                    );
                                    String::new()
                                }),
                            Some(Value::Null) | None => {
                                if required {
                                    self.log(
                                        run.log_step,
                                        &format!("##[warning]Input required and not supplied: {name}"),
                                    );
                                }
                                String::new()
                            }
                            Some(other) => expr::to_display_string(other),
                        }
                    }
                };
                inputs.insert(name.clone(), value);
            }
        }
        let mut unexpected = Vec::new();
        for (k, v) in with {
            if !inputs.keys().any(|n| n.eq_ignore_ascii_case(k)) {
                if defs.is_some() {
                    unexpected.push(k.clone());
                }
                inputs.insert(k.clone(), v.clone());
            }
        }
        if !unexpected.is_empty() {
            self.log(
                run.log_step,
                &format!(
                    "##[warning]Unexpected input(s) '{}', valid inputs are ['{}']",
                    unexpected.join("', '"),
                    defs.map(|d| d.keys().cloned().collect::<Vec<_>>().join("', '"))
                        .unwrap_or_default()
                ),
            );
        }
        inputs
    }

    async fn run_action(
        &mut self,
        scope: &mut Scope,
        step: &Step,
        run: &StepRun,
        def: ActionDef,
        with: IndexMap<String, String>,
    ) -> Body {
        let inputs = self.action_inputs(scope, run, &def.meta, &with);
        let runs = def.meta.get("runs").cloned().unwrap_or(Value::Null);
        let using = runs
            .get("using")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        let inputs_value = Value::Object(
            inputs
                .iter()
                .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                .collect(),
        );
        let mut extra: IndexMap<String, String> = inputs
            .iter()
            .map(|(k, v)| {
                (
                    format!("INPUT_{}", k.replace(' ', "_").to_ascii_uppercase()),
                    v.clone(),
                )
            })
            .collect();
        extra.insert("GITHUB_ACTION_PATH".into(), def.guest_dir.clone());
        if let Some(r) = &def.repository {
            extra.insert("GITHUB_ACTION_REPOSITORY".into(), r.clone());
        }
        let action_scope = Scope {
            inputs: inputs_value.clone(),
            steps: Map::new(),
            env: IndexMap::new(),
            failed: false,
            action_path: Some(def.guest_dir.clone()),
            action_repository: def.repository.clone(),
            depth: scope.depth + 1,
        };
        let str_of = |k: &str| runs.get(k).and_then(Value::as_str).map(str::to_string);
        if using.starts_with("node") {
            let Some(main) = str_of("main") else {
                self.log(
                    run.log_step,
                    "##[error]The action's `runs.main` is missing.",
                );
                return Body::failure();
            };
            let mut state = IndexMap::new();
            if let Some(pre) = str_of("pre") {
                let cond = str_of("pre-if").unwrap_or_else(|| "always()".into());
                let ok = {
                    let ctx = self.eval_ctx(scope, &run.env(), &run.action_name, Some(&run.files));
                    expr::evaluate_condition(&cond, &ctx).unwrap_or(false)
                };
                if ok {
                    let b = self
                        .run_node(
                            &action_scope,
                            run,
                            &format!("{}/{pre}", def.guest_dir),
                            &extra,
                        )
                        .await;
                    state.extend(b.state);
                    let file_state = std::fs::read_to_string(&run.files.host[super::job::F_STATE])
                        .unwrap_or_default();
                    let _ = std::fs::write(&run.files.host[super::job::F_STATE], b"");
                    if let Ok(pairs) = super::commands::parse_file_commands(&file_state) {
                        state.extend(pairs);
                    }
                    if b.status != Some(Conclusion::Success) {
                        return Body { state, ..b };
                    }
                }
            }
            let mut main_extra = extra.clone();
            for (k, v) in &state {
                main_extra.insert(format!("STATE_{k}"), v.clone());
            }
            let mut body = self
                .run_node(
                    &action_scope,
                    run,
                    &format!("{}/{main}", def.guest_dir),
                    &main_extra,
                )
                .await;
            let mut all_state = state;
            all_state.extend(std::mem::take(&mut body.state));
            body.state = all_state;
            if let Some(post) = str_of("post") {
                body.post = Some(PostStep {
                    name: run.display.clone(),
                    cond: str_of("post-if").unwrap_or_else(|| "always()".into()),
                    kind: PostKind::Node {
                        script: format!("{}/{post}", def.guest_dir),
                    },
                    env: extra.clone(),
                    state: IndexMap::new(),
                    action_name: run.action_name.clone(),
                    action_path: Some(def.guest_dir.clone()),
                    action_repository: def.repository.clone(),
                    inputs: inputs_value,
                });
            }
            body
        } else if using == "composite" {
            self.run_composite(scope, step, run, &def, inputs_value)
                .await
        } else if using == "docker" {
            let Some(image) = str_of("image") else {
                self.log(
                    run.log_step,
                    "##[error]The action's `runs.image` is missing.",
                );
                return Body::failure();
            };
            let image = match self.action_image(&def, &image, run).await {
                Ok(i) => i,
                Err(e) => {
                    self.log(run.log_step, &format!("##[error]{e}"));
                    return Body::failure();
                }
            };
            let (args, env_extra, entrypoint) = {
                let ctx = self.eval_ctx(
                    &action_scope,
                    &run.env(),
                    &run.action_name,
                    Some(&run.files),
                );
                let args: Vec<String> = runs
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|v| match v {
                                Value::String(s) => {
                                    expr::interpolate(s, &ctx).unwrap_or_else(|_| s.clone())
                                }
                                o => expr::to_display_string(o),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut env_extra = extra.clone();
                if let Some(e) = runs.get("env").and_then(Value::as_object) {
                    for (k, v) in e {
                        let v = match v {
                            Value::String(s) => {
                                expr::interpolate(s, &ctx).unwrap_or_else(|_| s.clone())
                            }
                            o => expr::to_display_string(o),
                        };
                        env_extra.insert(k.clone(), v);
                    }
                }
                (args, env_extra, str_of("entrypoint"))
            };
            let mut body = self
                .run_container(
                    &action_scope,
                    run,
                    &image,
                    entrypoint.as_deref(),
                    &args,
                    &env_extra,
                )
                .await;
            if let Some(post) = str_of("post-entrypoint") {
                body.post = Some(PostStep {
                    name: run.display.clone(),
                    cond: str_of("post-if").unwrap_or_else(|| "always()".into()),
                    kind: PostKind::Docker {
                        image,
                        entrypoint: Some(post),
                        args,
                    },
                    env: env_extra,
                    state: IndexMap::new(),
                    action_name: run.action_name.clone(),
                    action_path: Some(def.guest_dir.clone()),
                    action_repository: def.repository.clone(),
                    inputs: inputs_value,
                });
            }
            body
        } else {
            self.log(
                run.log_step,
                &format!("##[error]Unsupported action runtime '{using}'."),
            );
            Body::failure()
        }
    }

    async fn run_composite(
        &mut self,
        scope: &mut Scope,
        _step: &Step,
        run: &StepRun,
        def: &ActionDef,
        inputs: Value,
    ) -> Body {
        if scope.depth >= 9 {
            self.log(
                run.log_step,
                "##[error]Composite actions are nested too deeply (maximum 9 levels).",
            );
            return Body::failure();
        }
        let runs = def.meta.get("runs").cloned().unwrap_or(Value::Null);
        let raw_steps = runs
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut steps = Vec::new();
        for s in &raw_steps {
            match parse_step(s) {
                Ok(s) => steps.push(s),
                Err(e) => {
                    self.log(
                        run.log_step,
                        &format!("##[error]Invalid composite action: {e}"),
                    );
                    return Body::failure();
                }
            }
        }
        let mut env = scope.env.clone();
        env.extend(run.step_env.clone());
        let mut sub = Scope {
            inputs,
            steps: Map::new(),
            env,
            failed: false,
            action_path: Some(def.guest_dir.clone()),
            action_repository: def.repository.clone(),
            depth: scope.depth + 1,
        };
        let mut status = Conclusion::Success;
        for s in &steps {
            let r = self.run_step(&mut sub, s, run.log_step, None).await;
            match r.conclusion {
                Conclusion::Failure => status = Conclusion::Failure,
                Conclusion::Cancelled if status == Conclusion::Success => {
                    status = Conclusion::Cancelled
                }
                _ => {}
            }
        }
        if run.cancel.is_cancelled() && status == Conclusion::Success {
            status = Conclusion::Cancelled;
        }
        let mut body = Body {
            status: Some(status),
            ..Default::default()
        };
        if let Some(outs) = def.meta.get("outputs").and_then(Value::as_object) {
            let job_env = self.job_env(&sub);
            let ctx = self.eval_ctx(&sub, &job_env, &run.action_name, None);
            for (k, o) in outs {
                let src = o.get("value").and_then(Value::as_str).unwrap_or("");
                match expr::interpolate(src, &ctx) {
                    Ok(v) => {
                        body.outputs.insert(k.clone(), v);
                    }
                    Err(e) => self.log(
                        run.log_step,
                        &format!("##[warning]Failed to evaluate output '{k}': {e}"),
                    ),
                }
            }
        }
        body
    }

    /// Run `node <script>` in the job environment.
    pub(super) async fn run_node(
        &mut self,
        scope: &Scope,
        run: &StepRun,
        script: &str,
        extra: &IndexMap<String, String>,
    ) -> Body {
        let env = self.process_env(scope, run, extra);
        let Some(exec) = self.exec.as_ref() else {
            return Body::failure();
        };
        let argv = vec!["node".to_string(), script.to_string()];
        let pspec = exec.command(&argv, &env, &self.paths.guest_workspace(), &self.paths);
        let (outcome, ls) = self
            .exec_process(&pspec, run.log_step, run.deadline, &run.cancel)
            .await;
        let mut body = Body::from_outcome(&outcome);
        body.outputs = ls.outputs;
        body.state = ls.state;
        body
    }

    /// `docker run` a step container (docker:// steps, docker actions).
    pub(super) async fn run_container(
        &mut self,
        scope: &Scope,
        run: &StepRun,
        image: &str,
        entrypoint: Option<&str>,
        args: &[String],
        extra: &IndexMap<String, String>,
    ) -> Body {
        let Some(exec) = self.exec.as_ref() else {
            return Body::failure();
        };
        if !exec.docker_ok {
            self.log(
                run.log_step,
                "##[error]Container actions require docker, which is not available on this runner.",
            );
            return Body::failure();
        }
        let docker = exec.docker.clone();
        let network = exec.network.clone();
        let logger = self.logger.clone();
        let log_step = run.log_step;
        let log = move |l: &str| logger.log(log_step, l);
        if let Err(e) = ensure_image(&docker, image, &log, &run.cancel).await {
            self.log(run.log_step, &format!("##[error]{e:#}"));
            return Body::failure();
        }
        let mut env = self.process_env(scope, run, extra);
        env.shift_remove("PATH");
        for v in env.values_mut() {
            *v = self.paths.to_container(v);
        }
        env.insert("GITHUB_WORKSPACE".into(), "/github/workspace".into());
        env.insert("HOME".into(), "/github/home".into());
        let name = format!("bgh-step-{}", uuid::Uuid::new_v4());
        let mut dargs = vec![
            "run".to_string(),
            "--rm".into(),
            "--name".into(),
            name.clone(),
        ];
        if let Some(n) = network {
            dargs.extend(["--network".into(), n]);
        }
        dargs.extend([
            "-v".into(),
            format!("{}:{CONTAINER_ROOT}", self.paths.host_root.display()),
            "-v".into(),
            format!(
                "{}:/github/workspace",
                self.paths.host_workspace().display()
            ),
            "-v".into(),
            format!(
                "{}:/github/home",
                self.paths.host("_temp/_github_home").display()
            ),
            "-v".into(),
            format!(
                "{}:/github/workflow",
                self.paths.host("_temp/_github_workflow").display()
            ),
            "-w".into(),
            "/github/workspace".into(),
        ]);
        let mut penv = Vec::new();
        docker_env_args(&env, &mut dargs, &mut penv);
        if let Some(e) = entrypoint {
            dargs.extend(["--entrypoint".into(), e.to_string()]);
        }
        dargs.push(image.to_string());
        dargs.extend(args.iter().cloned());
        let mut pspec = ProcessSpec::new(docker.clone(), dargs);
        pspec.env = penv;
        pspec.kill = KillMode::DockerContainer { docker, name };
        let (outcome, ls) = self
            .exec_process(&pspec, run.log_step, run.deadline, &run.cancel)
            .await;
        let mut body = Body::from_outcome(&outcome);
        body.outputs = ls.outputs;
        body.state = ls.state;
        body
    }

    /// Image of a docker action: `docker://img` or a Dockerfile to build.
    async fn action_image(
        &mut self,
        def: &ActionDef,
        image: &str,
        run: &StepRun,
    ) -> Result<String, String> {
        if let Some(i) = image.strip_prefix("docker://") {
            return Ok(i.to_string());
        }
        let Some(exec) = self.exec.as_ref() else {
            return Err("no executor".into());
        };
        if !exec.docker_ok {
            return Err(
                "Container actions require docker, which is not available on this runner.".into(),
            );
        }
        let docker = exec.docker.clone();
        let dockerfile = def.host_dir.join(image);
        let content = std::fs::read(&dockerfile)
            .map_err(|e| format!("Failed to read {}: {e}", dockerfile.display()))?;
        let mut h = Sha256::new();
        h.update(def.host_dir.to_string_lossy().as_bytes());
        h.update(&content);
        let tag = format!("bgh-action-{}", &hex::encode(h.finalize())[..16]);
        let mut spec = ProcessSpec::new(
            docker,
            vec![
                "build".into(),
                "-t".into(),
                tag.clone(),
                "-f".into(),
                dockerfile.to_string_lossy().into_owned(),
                def.host_dir.to_string_lossy().into_owned(),
            ],
        );
        spec.cwd = Some(def.host_dir.clone());
        self.log(
            run.log_step,
            &format!(
                "##[group]Build container for action use: '{}'",
                dockerfile.display()
            ),
        );
        self.log(run.log_step, &format!("[command]{}", spec.display()));
        let (outcome, _) = self
            .exec_process(&spec, run.log_step, run.deadline, &run.cancel)
            .await;
        self.log(run.log_step, "##[endgroup]");
        if outcome.success() {
            Ok(tag)
        } else {
            Err("Docker build failed".into())
        }
    }

    /// Run git on the host, logging the (masked) command and its output.
    pub(super) async fn git(
        &mut self,
        args: Vec<String>,
        cwd: Option<&Path>,
        log_step: i64,
        cancel: &tokio_util::sync::CancellationToken,
        quiet: bool,
    ) -> Result<String, String> {
        let mut spec = ProcessSpec::new(self.cfg.git_bin.clone(), args);
        spec.cwd = cwd.map(Path::to_path_buf);
        spec.env = vec![
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GCM_INTERACTIVE".into(), "Never".into()),
        ];
        self.log(log_step, &format!("[command]{}", spec.display()));
        let mut out = String::new();
        let logger = self.logger.clone();
        let outcome = {
            let mut on_line = |l: &str| {
                if !quiet {
                    logger.log(log_step, l);
                }
                out.push_str(l);
                out.push('\n');
            };
            super::process::run_process(&spec, &mut on_line, cancel, None).await
        };
        match outcome {
            ProcessOutcome::Exited(0) => Ok(out),
            ProcessOutcome::Exited(code) => Err(format!(
                "The process '{}' failed with exit code {code}",
                self.cfg.git_bin
            )),
            ProcessOutcome::Cancelled => Err("The operation was canceled.".into()),
            ProcessOutcome::TimedOut => Err("git timed out".into()),
            ProcessOutcome::Failed(e) => Err(e),
        }
    }

    /// Auth header for git against this server.
    pub(super) fn git_auth_args(&self, token: &str) -> Vec<String> {
        if token.is_empty() {
            return Vec::new();
        }
        vec![
            "-c".into(),
            format!(
                "http.extraheader=AUTHORIZATION: basic {}",
                basic_auth(token)
            ),
        ]
    }

    /// Fetch `url` at `git_ref` into `dest` (shallow).
    async fn clone_action(
        &mut self,
        url: &str,
        git_ref: &str,
        dest: &Path,
        auth: bool,
        run: &StepRun,
    ) -> Result<(), String> {
        let tmp = dest.with_file_name(format!(
            ".{}.tmp-{}",
            dest.file_name().and_then(|f| f.to_str()).unwrap_or("x"),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let token = if auth {
            self.spec.token.clone()
        } else {
            String::new()
        };
        let result = async {
            self.git(
                vec!["init".into(), "-q".into()],
                Some(&tmp),
                run.log_step,
                &run.cancel,
                true,
            )
            .await?;
            self.git(
                vec![
                    "remote".into(),
                    "add".into(),
                    "origin".into(),
                    url.to_string(),
                ],
                Some(&tmp),
                run.log_step,
                &run.cancel,
                true,
            )
            .await?;
            let mut fetch = self.git_auth_args(&token);
            fetch.extend([
                "fetch".into(),
                "--no-tags".into(),
                "--depth=1".into(),
                "origin".into(),
                git_ref.to_string(),
            ]);
            let shallow = self
                .git(fetch, Some(&tmp), run.log_step, &run.cancel, true)
                .await;
            let target = if shallow.is_ok() {
                "FETCH_HEAD".to_string()
            } else if let Err(e) = shallow
                && !is_sha(git_ref)
            {
                return Err(e);
            } else {
                let mut full = self.git_auth_args(&token);
                full.extend([
                    "fetch".into(),
                    "--tags".into(),
                    "origin".into(),
                    "+refs/heads/*:refs/remotes/origin/*".into(),
                ]);
                self.git(full, Some(&tmp), run.log_step, &run.cancel, true)
                    .await?;
                git_ref.to_string()
            };
            self.git(
                vec!["checkout".into(), "-q".into(), "--force".into(), target],
                Some(&tmp),
                run.log_step,
                &run.cancel,
                true,
            )
            .await?;
            Ok::<(), String>(())
        }
        .await;
        match result {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(dest);
                if let Some(p) = dest.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                std::fs::rename(&tmp, dest).map_err(|e| e.to_string())
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp);
                Err(e)
            }
        }
    }

    async fn resolve_action(&mut self, uses: &str, run: &StepRun) -> Result<ActionDef, String> {
        let (host_dir, guest_dir, repository) = if uses.starts_with("./") || uses == "." {
            let guest = self.paths.resolve(&self.paths.guest_workspace(), uses);
            (self.paths.to_host(&guest), guest, None)
        } else {
            let r = parse_remote(uses).ok_or_else(|| {
                format!("Expected format {{org}}/{{repo}}[/path]@ref. Actual '{uses}'")
            })?;
            let rel = format!("_actions/{}/{}/{}", r.owner, r.repo, r.git_ref);
            let root = self.paths.host(&rel);
            if !root.join(".git").exists() {
                self.log(
                    run.log_step,
                    &format!(
                        "Download action repository '{}/{}@{}'",
                        r.owner, r.repo, r.git_ref
                    ),
                );
                let own = format!(
                    "{}/{}/{}.git",
                    self.spec.server_url.trim_end_matches('/'),
                    r.owner,
                    r.repo
                );
                let mut errors = Vec::new();
                let mut ok = self
                    .clone_action(&own, &r.git_ref, &root, true, run)
                    .await
                    .map_err(|e| errors.push(format!("{own}: {e}")))
                    .is_ok();
                if !ok && self.cfg.remote_actions {
                    let gh = format!(
                        "{}/{}/{}.git",
                        self.cfg.github_url.trim_end_matches('/'),
                        r.owner,
                        r.repo
                    );
                    ok = self
                        .clone_action(&gh, &r.git_ref, &root, false, run)
                        .await
                        .map_err(|e| errors.push(format!("{gh}: {e}")))
                        .is_ok();
                }
                if !ok {
                    return Err(format!(
                        "Unable to resolve action '{uses}': {}",
                        errors.join("; ")
                    ));
                }
            }
            let sub = if r.path.is_empty() {
                rel.clone()
            } else {
                format!("{rel}/{}", r.path)
            };
            (
                self.paths.host(&sub),
                self.paths.guest(&sub),
                Some(format!("{}/{}", r.owner, r.repo)),
            )
        };
        let meta = ["action.yml", "action.yaml"]
            .iter()
            .map(|f| host_dir.join(f))
            .find(|p| p.is_file());
        let meta = match meta {
            Some(p) => {
                let src = std::fs::read_to_string(&p).map_err(|e| e.to_string())?;
                serde_yaml::from_str::<Value>(&src)
                    .map_err(|e| format!("Failed to parse {}: {e}", p.display()))?
            }
            None if host_dir.join("Dockerfile").is_file() => serde_json::json!({
                "runs": {"using": "docker", "image": "Dockerfile"}
            }),
            None => {
                return Err(format!(
                    "Can't find 'action.yml', 'action.yaml' or 'Dockerfile' under '{}'. Did you forget to run actions/checkout before running your local action?",
                    guest_dir
                ));
            }
        };
        Ok(ActionDef {
            host_dir,
            guest_dir,
            repository,
            meta,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_refs() {
        let r = parse_remote("actions/setup-node/sub/dir@v4").unwrap();
        assert_eq!(
            (
                r.owner.as_str(),
                r.repo.as_str(),
                r.path.as_str(),
                r.git_ref.as_str()
            ),
            ("actions", "setup-node", "sub/dir", "v4")
        );
        assert!(parse_remote("nope@v1").is_none());
        assert!(parse_remote("a/b").is_none());
    }

    #[test]
    fn parses_composite_steps() {
        let v = serde_json::json!({"run": "echo", "shell": "bash", "working-directory": "x", "env": {"A": 1}, "if": true});
        let s = parse_step(&v).unwrap();
        assert_eq!(s.working_directory.as_deref(), Some("x"));
        assert_eq!(s.env["A"], "1");
        assert_eq!(s.r#if.as_deref(), Some("true"));
        assert!(parse_step(&serde_json::json!({"name": "x"})).is_err());
    }
}

//! End-to-end tests of the job runner against an in-memory [`Backend`].

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::protocol::{
    ArtifactInfo, Backend, ContainerSpec, Heartbeat, JobCompletion, JobSpec, RunDefaults, Step,
    StepState,
};

#[derive(Default)]
struct Mock {
    logs: Mutex<BTreeMap<i64, String>>,
    updates: Mutex<Vec<Vec<StepState>>>,
    artifacts: Mutex<Vec<(ArtifactInfo, Vec<u8>)>>,
    jobs: Mutex<Vec<JobSpec>>,
    completed: Mutex<Vec<(i64, JobCompletion)>>,
    /// Ask the runner to cancel once this step number is in progress.
    cancel_when_running: Option<i64>,
}

#[async_trait]
impl Backend for Mock {
    async fn acquire(&self, wait: Duration) -> anyhow::Result<Option<JobSpec>> {
        let job = self.jobs.lock().unwrap().pop();
        if job.is_none() {
            tokio::time::sleep(wait.min(Duration::from_millis(50))).await;
        }
        Ok(job)
    }

    async fn append_log(&self, _job_id: i64, step: i64, text: &str) -> anyhow::Result<()> {
        assert!(text.ends_with('\n'), "log chunks must be complete lines");
        self.logs
            .lock()
            .unwrap()
            .entry(step)
            .or_default()
            .push_str(text);
        Ok(())
    }

    async fn update_steps(&self, _job_id: i64, steps: &[StepState]) -> anyhow::Result<Heartbeat> {
        self.updates.lock().unwrap().push(steps.to_vec());
        let cancel = self.cancel_when_running.is_some_and(|n| {
            steps
                .iter()
                .any(|s| s.number == n && s.status == "in_progress")
        });
        Ok(Heartbeat { cancel })
    }

    async fn complete(&self, job_id: i64, result: &JobCompletion) -> anyhow::Result<()> {
        self.completed
            .lock()
            .unwrap()
            .push((job_id, result.clone()));
        Ok(())
    }

    async fn upload_artifact(
        &self,
        _job_id: i64,
        name: &str,
        zip: &Path,
        _retention_days: Option<i64>,
    ) -> anyhow::Result<ArtifactInfo> {
        let data = std::fs::read(zip)?;
        let mut arts = self.artifacts.lock().unwrap();
        arts.retain(|(a, _)| a.name != name);
        let info = ArtifactInfo {
            id: arts.len() as i64 + 100,
            name: name.to_string(),
            size_in_bytes: data.len() as i64,
        };
        arts.push((info.clone(), data));
        Ok(info)
    }

    async fn list_artifacts(&self, _job_id: i64) -> anyhow::Result<Vec<ArtifactInfo>> {
        Ok(self
            .artifacts
            .lock()
            .unwrap()
            .iter()
            .map(|(a, _)| a.clone())
            .collect())
    }

    async fn download_artifact(
        &self,
        _job_id: i64,
        artifact_id: i64,
        dest: &Path,
    ) -> anyhow::Result<()> {
        let arts = self.artifacts.lock().unwrap();
        let (_, data) = arts
            .iter()
            .find(|(a, _)| a.id == artifact_id)
            .ok_or_else(|| anyhow::anyhow!("no artifact"))?;
        std::fs::write(dest, data)?;
        Ok(())
    }
}

impl Mock {
    fn log(&self, step: i64) -> String {
        self.logs
            .lock()
            .unwrap()
            .get(&step)
            .cloned()
            .unwrap_or_default()
    }

    fn all_logs(&self) -> String {
        self.logs.lock().unwrap().values().cloned().collect()
    }
}

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn spec(steps: Vec<Step>) -> JobSpec {
    JobSpec {
        job_id: 42,
        run_id: 7,
        run_number: 3,
        run_attempt: 1,
        job_key: "build".into(),
        name: "build".into(),
        workflow_name: "CI".into(),
        workflow_path: ".github/workflows/ci.yml".into(),
        repository: "octo/app".into(),
        repository_id: 9,
        repository_owner: "octo".into(),
        server_url: "http://localhost:3000".into(),
        api_url: "http://localhost:3000/api/v3".into(),
        token: "ghs_tokenvalue123".into(),
        github: json!({
            "repository": "octo/app",
            "sha": SHA,
            "ref": "refs/heads/main",
            "ref_name": "main",
            "ref_type": "branch",
            "event_name": "push",
            "actor": "octocat",
            "event": {"marker": "event-payload-ok"},
        }),
        env: IndexMap::new(),
        vars: json!({"V1": "var-one"}),
        secrets: IndexMap::new(),
        matrix: json!({"os": "linux"}),
        needs: json!({}),
        inputs: json!({}),
        strategy: json!({"fail-fast": true}),
        defaults: RunDefaults::default(),
        container: None,
        services: IndexMap::new(),
        steps,
        outputs: IndexMap::new(),
        timeout_minutes: 10,
        environment: None,
        token_permissions: IndexMap::new(),
    }
}

fn run(script: &str) -> Step {
    Step {
        run: Some(script.to_string()),
        ..Default::default()
    }
}

fn run_id(id: &str, script: &str) -> Step {
    Step {
        id: Some(id.to_string()),
        run: Some(script.to_string()),
        ..Default::default()
    }
}

fn uses(u: &str, with: &[(&str, &str)]) -> Step {
    Step {
        uses: Some(u.to_string()),
        with: with
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..Default::default()
    }
}

fn cond(mut s: Step, c: &str) -> Step {
    s.r#if = Some(c.to_string());
    s
}

struct Env {
    _dir: tempfile::TempDir,
    cfg: RunnerConfig,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let cfg = RunnerConfig {
        name: "test-runner".into(),
        work_dir: dir.path().join("work"),
        executor: ExecutorKind::Shell,
        remote_actions: false,
        heartbeat_interval: Duration::from_millis(200),
        ..RunnerConfig::default()
    };
    Env { _dir: dir, cfg }
}

async fn exec(mock: &Arc<Mock>, cfg: &RunnerConfig, spec: JobSpec) -> JobCompletion {
    let backend: Arc<dyn Backend> = mock.clone();
    execute_job(backend, Arc::new(cfg.clone()), spec).await
}

fn conclusions(c: &JobCompletion) -> Vec<(String, String)> {
    c.steps
        .iter()
        .map(|s| (s.name.clone(), s.conclusion.clone().unwrap_or_default()))
        .collect()
}

fn workspace(cfg: &RunnerConfig, spec: &JobSpec) -> std::path::PathBuf {
    cfg.work_dir.join(spec.job_id.to_string()).join("app/app")
}

#[tokio::test]
async fn multi_step_env_and_expressions() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut s = spec(vec![
        Step {
            name: Some("Greet ${{ matrix.os }}".into()),
            env: [("BAZ".to_string(), "${{ env.FOO }}-x".to_string())]
                .into_iter()
                .collect(),
            run: Some(
                "echo \"vals $FOO $BAZ ${{ matrix.os }} ${{ vars.V1 }} $GITHUB_REPOSITORY $CI $RUNNER_OS $GITHUB_ACTION\"\n\
                 echo \"sha=$GITHUB_SHA ref=$GITHUB_REF_NAME ws=$GITHUB_WORKSPACE\"\n\
                 cat \"$GITHUB_EVENT_PATH\""
                    .into(),
            ),
            ..Default::default()
        },
        run("echo \"second ${{ github.repository }} ${{ runner.os }} ${{ job.status }} $GITHUB_ACTION\""),
    ]);
    s.env.insert("FOO".into(), "bar".into());
    let ws = workspace(&e.cfg, &s);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    assert_eq!(
        conclusions(&c),
        vec![
            ("Set up job".into(), "success".into()),
            ("Greet linux".into(), "success".into()),
            (
                "Run echo \"second octo/app Linux success $GITHUB_ACTION\"".into(),
                "success".into()
            ),
            ("Complete job".into(), "success".into()),
        ]
    );
    let log = mock.log(2);
    assert!(log.contains("##[group]Run echo"), "{log}");
    assert!(
        log.contains("shell: bash --noprofile --norc -eo pipefail {0}"),
        "{log}"
    );
    assert!(log.contains("  BAZ: bar-x"), "{log}");
    assert!(
        log.contains("vals bar bar-x linux var-one octo/app true Linux __run\n"),
        "{log}"
    );
    assert!(
        log.contains(&format!("sha={SHA} ref=main ws={}", ws.display())),
        "{log}"
    );
    assert!(log.contains("event-payload-ok"), "{log}");
    assert!(
        mock.log(3)
            .contains("second octo/app Linux success __run_2")
    );
    assert!(mock.log(1).contains("Runner name: 'test-runner'"));
    // Job directory removed.
    assert!(!e.cfg.work_dir.join("42").exists());
    // Steps were reported, ending with the final state.
    let updates = mock.updates.lock().unwrap();
    assert!(updates.len() >= 2);
    assert_eq!(updates.last().unwrap(), &c.steps);
    assert!(
        c.steps
            .iter()
            .all(|s| s.started_at.is_some() && s.completed_at.is_some())
    );
}

#[tokio::test]
async fn outputs_flow_to_later_steps_and_job_outputs() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut s = spec(vec![
        run_id(
            "a",
            "echo \"y=1\" >> \"$GITHUB_OUTPUT\"\n\
             { echo 'multi<<EOF'; echo l1; echo l2; echo EOF; } >> \"$GITHUB_OUTPUT\"\n\
             echo '::set-output name=z::legacy'",
        ),
        run(
            "echo \"got ${{ steps.a.outputs.y }} ${{ steps.a.outputs.z }} ${{ steps.a.outcome }}\"",
        ),
    ]);
    s.outputs
        .insert("o".into(), "${{ steps.a.outputs.multi }}".into());
    s.outputs
        .insert("y".into(), "y=${{ steps.a.outputs.y }}".into());
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    assert!(
        mock.log(3).contains("got 1 legacy success"),
        "{}",
        mock.log(3)
    );
    assert_eq!(c.outputs["o"], "l1\nl2");
    assert_eq!(c.outputs["y"], "y=1");
}

#[tokio::test]
async fn github_env_and_path_persist() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let s = spec(vec![
        run("echo \"MYVAR=hello\" >> \"$GITHUB_ENV\"\n\
             { echo 'ML<<X'; echo a; echo b; echo X; } >> \"$GITHUB_ENV\"\n\
             mkdir -p \"$RUNNER_TEMP/bin\"\n\
             printf '#!/bin/sh\\necho mytool-ran\\n' > \"$RUNNER_TEMP/bin/mytool\"\n\
             chmod +x \"$RUNNER_TEMP/bin/mytool\"\n\
             echo \"$RUNNER_TEMP/bin\" >> \"$GITHUB_PATH\""),
        run(
            "echo \"var=$MYVAR ${{ env.MYVAR }}\"; echo \"$ML\" | tr '\\n' ,; echo; mytool; echo \"$PATH\"",
        ),
    ]);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    let log = mock.log(3);
    assert!(log.contains("var=hello hello"), "{log}");
    assert!(log.contains("a,b,"), "{log}");
    assert!(log.contains("mytool-ran"), "{log}");
}

#[tokio::test]
async fn conditions_failure_and_always() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let s = spec(vec![
        cond(run("echo never"), "false"),
        run("echo before; exit 3"),
        run("echo skipped-step"),
        cond(run("echo on-failure ${{ job.status }}"), "failure()"),
        cond(run("echo always-ran"), "${{ always() }}"),
        cond(run("echo success-only"), "success()"),
    ]);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "failure");
    let concl: Vec<String> = conclusions(&c).into_iter().map(|(_, c)| c).collect();
    assert_eq!(
        concl,
        vec![
            "success", "skipped", "failure", "skipped", "success", "success", "skipped", "success"
        ]
    );
    assert!(
        mock.log(3)
            .contains("##[error]Process completed with exit code 3.")
    );
    assert!(mock.log(5).contains("on-failure failure"));
    assert!(mock.log(6).contains("always-ran"));
    assert!(mock.log(2).is_empty());
    assert!(!mock.all_logs().contains("skipped-step\n"));
}

#[tokio::test]
async fn continue_on_error() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut failing = run_id("x", "exit 1");
    failing.continue_on_error = Some(Value::Bool(true));
    let mut failing2 = run_id("y", "exit 2");
    failing2.continue_on_error = Some(Value::String("${{ matrix.os == 'linux' }}".into()));
    let s = spec(vec![
        failing,
        failing2,
        run(
            "echo \"res ${{ steps.x.outcome }} ${{ steps.x.conclusion }} ${{ steps.y.conclusion }}\"",
        ),
    ]);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    assert!(
        mock.log(4).contains("res failure success success"),
        "{}",
        mock.log(4)
    );
}

#[tokio::test]
async fn masks_secrets_and_add_mask() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut s = spec(vec![run("echo \"plain topsecret\"\n\
             echo \"::add-mask::hidden1\"\n\
             echo \"value hidden1 here\"\n\
             echo \"${{ secrets.S }} via expr\"\n\
             echo \"token $TOK\"\n\
             echo \"second line2x\"")]);
    s.secrets.insert("S".into(), "topsecret".into());
    s.secrets.insert("ML".into(), "line1x\nline2x\n".into());
    s.steps[0]
        .env
        .insert("TOK".into(), "${{ github.token }}".into());
    s.outputs.insert("leak".into(), "${{ secrets.S }}".into());
    s.outputs.insert("fine".into(), "ok".into());
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success");
    let all = mock.all_logs();
    for bad in ["topsecret", "ghs_tokenvalue123", "line2x"] {
        assert!(!all.contains(bad), "{bad} leaked: {all}");
    }
    // The script header is printed before the mask is registered (as on
    // GitHub); the output must be masked.
    let output = all.split("##[endgroup]\n").nth(1).unwrap();
    assert!(!output.contains("hidden1"), "{output}");
    assert!(all.contains("plain ***"));
    assert!(all.contains("value *** here"));
    assert!(all.contains("*** via expr"));
    assert!(all.contains("token ***"));
    assert!(!output.contains("add-mask"));
    assert!(!c.outputs.contains_key("leak"));
    assert_eq!(c.outputs["fine"], "ok");
    assert!(all.contains("Skip output 'leak' since it may contain secret."));
}

#[tokio::test]
async fn annotations_groups_and_summary() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let s = spec(vec![
        run(
            "echo '::error file=a.rs,line=3,col=2,title=Bad%3A thing::boom'\n\
             echo '::warning::careful%0Anow'\n\
             echo '::notice::fyi'\n\
             echo '::group::My Group'\n\
             echo inside\n\
             echo '::endgroup::'\n\
             echo '::stop-commands::tok123'\n\
             echo '::error::not-a-command'\n\
             echo '::tok123::'\n\
             echo '::debug::hidden-debug'\n\
             echo '# Summary' >> \"$GITHUB_STEP_SUMMARY\"",
        ),
        run("echo 'more' >> \"$GITHUB_STEP_SUMMARY\""),
    ]);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success");
    assert_eq!(c.annotations.len(), 3, "{:?}", c.annotations);
    let a = &c.annotations[0];
    assert_eq!(a.level, "failure");
    assert_eq!(a.message, "boom");
    assert_eq!(a.path.as_deref(), Some("a.rs"));
    assert_eq!(a.start_line, Some(3));
    assert_eq!(a.start_column, Some(2));
    assert_eq!(a.title.as_deref(), Some("Bad: thing"));
    assert_eq!(c.annotations[1].level, "warning");
    assert_eq!(c.annotations[1].message, "careful\nnow");
    assert_eq!(c.annotations[2].level, "notice");
    let log = mock.log(2);
    assert!(log.contains("##[error]boom\n"), "{log}");
    assert!(
        log.contains("##[group]My Group\ninside\n##[endgroup]\n"),
        "{log}"
    );
    assert!(log.contains("::error::not-a-command\n"), "{log}");
    assert!(!log.contains("##[debug]"));
    assert_eq!(c.summary.as_deref(), Some("# Summary\nmore\n"));
}

#[tokio::test]
async fn working_directory_defaults_and_shells() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut s = spec(vec![
        Step {
            working_directory: Some(".".into()),
            run: Some("mkdir -p d/sub other".into()),
            ..Default::default()
        },
        run("echo \"cwd=$(pwd)\""),
        Step {
            working_directory: Some("other".into()),
            run: Some("echo \"cwd2=$(pwd)\"".into()),
            ..Default::default()
        },
        Step {
            shell: Some("bash".into()),
            run: Some("echo \"bash=$BASH_VERSION\" | cut -c1-5".into()),
            ..Default::default()
        },
        Step {
            shell: Some("sh -c 'echo custom; . {0}'".into()),
            working_directory: Some("d/sub".into()),
            run: Some("echo \"custom-ran $(basename $(pwd))\"".into()),
            ..Default::default()
        },
    ]);
    s.defaults = RunDefaults {
        shell: Some("sh".into()),
        working_directory: Some("d".into()),
    };
    // The default working directory must exist for the first step, so it
    // overrides it with ".".
    let ws = workspace(&e.cfg, &s);
    let have_python = executor::which("python").is_some();
    if have_python {
        s.steps.push(Step {
            shell: Some("python".into()),
            run: Some("print('py', 1 + 1)".into()),
            ..Default::default()
        });
    }
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    assert!(mock.log(2).contains("shell: sh -e {0}"));
    assert!(
        mock.log(3).contains(&format!("cwd={}/d\n", ws.display())),
        "{}",
        mock.log(3)
    );
    assert!(
        mock.log(4)
            .contains(&format!("cwd2={}/other\n", ws.display()))
    );
    assert!(mock.log(5).contains("bash="));
    assert!(
        mock.log(6).contains("custom\ncustom-ran sub\n"),
        "{}",
        mock.log(6)
    );
    if have_python {
        assert!(mock.log(7).contains("py 2"), "{}", mock.log(7));
    }
}

#[tokio::test]
async fn invalid_shell_fails_step() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let s = spec(vec![Step {
        shell: Some("fish".into()),
        run: Some("echo x".into()),
        ..Default::default()
    }]);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "failure");
    assert!(mock.log(2).contains("Invalid shell option 'fish'"));
}

#[tokio::test]
async fn step_timeout_fails_step() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut slow = run("echo started; sleep 20; echo finished");
    slow.timeout_minutes = Some(json!(0.005));
    let s = spec(vec![slow, cond(run("echo after"), "always()")]);
    let start = std::time::Instant::now();
    let c = exec(&mock, &e.cfg, s).await;
    assert!(start.elapsed() < Duration::from_secs(15));
    assert_eq!(c.conclusion, "failure");
    let log = mock.log(2);
    assert!(log.contains("started"));
    assert!(!log.contains("\nfinished\n"));
    assert!(log.contains("has timed out after"), "{log}");
    assert!(mock.log(3).contains("after"));
}

#[tokio::test]
async fn job_timeout_cancels_job() {
    let mut e = env();
    e.cfg.job_timeout = Some(Duration::from_millis(800));
    let mock = Arc::new(Mock::default());
    let s = spec(vec![
        run("sleep 20"),
        run("echo normal"),
        cond(run("echo cleanup-ran"), "always()"),
    ]);
    let start = std::time::Instant::now();
    let c = exec(&mock, &e.cfg, s).await;
    assert!(start.elapsed() < Duration::from_secs(15));
    assert_eq!(c.conclusion, "cancelled");
    let concl: Vec<String> = conclusions(&c).into_iter().map(|(_, c)| c).collect();
    assert_eq!(
        concl,
        vec!["success", "cancelled", "skipped", "success", "success"]
    );
    assert!(
        mock.all_logs()
            .contains("has exceeded the maximum execution time of"),
        "{}",
        mock.all_logs()
    );
    assert!(mock.log(4).contains("cleanup-ran"));
}

#[tokio::test]
async fn heartbeat_cancellation() {
    let e = env();
    let mock = Arc::new(Mock {
        cancel_when_running: Some(2),
        ..Default::default()
    });
    let s = spec(vec![
        run("echo running; sleep 30"),
        run("echo normal"),
        cond(
            run("echo cancelled-handler ${{ job.status }}"),
            "cancelled()",
        ),
    ]);
    let start = std::time::Instant::now();
    let c = exec(&mock, &e.cfg, s).await;
    assert!(start.elapsed() < Duration::from_secs(15));
    assert_eq!(c.conclusion, "cancelled");
    let concl: Vec<String> = conclusions(&c).into_iter().map(|(_, c)| c).collect();
    assert_eq!(
        concl,
        vec!["success", "cancelled", "skipped", "success", "success"]
    );
    assert!(mock.log(2).contains("The operation was canceled."));
    assert!(mock.log(4).contains("cancelled-handler cancelled"));
}

#[tokio::test]
async fn local_composite_action() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut step = uses("./.github/actions/comp", &[("who", "${{ matrix.os }}-bob")]);
    step.id = Some("c".into());
    step.env.insert("PARENT".into(), "from-parent".into());
    let s = spec(vec![
        step,
        run("echo \"result=${{ steps.c.outputs.greeting }} ${{ steps.c.outcome }}\""),
    ]);
    let ws = workspace(&e.cfg, &s);
    let dir = ws.join(".github/actions/comp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("action.yml"),
        r#"
name: comp
inputs:
  who:
    description: who
    default: world
  greeting-word:
    default: hello
outputs:
  greeting:
    value: ${{ steps.g.outputs.text }}
runs:
  using: composite
  steps:
    - id: g
      run: |
        echo "text=${{ inputs.greeting-word }} ${{ inputs.who }}" >> "$GITHUB_OUTPUT"
        echo "in composite $PARENT $(basename "$GITHUB_ACTION_PATH")"
      shell: bash
    - run: echo "second ${{ steps.g.outputs.text }}"
      shell: sh
    - if: failure()
      run: echo "should-not-run"
      shell: sh
"#,
    )
    .unwrap();
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    let log = mock.log(2);
    assert!(log.contains("in composite from-parent comp"), "{log}");
    assert!(log.contains("second hello linux-bob"), "{log}");
    assert!(!log.contains("should-not-run\n"), "{log}");
    assert!(
        mock.log(3).contains("result=hello linux-bob success"),
        "{}",
        mock.log(3)
    );
}

#[tokio::test]
async fn failing_composite_fails_step() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let s = spec(vec![uses("./act", &[])]);
    let ws = workspace(&e.cfg, &s);
    std::fs::create_dir_all(ws.join("act")).unwrap();
    std::fs::write(
        ws.join("act/action.yaml"),
        "runs:\n  using: composite\n  steps:\n    - run: exit 4\n      shell: sh\n    - run: echo cleanup-in-composite\n      if: always()\n      shell: sh\n",
    )
    .unwrap();
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "failure");
    let log = mock.log(2);
    assert!(log.contains("exit code 4"), "{log}");
    assert!(log.contains("cleanup-in-composite"), "{log}");
}

#[tokio::test]
async fn missing_local_action_fails() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let c = exec(&mock, &e.cfg, spec(vec![uses("./nope", &[])])).await;
    assert_eq!(c.conclusion, "failure");
    assert!(mock.log(2).contains("Can't find 'action.yml'"));
}

#[tokio::test]
async fn local_node_action_with_pre_and_post() {
    if executor::which("node").is_none() {
        eprintln!("skipping: node not installed");
        return;
    }
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut step = uses("./node-act", &[("name", "Rusty")]);
    step.id = Some("n".into());
    step.name = Some("Node thing".into());
    let s = spec(vec![step, run("echo \"out=${{ steps.n.outputs.out }}\"")]);
    let ws = workspace(&e.cfg, &s);
    let dir = ws.join("node-act");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("action.yml"),
        "inputs:\n  name:\n    required: true\n  greeting:\n    default: Hi ${{ github.actor }}\nruns:\n  using: node20\n  pre: pre.js\n  main: main.js\n  post: post.js\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("pre.js"),
        "const fs=require('fs'); console.log('pre ran'); fs.appendFileSync(process.env.GITHUB_STATE, 'fromPre=p1\\n');",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.js"),
        "const fs=require('fs');\n\
         console.log('main', process.env.INPUT_NAME, process.env.INPUT_GREETING, process.env.STATE_fromPre, require('path').basename(process.env.GITHUB_ACTION_PATH));\n\
         fs.appendFileSync(process.env.GITHUB_OUTPUT, 'out=node-ok\\n');\n\
         console.log('::save-state name=k::v1');\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("post.js"),
        "console.log('post state', process.env.STATE_k, process.env.STATE_fromPre, process.env.INPUT_NAME);",
    )
    .unwrap();
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    let names: Vec<String> = c.steps.iter().map(|s| s.name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "Set up job",
            "Node thing",
            "Run echo \"out=${{ steps.n.outputs.out }}\"",
            "Post Node thing",
            "Complete job"
        ]
        .into_iter()
        .map(|s| s.replace("${{ steps.n.outputs.out }}", "node-ok"))
        .collect::<Vec<_>>()
    );
    let log = mock.log(2);
    assert!(log.contains("pre ran"), "{log}");
    assert!(log.contains("main Rusty Hi octocat p1 node-act"), "{log}");
    assert!(mock.log(3).contains("out=node-ok"));
    assert!(
        mock.log(4).contains("post state v1 p1 Rusty"),
        "{}",
        mock.log(4)
    );
}

#[tokio::test]
async fn artifacts_roundtrip() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut up = uses(
        "actions/upload-artifact@v4",
        &[("name", "res"), ("path", "out\n!out/skip.txt")],
    );
    up.id = Some("up".into());
    let s = spec(vec![
        run("mkdir -p out/sub; echo a > out/a.txt; echo b > out/sub/b.txt; echo s > out/skip.txt"),
        up,
        uses(
            "actions/download-artifact@v4",
            &[("name", "res"), ("path", "dl")],
        ),
        uses("actions/download-artifact@v4", &[("path", "all")]),
        run(
            "cat dl/a.txt dl/sub/b.txt all/res/a.txt; test ! -e dl/skip.txt && echo no-skip; echo id=${{ steps.up.outputs.artifact-id }}",
        ),
        uses(
            "actions/upload-artifact@v4",
            &[("name", "none"), ("path", "missing/*.bin")],
        ),
    ]);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    assert_eq!(
        mock.log(6),
        "##[group]Run cat dl/a.txt dl/sub/b.txt all/res/a.txt; test ! -e dl/skip.txt && echo no-skip; echo id=100\ncat dl/a.txt dl/sub/b.txt all/res/a.txt; test ! -e dl/skip.txt && echo no-skip; echo id=100\nshell: bash --noprofile --norc -eo pipefail {0}\n##[endgroup]\na\nb\na\nno-skip\nid=100\n"
    );
    assert!(mock.log(7).contains("##[warning]No files were found"));
    assert_eq!(mock.artifacts.lock().unwrap().len(), 1);
    assert!(c.annotations.iter().any(|a| a.level == "warning"));
}

#[tokio::test]
async fn cache_is_a_noop() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let mut c1 = uses("actions/cache@v4", &[("path", "x"), ("key", "k")]);
    c1.id = Some("cache".into());
    let s = spec(vec![
        c1,
        run("echo hit=${{ steps.cache.outputs.cache-hit }}"),
    ]);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success");
    assert!(mock.log(2).contains("Caching is not supported"));
    assert!(mock.log(3).contains("hit=false"));
}

#[tokio::test]
async fn hash_files_in_expressions() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let s = spec(vec![
        run("mkdir -p sub; printf hi > sub/f.lock; printf x > other.txt"),
        run("echo \"h=${{ hashFiles('**/*.lock') }} e=${{ hashFiles('*.none') }}.\""),
    ]);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    let mut h = Sha256::new();
    h.update(Sha256::digest(b"hi"));
    let expected = hex::encode(h.finalize());
    assert!(
        mock.log(3).contains(&format!("h={expected} e=.")),
        "{}",
        mock.log(3)
    );
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@e",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test]
async fn checkout_from_local_server() {
    let e = env();
    let srv = tempfile::tempdir().unwrap();
    let bare = srv.path().join("octo/app.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]);
    let src = srv.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    std::fs::write(src.join("README.md"), "first\n").unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-q", "-m", "one"]);
    let first = git(&src, &["rev-parse", "HEAD"]);
    std::fs::write(src.join("README.md"), "second\n").unwrap();
    git(&src, &["commit", "-q", "-am", "two"]);
    let second = git(&src, &["rev-parse", "HEAD"]);
    git(&src, &["tag", "v1", &first]);
    git(&src, &["push", "-q", bare.to_str().unwrap(), "main", "v1"]);

    let mock = Arc::new(Mock::default());
    let mut s = spec(vec![
        uses("actions/checkout@v4", &[]),
        run(
            "cat README.md; git rev-parse --abbrev-ref HEAD; git rev-parse HEAD; git config --local --get-regexp 'http.*extraheader' | cut -c1-10",
        ),
        uses("actions/checkout@v4", &[("ref", "v1"), ("path", "tagged")]),
        run("cat tagged/README.md"),
        uses(
            "actions/checkout@v4",
            &[
                ("fetch-depth", "0"),
                ("path", "full"),
                ("persist-credentials", "false"),
            ],
        ),
        run(
            "cd full && git log --oneline | wc -l | tr -d ' ' && git config --local --get-regexp extraheader || echo no-creds",
        ),
    ]);
    s.server_url = format!("file://{}", srv.path().display());
    s.github["sha"] = json!(second);
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    let log = mock.log(3);
    assert!(
        log.contains(&format!("second\nmain\n{second}\nhttp.file:")),
        "{log}"
    );
    assert!(mock.log(5).contains("first"), "{}", mock.log(5));
    assert!(mock.log(7).contains("2\nno-creds"), "{}", mock.log(7));
    assert!(!mock.all_logs().contains("ghs_tokenvalue123"));
}

#[tokio::test]
async fn remote_action_from_server() {
    let e = env();
    let srv = tempfile::tempdir().unwrap();
    let bare = srv.path().join("acme/hello.git");
    std::fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]);
    let src = srv.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    git(&src, &["init", "-q"]);
    std::fs::write(
        src.join("sub/action.yml"),
        "inputs:\n  x:\n    default: dflt\nruns:\n  using: composite\n  steps:\n    - run: echo \"remote composite ${{ inputs.x }} $GITHUB_ACTION_REPOSITORY\"\n      shell: sh\n",
    )
    .unwrap();
    git(&src, &["add", "."]);
    git(&src, &["commit", "-q", "-m", "one"]);
    git(&src, &["tag", "v2"]);
    git(&src, &["push", "-q", bare.to_str().unwrap(), "main", "v2"]);

    let mock = Arc::new(Mock::default());
    let mut s = spec(vec![
        uses("acme/hello/sub@v2", &[]),
        uses("acme/hello/sub@main", &[("x", "given")]),
        uses("acme/missing@v1", &[]),
    ]);
    s.server_url = format!("file://{}", srv.path().display());
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "failure");
    assert!(
        mock.log(2).contains("remote composite dflt acme/hello"),
        "{}",
        mock.log(2)
    );
    assert!(
        mock.log(3).contains("remote composite given"),
        "{}",
        mock.log(3)
    );
    assert!(
        mock.log(4)
            .contains("Unable to resolve action 'acme/missing@v1'")
    );
    let concl: Vec<String> = conclusions(&c).into_iter().map(|(_, c)| c).collect();
    assert_eq!(
        concl,
        vec!["success", "success", "success", "failure", "success"]
    );
}

#[tokio::test]
async fn worker_loop_runs_one_job() {
    let e = env();
    let mock = Arc::new(Mock::default());
    mock.jobs
        .lock()
        .unwrap()
        .push(spec(vec![run("echo from-loop")]));
    let backend: Arc<dyn Backend> = mock.clone();
    tokio::time::timeout(
        Duration::from_secs(30),
        worker_loop(
            backend,
            Arc::new(e.cfg.clone()),
            2,
            CancellationToken::new(),
            true,
        ),
    )
    .await
    .expect("worker loop finished");
    let done = mock.completed.lock().unwrap();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].0, 42);
    assert_eq!(done[0].1.conclusion, "success");
    assert!(mock.log(2).contains("from-loop"));
}

#[tokio::test]
async fn worker_loop_stops_on_shutdown() {
    let e = env();
    let mock = Arc::new(Mock::default());
    let backend: Arc<dyn Backend> = mock.clone();
    let shutdown = CancellationToken::new();
    let h = tokio::spawn(worker_loop(
        backend,
        Arc::new(e.cfg.clone()),
        1,
        shutdown.clone(),
        false,
    ));
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), h)
        .await
        .expect("stopped")
        .unwrap();
}

async fn docker_ok() -> bool {
    if executor::docker_available("docker").await {
        true
    } else {
        eprintln!("skipping: docker not available");
        false
    }
}

#[tokio::test]
async fn docker_executor_with_service() {
    if !docker_ok().await {
        return;
    }
    let mut e = env();
    e.cfg.executor = ExecutorKind::Docker;
    e.cfg.default_image = "alpine:3".into();
    let mock = Arc::new(Mock::default());
    let mut s = spec(vec![
        run(
            "cat /etc/os-release; echo \"ws=$GITHUB_WORKSPACE pwd=$(pwd)\"; echo out=1 >> \"$GITHUB_OUTPUT\"; echo hello > file.txt",
        ),
        run(
            "ping -c1 -W2 svc >/dev/null 2>&1 && echo svc-reachable || (nslookup svc >/dev/null 2>&1 && echo svc-reachable)",
        ),
        run(
            "echo \"cid=${{ job.container.id != '' }} net=${{ job.container.network }} svc=${{ job.services.svc.id != '' }} file=$(cat file.txt)\"; echo \"$SECRET_ENV\"",
        ),
        uses(
            "docker://alpine:3",
            &[(
                "args",
                "sh -c 'echo from-docker-step $GITHUB_WORKSPACE $(cat file.txt)'",
            )],
        ),
        uses(
            "actions/upload-artifact@v4",
            &[("name", "d"), ("path", "file.txt")],
        ),
    ]);
    s.steps[2]
        .env
        .insert("SECRET_ENV".into(), "${{ secrets.S }}".into());
    s.secrets.insert("S".into(), "dockersecret".into());
    s.services.insert(
        "svc".into(),
        ContainerSpec {
            image: "alpine:3".into(),
            options: Some("-t".into()),
            ..Default::default()
        },
    );
    let c = exec(&mock, &e.cfg, s).await;
    assert_eq!(c.conclusion, "success", "{}", mock.all_logs());
    let log = mock.log(2);
    assert!(log.contains("Alpine"), "{log}");
    assert!(log.contains("shell: sh -e {0}"), "{log}");
    assert!(log.contains("ws=/__w/app/app pwd=/__w/app/app"), "{log}");
    assert!(mock.log(3).contains("svc-reachable"), "{}", mock.all_logs());
    let l4 = mock.log(4);
    assert!(
        l4.contains("cid=true net=bgh-job-42 svc=true file=hello"),
        "{l4}"
    );
    assert!(l4.contains("***") && !l4.contains("dockersecret"), "{l4}");
    assert!(
        mock.log(5)
            .contains("from-docker-step /github/workspace hello"),
        "{}",
        mock.log(5)
    );
    // Containers and network are gone, and so is the job directory.
    let ps = std::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "name=bgh-job-42",
            "--format",
            "{{.Names}}",
        ])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&ps.stdout).trim().is_empty());
    let net = std::process::Command::new("docker")
        .args(["network", "inspect", "bgh-job-42"])
        .output()
        .unwrap();
    assert!(!net.status.success());
    assert!(!e.cfg.work_dir.join("42").exists());
}

#[tokio::test]
async fn docker_executor_cancellation() {
    if !docker_ok().await {
        return;
    }
    let mut e = env();
    e.cfg.executor = ExecutorKind::Docker;
    e.cfg.default_image = "alpine:3".into();
    let mock = Arc::new(Mock {
        cancel_when_running: Some(2),
        ..Default::default()
    });
    let mut s = spec(vec![
        run("echo running; sleep 60"),
        cond(run("echo after-cancel"), "always()"),
    ]);
    s.job_id = 43;
    let start = std::time::Instant::now();
    let c = exec(&mock, &e.cfg, s).await;
    assert!(
        start.elapsed() < Duration::from_secs(40),
        "{:?}",
        start.elapsed()
    );
    assert_eq!(c.conclusion, "cancelled");
    assert!(mock.log(3).contains("after-cancel"));
}

#[tokio::test]
async fn detect_executor_setting() {
    assert_eq!(
        RunnerConfig::detect_executor("shell", "docker").await,
        Some(ExecutorKind::Shell)
    );
    assert_eq!(
        RunnerConfig::detect_executor("docker", "/nonexistent").await,
        Some(ExecutorKind::Docker)
    );
    // `auto` never falls back to running jobs on the host.
    assert_eq!(
        RunnerConfig::detect_executor("auto", "/nonexistent/docker").await,
        None
    );
}

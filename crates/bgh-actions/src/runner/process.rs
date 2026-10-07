//! Running a child process with merged stdout/stderr delivered line by
//! line, a deadline and cancellation (process-tree kill).

use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// How to stop a running process (and everything it started).
#[derive(Debug, Clone)]
pub enum KillMode {
    /// Signal the process group of the spawned process (it is spawned as a
    /// group leader).
    ProcessGroup,
    /// A `docker exec`: the process inside `container` wrote its pid to
    /// `pid_file` (a container path); signal it and its children.
    DockerExec {
        docker: String,
        container: String,
        pid_file: String,
    },
    /// A `docker run --name <name>`: kill / remove that container.
    DockerContainer { docker: String, name: String },
}

#[derive(Debug, Clone)]
pub struct ProcessSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Added on top of the allowlisted host environment ([`HOST_ENV`]).
    pub env: Vec<(String, String)>,
    /// Start from an empty environment instead of the allowlisted host
    /// variables. The runner's own environment (database URLs, `BGH_*`
    /// secrets, SMTP credentials, ...) is never inherited.
    pub env_clear: bool,
    pub cwd: Option<PathBuf>,
    pub kill: KillMode,
}

impl ProcessSpec {
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Self {
        ProcessSpec {
            program: program.into(),
            args,
            env: Vec::new(),
            env_clear: false,
            cwd: None,
            kill: KillMode::ProcessGroup,
        }
    }

    /// `program arg arg` for logs.
    pub fn display(&self) -> String {
        let mut s = self.program.clone();
        for a in &self.args {
            s.push(' ');
            s.push_str(&super::commands::shell_quote(a));
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutcome {
    Exited(i32),
    Cancelled,
    TimedOut,
    /// The process could not be started.
    Failed(String),
}

impl ProcessOutcome {
    pub fn success(&self) -> bool {
        *self == ProcessOutcome::Exited(0)
    }
}

/// Host environment variables a step (or docker CLI call) inherits from
/// the runner process; everything else is cleared. The job's own
/// environment is added on top.
pub const HOST_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TERM",
    "TMPDIR",
    // docker CLI (docker executor, `docker://` steps, services)
    "DOCKER_HOST",
    "DOCKER_CONFIG",
    "DOCKER_CONTEXT",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
    // outbound proxies and CA bundles
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

/// The allowlisted part of this process's environment ([`HOST_ENV`]).
pub fn host_env() -> Vec<(String, String)> {
    HOST_ENV
        .iter()
        .filter_map(|k| Some((k.to_string(), std::env::var(k).ok()?)))
        .collect()
}

/// Grace period between SIGTERM and SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(5);
/// After the main process exited, how long to wait for other holders of
/// the output pipe (background processes) before giving up on EOF.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(2);

async fn send_signal(kill: &KillMode, pid: Option<u32>, force: bool) {
    let sig = if force { "KILL" } else { "TERM" };
    match kill {
        KillMode::ProcessGroup => {
            if let Some(pid) = pid {
                let _ = tokio::process::Command::new("kill")
                    .args(["-s", sig, "--", &format!("-{pid}")])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;
            }
        }
        KillMode::DockerExec {
            docker,
            container,
            pid_file,
        } => {
            let script = format!(
                "p=$(cat {pf} 2>/dev/null); [ -n \"$p\" ] || exit 0; \
                 kill -{sig} -- -$p 2>/dev/null; pkill -{sig} -P $p 2>/dev/null; kill -{sig} $p 2>/dev/null; true",
                pf = super::commands::shell_quote(pid_file)
            );
            let _ = tokio::process::Command::new(docker)
                .args(["exec", container, "sh", "-c", &script])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
        KillMode::DockerContainer { docker, name } => {
            let args: Vec<&str> = if force {
                vec!["rm", "-f", name]
            } else {
                vec!["kill", "--signal", "TERM", name]
            };
            let _ = tokio::process::Command::new(docker)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
}

async fn sleep_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// Run `spec`, feeding every output line (stdout and stderr merged in
/// write order, without the line terminator) to `on_line`.
pub async fn run_process(
    spec: &ProcessSpec,
    on_line: &mut (dyn FnMut(&str) + Send),
    cancel: &CancellationToken,
    deadline: Option<Instant>,
) -> ProcessOutcome {
    if cancel.is_cancelled() {
        return ProcessOutcome::Cancelled;
    }
    let (reader, writer) = match std::io::pipe() {
        Ok(p) => p,
        Err(e) => return ProcessOutcome::Failed(format!("pipe: {e}")),
    };
    let writer2 = match writer.try_clone() {
        Ok(w) => w,
        Err(e) => return ProcessOutcome::Failed(format!("pipe: {e}")),
    };
    let mut cmd = tokio::process::Command::new(&spec.program);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::from(writer2))
        .process_group(0)
        .kill_on_drop(true);
    cmd.env_clear();
    if !spec.env_clear {
        cmd.envs(host_env());
    }
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    let spawned = cmd.spawn();
    // Close our copies of the pipe's write end so EOF arrives when the
    // child (and its descendants) exit.
    drop(cmd);
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            let cwd = spec
                .cwd
                .as_ref()
                .map(|c| c.display().to_string())
                .unwrap_or_default();
            return ProcessOutcome::Failed(format!(
                "An error occurred trying to start process '{}' with working directory '{}'. {}",
                spec.program, cwd, e
            ));
        }
    };
    let pid = child.id();

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let mut r = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    while matches!(buf.last(), Some(b'\n' | b'\r')) {
                        buf.pop();
                    }
                    if tx.send(String::from_utf8_lossy(&buf).into_owned()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    let mut exit: Option<i32> = None;
    let mut eof = false;
    let mut reason: Option<ProcessOutcome> = None;
    let mut force_at: Option<Instant> = None;
    let mut forced = false;
    let mut drain_until: Option<Instant> = None;
    loop {
        if exit.is_some() && eof {
            break;
        }
        tokio::select! {
            line = rx.recv(), if !eof => match line {
                Some(l) => on_line(&l),
                None => eof = true,
            },
            st = child.wait(), if exit.is_none() => {
                let code = match st {
                    Ok(st) => st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0)),
                    Err(_) => -1,
                };
                exit = Some(code);
                drain_until = Some(Instant::now() + DRAIN_AFTER_EXIT);
            }
            _ = cancel.cancelled(), if reason.is_none() && exit.is_none() => {
                reason = Some(ProcessOutcome::Cancelled);
                send_signal(&spec.kill, pid, false).await;
                force_at = Some(Instant::now() + KILL_GRACE);
            }
            _ = sleep_opt(deadline), if reason.is_none() && exit.is_none() && deadline.is_some() => {
                reason = Some(ProcessOutcome::TimedOut);
                send_signal(&spec.kill, pid, false).await;
                force_at = Some(Instant::now() + KILL_GRACE);
            }
            _ = sleep_opt(force_at), if !forced && force_at.is_some() && exit.is_none() => {
                forced = true;
                send_signal(&spec.kill, pid, true).await;
                if !matches!(spec.kill, KillMode::ProcessGroup) {
                    send_signal(&KillMode::ProcessGroup, pid, true).await;
                }
                let _ = child.start_kill();
            }
            _ = sleep_opt(drain_until), if drain_until.is_some() => break,
        }
    }
    // Kill leftovers of the process group (background jobs).
    if matches!(spec.kill, KillMode::ProcessGroup) && !eof {
        send_signal(&spec.kill, pid, true).await;
    }
    match reason {
        Some(r) => r,
        None => ProcessOutcome::Exited(exit.unwrap_or(-1)),
    }
}

/// Run a command and collect its merged output.
pub async fn capture(spec: &ProcessSpec, cancel: &CancellationToken) -> (ProcessOutcome, String) {
    let mut out = String::new();
    let mut on_line = |l: &str| {
        out.push_str(l);
        out.push('\n');
    };
    let outcome = run_process(spec, &mut on_line, cancel, None).await;
    (outcome, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_allowlisted_host_env_is_inherited() {
        // `cargo test` sets CARGO_* in this process; steps must not see it.
        assert!(std::env::var("CARGO_MANIFEST_DIR").is_ok());
        let mut spec = ProcessSpec::new("sh", vec!["-c".into(), "env".into()]);
        spec.env.push(("JOB_VAR".into(), "1".into()));
        let (outcome, out) = capture(&spec, &CancellationToken::new()).await;
        assert!(outcome.success());
        let names: Vec<&str> = out
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, _)| k)
            .collect();
        assert!(names.contains(&"PATH"), "{out}");
        assert!(names.contains(&"JOB_VAR"), "{out}");
        assert!(!names.iter().any(|k| k.starts_with("CARGO")), "{out}");
        for k in &names {
            assert!(
                HOST_ENV.contains(k)
                    || *k == "JOB_VAR"
                    || *k == "PWD"
                    || *k == "SHLVL"
                    || *k == "_",
                "unexpected {k}"
            );
        }
    }

    #[tokio::test]
    async fn merges_output_and_exit_code() {
        let spec = ProcessSpec::new(
            "sh",
            vec!["-c".into(), "echo a; echo b >&2; echo c; exit 3".into()],
        );
        let (outcome, out) = capture(&spec, &CancellationToken::new()).await;
        assert_eq!(outcome, ProcessOutcome::Exited(3));
        assert_eq!(out, "a\nb\nc\n");
    }

    #[tokio::test]
    async fn timeout_kills_process_tree() {
        let spec = ProcessSpec::new("sh", vec!["-c".into(), "sleep 30 & sleep 30".into()]);
        let start = std::time::Instant::now();
        let mut sink = |_: &str| {};
        let outcome = run_process(
            &spec,
            &mut sink,
            &CancellationToken::new(),
            Some(Instant::now() + Duration::from_millis(300)),
        )
        .await;
        assert_eq!(outcome, ProcessOutcome::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn spawn_failure() {
        let spec = ProcessSpec::new("/nonexistent/bin", vec![]);
        let (outcome, _) = capture(&spec, &CancellationToken::new()).await;
        assert!(matches!(outcome, ProcessOutcome::Failed(_)));
    }
}

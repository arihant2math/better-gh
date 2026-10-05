//! Executors: where step processes run (host shell or a docker job
//! container), job directory layout and container lifecycle.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use super::commands::split_words;
use super::process::{KillMode, ProcessOutcome, ProcessSpec, capture, run_process};
use super::{ExecutorKind, RunnerConfig};
use crate::protocol::{ContainerSpec, JobSpec};

/// Container path the job root is mounted at under docker.
pub const CONTAINER_ROOT: &str = "/__w";

/// Job directory layout. "guest" paths are what the job sees (identical to
/// host paths for the shell executor, `/__w/...` under docker).
#[derive(Debug, Clone)]
pub struct Paths {
    pub host_root: PathBuf,
    pub guest_root: String,
    pub repo_name: String,
}

fn strip_path_prefix<'a>(path: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = path.strip_prefix(prefix)?;
    if rest.is_empty() || rest.starts_with('/') {
        Some(rest.trim_start_matches('/'))
    } else {
        None
    }
}

impl Paths {
    pub fn guest(&self, rel: &str) -> String {
        if rel.is_empty() {
            self.guest_root.clone()
        } else {
            format!("{}/{}", self.guest_root, rel)
        }
    }

    pub fn host(&self, rel: &str) -> PathBuf {
        self.host_root.join(rel)
    }

    fn workspace_rel(&self) -> String {
        format!("{0}/{0}", self.repo_name)
    }

    pub fn guest_workspace(&self) -> String {
        self.guest(&self.workspace_rel())
    }

    pub fn host_workspace(&self) -> PathBuf {
        self.host(&self.workspace_rel())
    }

    /// `RUNNER_WORKSPACE`: the parent of GITHUB_WORKSPACE.
    pub fn guest_runner_workspace(&self) -> String {
        self.guest(&self.repo_name)
    }

    pub fn guest_temp(&self) -> String {
        self.guest("_temp")
    }

    pub fn host_temp(&self) -> PathBuf {
        self.host("_temp")
    }

    /// Map a guest path to the host. Paths outside the job root are
    /// returned unchanged (only meaningful for the shell executor).
    pub fn to_host(&self, guest: &str) -> PathBuf {
        match strip_path_prefix(guest, &self.guest_root) {
            Some("") => self.host_root.clone(),
            Some(rest) => self.host_root.join(rest),
            None => PathBuf::from(guest),
        }
    }

    pub fn to_guest(&self, host: &Path) -> String {
        match host.strip_prefix(&self.host_root) {
            Ok(rest) if rest.as_os_str().is_empty() => self.guest_root.clone(),
            Ok(rest) => format!("{}/{}", self.guest_root, rest.to_string_lossy()),
            Err(_) => host.to_string_lossy().into_owned(),
        }
    }

    /// Map a guest path to the path seen inside a `docker run` step
    /// container (job root mounted at `/__w`).
    pub fn to_container(&self, guest: &str) -> String {
        match strip_path_prefix(guest, &self.guest_root) {
            Some("") => CONTAINER_ROOT.to_string(),
            Some(rest) => format!("{CONTAINER_ROOT}/{rest}"),
            None => guest.to_string(),
        }
    }

    /// Resolve `p` (absolute or relative to `base`) as a guest path.
    pub fn resolve(&self, base: &str, p: &str) -> String {
        let p = p.trim();
        if p.starts_with('/') {
            normalize(p)
        } else if p.is_empty() || p == "." {
            base.to_string()
        } else {
            normalize(&format!("{base}/{p}"))
        }
    }
}

/// Lexically normalize an absolute `/`-path (`.`/`..`/duplicate slashes).
pub fn normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Environment variables that the docker CLI itself reads; they are passed
/// inline (`-e K=V`) instead of through the CLI's environment.
fn docker_cli_var(k: &str) -> bool {
    let u = k.to_ascii_uppercase();
    matches!(
        u.as_str(),
        "PATH" | "HOME" | "TMPDIR" | "LD_PRELOAD" | "LD_LIBRARY_PATH" | "NO_PROXY"
    ) || u.starts_with("DOCKER_")
        || u.ends_with("_PROXY")
        || u.starts_with("SSL_")
}

/// Append `-e` flags for `env` to `args`; values of most variables go
/// through the CLI's environment (`-e K`) so they never show up in process
/// listings.
pub fn docker_env_args(
    env: &IndexMap<String, String>,
    args: &mut Vec<String>,
    penv: &mut Vec<(String, String)>,
) {
    for (k, v) in env {
        if k.is_empty() || k.contains('=') {
            continue;
        }
        args.push("-e".into());
        if docker_cli_var(k) {
            args.push(format!("{k}={v}"));
        } else {
            args.push(k.clone());
            penv.push((k.clone(), v.clone()));
        }
    }
}

/// Find an executable in the host PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return p.is_file().then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// Whether `docker info` succeeds.
pub async fn docker_available(docker: &str) -> bool {
    let fut = tokio::process::Command::new(docker)
        .arg("info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(Duration::from_secs(20), fut).await,
        Ok(Ok(st)) if st.success()
    )
}

/// Containers / network to remove at the end of the job. Removal also
/// happens on drop (panic, abort) if [`Cleanup::run`] was not called.
#[derive(Debug, Default)]
pub struct Cleanup {
    pub docker: String,
    pub containers: Vec<String>,
    pub network: Option<String>,
}

impl Cleanup {
    pub async fn run(&mut self, log: &(dyn Fn(&str) + Sync)) {
        for c in std::mem::take(&mut self.containers).into_iter().rev() {
            log(&format!("Stop and remove container: {c}"));
            let _ = quiet(&self.docker, &["rm", "-f", "-v", &c]).await;
        }
        if let Some(n) = self.network.take() {
            log(&format!("Remove container network: {n}"));
            let _ = quiet(&self.docker, &["network", "rm", &n]).await;
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for c in self.containers.drain(..) {
            let _ = std::process::Command::new(&self.docker)
                .args(["rm", "-f", "-v", &c])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        if let Some(n) = self.network.take() {
            let _ = std::process::Command::new(&self.docker)
                .args(["network", "rm", &n])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

async fn quiet(docker: &str, args: &[&str]) -> Option<String> {
    let out = tokio::process::Command::new(docker)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The prepared execution environment of a job.
#[derive(Debug)]
pub struct Exec {
    pub docker: String,
    /// Whether `docker` can be used (for `docker://` steps and services).
    pub docker_ok: bool,
    /// Job container name (docker executor).
    pub container: Option<String>,
    pub container_id: Option<String>,
    pub network: Option<String>,
    /// `job.services` context.
    pub services: Map<String, Value>,
    pub has_bash: bool,
    /// Default PATH of the job environment.
    pub default_path: String,
    pub cleanup: Cleanup,
}

impl Exec {
    /// A process running `argv` in the job environment (host or job
    /// container), with `env` set and working directory `cwd` (guest).
    pub fn command(
        &self,
        argv: &[String],
        env: &IndexMap<String, String>,
        cwd: &str,
        paths: &Paths,
    ) -> ProcessSpec {
        match &self.container {
            None => {
                let mut spec = ProcessSpec::new(argv[0].clone(), argv[1..].to_vec());
                spec.env = env.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                spec.cwd = Some(paths.to_host(cwd));
                spec
            }
            Some(container) => {
                let pid_file = format!("{}/{}.pid", paths.guest_temp(), uuid::Uuid::new_v4());
                let mut args = vec!["exec".to_string(), "-i".into(), "-w".into(), cwd.into()];
                let mut penv = Vec::new();
                docker_env_args(env, &mut args, &mut penv);
                args.push(container.clone());
                args.push("sh".into());
                args.push("-c".into());
                args.push(format!(
                    "echo $$ > {}; exec \"$@\"",
                    super::commands::shell_quote(&pid_file)
                ));
                args.push("bgh-step".into());
                args.extend(argv.iter().cloned());
                let mut spec = ProcessSpec::new(self.docker.clone(), args);
                spec.env = penv;
                spec.kill = KillMode::DockerExec {
                    docker: self.docker.clone(),
                    container: container.clone(),
                    pid_file,
                };
                spec
            }
        }
    }

    /// `job.container` context.
    pub fn container_context(&self) -> Value {
        json!({
            "id": self.container_id.clone().unwrap_or_default(),
            "network": self.network.clone().unwrap_or_default(),
        })
    }
}

/// Logs a docker command and its output; returns stdout+stderr.
async fn docker_logged(
    docker: &str,
    args: Vec<String>,
    penv: Vec<(String, String)>,
    log: &(dyn Fn(&str) + Sync),
    cancel: &CancellationToken,
    show_output: bool,
) -> anyhow::Result<String> {
    let mut spec = ProcessSpec::new(docker, args);
    spec.env = penv;
    log(&format!("[command]{}", spec.display()));
    let mut out = String::new();
    let mut on_line = |l: &str| {
        if show_output {
            log(l);
        }
        out.push_str(l);
        out.push('\n');
    };
    match run_process(&spec, &mut on_line, cancel, None).await {
        ProcessOutcome::Exited(0) => Ok(out),
        ProcessOutcome::Exited(code) => {
            if !show_output {
                for l in out.lines() {
                    log(l);
                }
            }
            anyhow::bail!("Docker command failed with exit code {code}")
        }
        ProcessOutcome::Cancelled => anyhow::bail!("The operation was canceled."),
        ProcessOutcome::TimedOut => anyhow::bail!("Docker command timed out"),
        ProcessOutcome::Failed(e) => anyhow::bail!("{e}"),
    }
}

/// Registry host of an image reference (None for Docker Hub).
fn registry_of(image: &str) -> Option<&str> {
    let first = image.split('/').next()?;
    (image.contains('/') && (first.contains('.') || first.contains(':') || first == "localhost"))
        .then_some(first)
}

async fn docker_login(docker: &str, image: &str, c: &ContainerSpec, log: &(dyn Fn(&str) + Sync)) {
    let (Some(user), Some(pass)) = (&c.username, &c.password) else {
        return;
    };
    if user.is_empty() {
        return;
    }
    let mut args = vec![
        "login".to_string(),
        "--username".into(),
        user.clone(),
        "--password-stdin".into(),
    ];
    if let Some(r) = registry_of(image) {
        args.push(r.to_string());
    }
    log(&format!("[command]{docker} {}", args.join(" ")));
    let child = tokio::process::Command::new(docker)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = child else {
        log("##[warning]Docker login failed");
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(pass.as_bytes()).await;
    }
    match child.wait_with_output().await {
        Ok(o) if o.status.success() => {}
        _ => log("##[warning]Docker login failed"),
    }
}

/// Pull `image` unless it is present locally.
pub async fn ensure_image(
    docker: &str,
    image: &str,
    log: &(dyn Fn(&str) + Sync),
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    if quiet(docker, &["image", "inspect", "--format", "{{.Id}}", image])
        .await
        .is_some()
    {
        return Ok(());
    }
    docker_logged(
        docker,
        vec!["pull".into(), image.into()],
        Vec::new(),
        log,
        cancel,
        true,
    )
    .await
    .map(|_| ())
}

fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

async fn start_service(
    exec: &mut Exec,
    job_id: i64,
    id: &str,
    svc: &ContainerSpec,
    publish_only: bool,
    log: &(dyn Fn(&str) + Sync),
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let docker = exec.docker.clone();
    docker_login(&docker, &svc.image, svc, log).await;
    ensure_image(&docker, &svc.image, log, cancel).await?;
    let name = format!("bgh-job-{job_id}-{}", sanitize_name(id));
    let _ = quiet(&docker, &["rm", "-f", &name]).await;
    let mut args = vec![
        "run".to_string(),
        "-d".into(),
        "--name".into(),
        name.clone(),
    ];
    if let Some(net) = &exec.network {
        args.extend(["--network".into(), net.clone()]);
        if !publish_only {
            args.extend(["--network-alias".into(), id.to_string()]);
        }
    }
    let mut penv = Vec::new();
    docker_env_args(&svc.env, &mut args, &mut penv);
    for p in &svc.ports {
        args.extend(["-p".into(), p.clone()]);
    }
    for v in &svc.volumes {
        args.extend(["-v".into(), v.clone()]);
    }
    if let Some(o) = &svc.options {
        args.extend(split_words(o));
    }
    args.push(svc.image.clone());
    let out = docker_logged(&docker, args, penv, log, cancel, false).await?;
    exec.cleanup.containers.push(name.clone());
    let cid = out.lines().last().unwrap_or("").trim().to_string();
    // Port mappings.
    let mut ports = Map::new();
    if let Some(p) = quiet(&docker, &["port", &name]).await {
        for line in p.lines() {
            // 5432/tcp -> 0.0.0.0:49153
            if let Some((c, h)) = line.split_once("->") {
                let cport = c.trim().split('/').next().unwrap_or("").to_string();
                let hport = h.trim().rsplit(':').next().unwrap_or("").to_string();
                if !cport.is_empty() && !ports.contains_key(&cport) {
                    ports.insert(cport, Value::String(hport));
                }
            }
        }
    }
    // Wait for the health check, if the image / options define one.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        let health = quiet(
            &docker,
            &[
                "inspect",
                "--format",
                "{{if .State.Health}}{{.State.Health.Status}}{{end}}",
                &name,
            ],
        )
        .await
        .unwrap_or_default();
        match health.as_str() {
            "" => break,
            "healthy" => {
                log(&format!("{id} service is healthy."));
                break;
            }
            "unhealthy" => anyhow::bail!("Service container {id} failed."),
            _ => {}
        }
        if tokio::time::Instant::now() > deadline || cancel.is_cancelled() {
            anyhow::bail!("Service container {id} failed to become healthy.");
        }
        log(&format!("{id} service is starting, waiting..."));
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    exec.services.insert(
        id.to_string(),
        json!({
            "id": cid,
            "network": exec.network.clone().unwrap_or_default(),
            "ports": ports,
        }),
    );
    Ok(())
}

/// Prepare the executor: network, service containers, job container.
pub async fn setup(
    cfg: &RunnerConfig,
    spec: &JobSpec,
    paths: &Paths,
    log: &(dyn Fn(&str) + Sync),
    cancel: &CancellationToken,
) -> anyhow::Result<Exec> {
    let docker = cfg.docker_bin.clone();
    let docker_ok = match cfg.executor {
        ExecutorKind::Docker => true,
        ExecutorKind::Shell => which(&docker).is_some(),
    };
    let mut exec = Exec {
        docker: docker.clone(),
        docker_ok,
        container: None,
        container_id: None,
        network: None,
        services: Map::new(),
        has_bash: false,
        default_path: String::new(),
        cleanup: Cleanup {
            docker: docker.clone(),
            containers: Vec::new(),
            network: None,
        },
    };
    let need_network = cfg.executor == ExecutorKind::Docker || !spec.services.is_empty();
    if need_network {
        if cfg.executor == ExecutorKind::Shell && !docker_available(&docker).await {
            anyhow::bail!(
                "Service containers require docker, which is not available on this runner."
            );
        }
        let net = format!("bgh-job-{}", spec.job_id);
        let _ = quiet(&docker, &["network", "rm", &net]).await;
        log("##[group]Create local container network");
        let r = docker_logged(
            &docker,
            vec!["network".into(), "create".into(), net.clone()],
            Vec::new(),
            log,
            cancel,
            false,
        )
        .await;
        log("##[endgroup]");
        r?;
        exec.network = Some(net.clone());
        exec.cleanup.network = Some(net);
    }
    for (id, svc) in &spec.services {
        log(&format!("##[group]Starting {id} service container"));
        let r = start_service(
            &mut exec,
            spec.job_id,
            id,
            svc,
            cfg.executor == ExecutorKind::Shell,
            log,
            cancel,
        )
        .await;
        log("##[endgroup]");
        r?;
    }
    match cfg.executor {
        ExecutorKind::Shell => {
            if spec.container.is_some() {
                log(
                    "##[warning]This runner uses the shell executor; the job `container:` is ignored and steps run on the host.",
                );
            }
            exec.has_bash = which("bash").is_some();
            exec.default_path = std::env::var("PATH").unwrap_or_default();
        }
        ExecutorKind::Docker => {
            let c = spec.container.clone().unwrap_or_else(|| ContainerSpec {
                image: cfg.default_image.clone(),
                ..Default::default()
            });
            log("##[group]Starting job container");
            let r = start_job_container(&mut exec, cfg, spec, paths, &c, log, cancel).await;
            log("##[endgroup]");
            r?;
        }
    }
    Ok(exec)
}

async fn start_job_container(
    exec: &mut Exec,
    _cfg: &RunnerConfig,
    spec: &JobSpec,
    paths: &Paths,
    c: &ContainerSpec,
    log: &(dyn Fn(&str) + Sync),
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let docker = exec.docker.clone();
    docker_login(&docker, &c.image, c, log).await;
    ensure_image(&docker, &c.image, log, cancel).await?;
    let name = format!("bgh-job-{}", spec.job_id);
    let _ = quiet(&docker, &["rm", "-f", &name]).await;
    let mut args = vec![
        "run".to_string(),
        "-d".into(),
        "--name".into(),
        name.clone(),
    ];
    if let Some(net) = &exec.network {
        args.extend(["--network".into(), net.clone()]);
    }
    args.extend([
        "-v".into(),
        format!("{}:{}", paths.host_root.display(), CONTAINER_ROOT),
    ]);
    if Path::new("/var/run/docker.sock").exists() {
        args.extend([
            "-v".into(),
            "/var/run/docker.sock:/var/run/docker.sock".into(),
        ]);
    }
    args.extend(["-w".into(), paths.guest_workspace()]);
    for p in &c.ports {
        args.extend(["-p".into(), p.clone()]);
    }
    for v in &c.volumes {
        args.extend(["-v".into(), v.clone()]);
    }
    let mut penv = Vec::new();
    docker_env_args(&c.env, &mut args, &mut penv);
    if let Some(o) = &c.options {
        args.extend(split_words(o));
    }
    args.extend([
        "--entrypoint".into(),
        "tail".into(),
        c.image.clone(),
        "-f".into(),
        "/dev/null".into(),
    ]);
    let out = docker_logged(&docker, args, penv, log, cancel, false).await?;
    exec.cleanup.containers.push(name.clone());
    exec.container_id = Some(out.lines().last().unwrap_or("").trim().to_string());
    exec.container = Some(name.clone());

    let none = CancellationToken::new();
    let probe = |script: &str| {
        ProcessSpec::new(
            docker.clone(),
            vec![
                "exec".into(),
                name.clone(),
                "sh".into(),
                "-c".into(),
                script.to_string(),
            ],
        )
    };
    let (o, _) = capture(&probe("command -v bash"), &none).await;
    exec.has_bash = o.success();
    let (o, path) = capture(&probe("printf '%s\\n' \"$PATH\""), &none).await;
    if !o.success() {
        anyhow::bail!("The job container does not provide a usable `sh`.");
    }
    exec.default_path = path.trim().to_string();
    if exec.default_path.is_empty() {
        exec.default_path = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_mapping() {
        let p = Paths {
            host_root: PathBuf::from("/srv/work/7"),
            guest_root: "/__w".into(),
            repo_name: "app".into(),
        };
        assert_eq!(p.guest_workspace(), "/__w/app/app");
        assert_eq!(p.host_workspace(), PathBuf::from("/srv/work/7/app/app"));
        assert_eq!(
            p.to_host("/__w/_temp/x"),
            PathBuf::from("/srv/work/7/_temp/x")
        );
        assert_eq!(p.to_host("/__wx"), PathBuf::from("/__wx"));
        assert_eq!(p.to_guest(Path::new("/srv/work/7/_temp")), "/__w/_temp");
        assert_eq!(p.resolve("/__w/app/app", "sub/../x"), "/__w/app/app/x");
        assert_eq!(p.resolve("/__w/app/app", "/abs"), "/abs");
        assert_eq!(registry_of("ghcr.io/a/b:1"), Some("ghcr.io"));
        assert_eq!(registry_of("alpine:3"), None);
        assert_eq!(registry_of("library/alpine"), None);
    }
}

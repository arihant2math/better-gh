//! `bgh-runner`: external Better GitHub Actions runner.
//!
//! ```text
//! bgh-runner register --url https://bgh.example --token <registration token>
//! bgh-runner run
//! bgh-runner remove
//! bgh-runner run --jitconfig <encoded_jit_config>   # one job, no registration
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use bgh_actions::protocol::{Backend, RegisterRequest};
use bgh_actions::runner::http::{HttpBackend, register, unregister};
use bgh_actions::runner::{ExecutorKind, RunnerConfig, worker_loop};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(name = "bgh-runner", version, about = "Better GitHub Actions runner")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Register this runner with a server.
    Register {
        /// Server URL, e.g. https://bgh.example.com
        #[arg(long, env = "BGH_URL")]
        url: String,
        /// Registration token (from the repository / organization settings).
        #[arg(long, env = "BGH_RUNNER_REGISTRATION_TOKEN")]
        token: String,
        /// Runner name (default: host name).
        #[arg(long)]
        name: Option<String>,
        /// Extra labels, comma separated.
        #[arg(long, value_delimiter = ',')]
        labels: Vec<String>,
        /// Run a single job, then unregister.
        #[arg(long)]
        ephemeral: bool,
        /// Runner group (organization and site runners; default group if unset).
        #[arg(long)]
        runner_group: Option<String>,
        /// Reported OS (Linux, macOS, Windows; default: this host's).
        #[arg(long)]
        os: Option<String>,
        /// Reported architecture (X64, X86, ARM64, ARM; default: this host's).
        #[arg(long)]
        arch: Option<String>,
        #[arg(long, default_value = ".bgh-runner.json", env = "BGH_RUNNER_CONFIG")]
        config: PathBuf,
    },
    /// Run jobs until interrupted.
    Run {
        #[arg(long, default_value = ".bgh-runner.json", env = "BGH_RUNNER_CONFIG")]
        config: PathBuf,
        /// auto | docker | shell
        #[arg(long, default_value = "auto", env = "BGH_RUNNER_EXECUTOR")]
        executor: String,
        /// Maximum number of concurrent jobs.
        #[arg(long, default_value_t = 1)]
        max_jobs: usize,
        #[arg(long, default_value = "./_work")]
        work_dir: PathBuf,
        /// Docker image for jobs without `container:`.
        #[arg(long, default_value = "catthehacker/ubuntu:act-latest")]
        image: String,
        /// Exit after one job.
        #[arg(long)]
        once: bool,
        /// Do not fetch `owner/repo@ref` actions from github.com.
        #[arg(long)]
        no_remote_actions: bool,
        #[arg(long, default_value = "docker", env = "BGH_DOCKER")]
        docker: String,
        #[arg(long, default_value = "git", env = "BGH_GIT")]
        git: String,
        #[arg(long, default_value = "https://github.com")]
        github_url: String,
        /// `encoded_jit_config` from `generate-jitconfig`: run one job as
        /// that ephemeral runner (no `register` / config file needed).
        #[arg(long, env = "BGH_RUNNER_JITCONFIG")]
        jitconfig: Option<String>,
    },
    /// Unregister and delete the configuration.
    Remove {
        #[arg(long, default_value = ".bgh-runner.json", env = "BGH_RUNNER_CONFIG")]
        config: PathBuf,
    },
}

#[derive(Serialize, Deserialize)]
struct SavedConfig {
    url: String,
    id: i64,
    name: String,
    token: String,
    #[serde(default)]
    ephemeral: bool,
    /// Reported OS / architecture (`RUNNER_OS` / `RUNNER_ARCH`).
    #[serde(default)]
    os: Option<String>,
    #[serde(default)]
    arch: Option<String>,
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|h| !h.is_empty())
        })
        .unwrap_or_else(|| "bgh-runner".to_string())
}

fn load(path: &Path) -> anyhow::Result<SavedConfig> {
    let data = std::fs::read(path).with_context(|| {
        format!(
            "cannot read {} (register the runner first with `bgh-runner register`)",
            path.display()
        )
    })?;
    serde_json::from_slice(&data).with_context(|| format!("invalid config {}", path.display()))
}

fn save(path: &Path, cfg: &SavedConfig) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    f.write_all(&serde_json::to_vec_pretty(cfg)?)?;
    Ok(())
}

async fn shutdown_signal(token: CancellationToken) {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = async {
            match term.as_mut() {
                Some(t) => { t.recv().await; }
                None => std::future::pending::<()>().await,
            }
        } => {}
    }
    tracing::info!("shutting down: cancelling running jobs");
    token.cancel();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Register {
            url,
            token,
            name,
            labels,
            ephemeral,
            runner_group,
            os,
            arch,
            config,
        } => {
            let name = name.unwrap_or_else(hostname);
            let os = os.unwrap_or_else(|| bgh_actions::runner::host_os().to_string());
            let arch = arch.unwrap_or_else(|| bgh_actions::runner::host_arch().to_string());
            let labels = labels
                .into_iter()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();
            let resp = register(
                &url,
                RegisterRequest {
                    token,
                    name,
                    labels,
                    ephemeral,
                    os: Some(os.clone()),
                    arch: Some(arch.clone()),
                    runner_group,
                },
            )
            .await?;
            save(
                &config,
                &SavedConfig {
                    url,
                    id: resp.id,
                    name: resp.name.clone(),
                    token: resp.token,
                    ephemeral,
                    os: bgh_actions::runner::normalize_os(&os).map(String::from),
                    arch: bgh_actions::runner::normalize_arch(&arch).map(String::from),
                },
            )?;
            println!(
                "Runner '{}' (id {}) registered; configuration written to {}",
                resp.name,
                resp.id,
                config.display()
            );
        }
        Command::Run {
            config,
            executor,
            max_jobs,
            work_dir,
            image,
            once,
            no_remote_actions,
            docker,
            git,
            github_url,
            jitconfig,
        } => {
            let jit = jitconfig.is_some();
            let saved = match jitconfig {
                Some(enc) => {
                    let j = bgh_actions::protocol::JitConfig::decode(&enc)?;
                    SavedConfig {
                        url: j.server_url,
                        id: j.runner_id,
                        name: j.runner_name,
                        token: j.token,
                        ephemeral: true,
                        os: None,
                        arch: None,
                    }
                }
                None => load(&config)?,
            };
            let max_jobs = if saved.ephemeral { 1 } else { max_jobs };
            // A dedicated runner host may run jobs on itself (like GitHub's
            // runner); the built-in runner never does that implicitly.
            let kind = match RunnerConfig::detect_executor(&executor, &docker).await {
                Some(kind) => kind,
                None => {
                    tracing::warn!(
                        "docker is not available: running jobs with the shell executor on this host"
                    );
                    ExecutorKind::Shell
                }
            };
            std::fs::create_dir_all(&work_dir)
                .with_context(|| format!("cannot create {}", work_dir.display()))?;
            let work_dir = std::fs::canonicalize(&work_dir)?;
            tracing::info!(
                runner = %saved.name,
                executor = ?kind,
                work_dir = %work_dir.display(),
                "listening for jobs on {}",
                saved.url
            );
            let defaults = RunnerConfig::default();
            let cfg = Arc::new(RunnerConfig {
                name: saved.name.clone(),
                os: saved.os.clone().unwrap_or(defaults.os),
                arch: saved.arch.clone().unwrap_or(defaults.arch),
                work_dir,
                executor: kind,
                docker_bin: docker,
                default_image: image,
                git_bin: git,
                remote_actions: !no_remote_actions,
                github_url,
                ..RunnerConfig::default()
            });
            let backend: Arc<dyn Backend> = Arc::new(HttpBackend::new(&saved.url, &saved.token));
            let shutdown = CancellationToken::new();
            tokio::spawn(shutdown_signal(shutdown.clone()));
            worker_loop(backend, cfg, max_jobs, shutdown, once || saved.ephemeral).await;
            if saved.ephemeral {
                unregister(&saved.url, &saved.token).await?;
                if !jit {
                    let _ = std::fs::remove_file(&config);
                }
                tracing::info!("ephemeral runner unregistered");
            }
        }
        Command::Remove { config } => {
            let saved = load(&config)?;
            if let Err(e) = unregister(&saved.url, &saved.token).await {
                bail!("{e:#}");
            }
            std::fs::remove_file(&config)?;
            println!("Runner '{}' removed", saved.name);
        }
    }
    Ok(())
}

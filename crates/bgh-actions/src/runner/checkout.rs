//! Native `actions/checkout`: runs git on the host (the workspace is bind
//! mounted into the job container under docker).

use indexmap::IndexMap;

use super::job::{Body, JobRunner, StepRun, basic_auth};

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl JobRunner {
    pub(super) async fn checkout(
        &mut self,
        run: &StepRun,
        with: &IndexMap<String, String>,
    ) -> Body {
        match self.checkout_inner(run, with).await {
            Ok(outputs) => {
                let mut b = Body::success();
                b.outputs = outputs;
                b
            }
            Err(e) => {
                self.log(run.log_step, &format!("##[error]{e}"));
                Body::failure()
            }
        }
    }

    async fn checkout_inner(
        &mut self,
        run: &StepRun,
        with: &IndexMap<String, String>,
    ) -> Result<IndexMap<String, String>, String> {
        let get = |k: &str| {
            with.get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let flag = |k: &str| get(k).map(|v| v != "false").unwrap_or(true);
        let repository = get("repository").unwrap_or_else(|| self.spec.repository.clone());
        let token = get("token").unwrap_or_else(|| self.spec.token.clone());
        if !token.is_empty() {
            self.masker.add(&token);
            self.masker.add(&basic_auth(&token));
        }
        let depth: u64 = match get("fetch-depth") {
            Some(d) => d
                .parse()
                .map_err(|_| format!("Invalid fetch-depth '{d}'"))?,
            None => 1,
        };
        let clean = flag("clean");
        let persist = flag("persist-credentials");
        let ws = self.paths.guest_workspace();
        let guest_dir = self
            .paths
            .resolve(&ws, get("path").as_deref().unwrap_or(""));
        let dir = self.paths.to_host(&guest_dir);
        let server = self.spec.server_url.trim_end_matches('/').to_string();
        let url = format!("{server}/{repository}.git");
        let same_repo = repository.eq_ignore_ascii_case(&self.spec.repository);
        let gstr = |k: &str| {
            self.spec
                .github
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let (mut git_ref, mut sha) = match get("ref") {
            None if same_repo => {
                let s = gstr("sha");
                (gstr("ref"), (!s.is_empty()).then_some(s))
            }
            None => (String::new(), None),
            Some(r) if is_sha(&r) => (String::new(), Some(r)),
            Some(r) => (r, None),
        };
        let step = run.log_step;
        let cancel = run.cancel.clone();
        self.log(step, &format!("Syncing repository: {repository}"));
        self.log(step, &format!("Working directory is '{guest_dir}'"));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
        let d = Some(dir.as_path());

        if dir.join(".git").exists() {
            if clean {
                let _ = self
                    .git(
                        vec!["clean".into(), "-ffdx".into()],
                        d,
                        step,
                        &cancel,
                        false,
                    )
                    .await;
                let _ = self
                    .git(
                        vec!["reset".into(), "--hard".into(), "HEAD".into()],
                        d,
                        step,
                        &cancel,
                        false,
                    )
                    .await;
            }
            self.git(
                vec![
                    "remote".into(),
                    "set-url".into(),
                    "origin".into(),
                    url.clone(),
                ],
                d,
                step,
                &cancel,
                false,
            )
            .await
            .or_else(|_| Ok::<_, String>(String::new()))?;
            let _ = self
                .git(
                    vec!["remote".into(), "add".into(), "origin".into(), url.clone()],
                    d,
                    step,
                    &cancel,
                    true,
                )
                .await;
        } else {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    let _ = if p.is_dir() && !p.is_symlink() {
                        std::fs::remove_dir_all(&p)
                    } else {
                        std::fs::remove_file(&p)
                    };
                }
            }
            self.log(step, "##[group]Initializing the repository");
            let r = async {
                self.git(vec!["init".into(), "-q".into()], d, step, &cancel, false)
                    .await?;
                self.git(
                    vec!["remote".into(), "add".into(), "origin".into(), url.clone()],
                    d,
                    step,
                    &cancel,
                    false,
                )
                .await
            }
            .await;
            self.log(step, "##[endgroup]");
            r?;
        }

        let auth = self.git_auth_args(&token);
        let depth_args: Vec<String> = if depth > 0 {
            vec![format!("--depth={depth}")]
        } else {
            Vec::new()
        };
        let fetch = |refspecs: Vec<String>, tags: bool| {
            let mut a = auth.clone();
            a.extend([
                "-c".to_string(),
                "protocol.version=2".to_string(),
                "fetch".into(),
                if tags { "--tags" } else { "--no-tags" }.into(),
                "--prune".into(),
                "--no-recurse-submodules".into(),
            ]);
            a.extend(depth_args.iter().cloned());
            a.push("origin".into());
            a.extend(refspecs);
            a
        };
        let all_refs = vec![
            "+refs/heads/*:refs/remotes/origin/*".to_string(),
            "+refs/tags/*:refs/tags/*".to_string(),
        ];

        self.log(step, "##[group]Fetching the repository");
        let fetch_result: Result<(), String> = async {
            if depth == 0 {
                self.git(fetch(all_refs.clone(), true), d, step, &cancel, false)
                    .await?;
            }
            if git_ref.is_empty() && sha.is_none() {
                // Default branch of another repository.
                let out = self
                    .git(
                        {
                            let mut a = auth.clone();
                            a.extend([
                                "ls-remote".into(),
                                "--symref".into(),
                                "origin".into(),
                                "HEAD".into(),
                            ]);
                            a
                        },
                        d,
                        step,
                        &cancel,
                        true,
                    )
                    .await?;
                git_ref = out
                    .lines()
                    .find_map(|l| l.strip_prefix("ref: "))
                    .and_then(|l| l.split_whitespace().next())
                    .unwrap_or("refs/heads/main")
                    .to_string();
            }
            if let Some(s) = sha.clone() {
                let target = if let Some(b) = git_ref.strip_prefix("refs/heads/") {
                    format!("refs/remotes/origin/{b}")
                } else if let Some(t) = git_ref.strip_prefix("refs/tags/") {
                    format!("refs/tags/{t}")
                } else if let Some(p) = git_ref.strip_prefix("refs/pull/") {
                    format!("refs/remotes/pull/{}", p.trim_end_matches("/head"))
                } else {
                    "refs/remotes/origin/bgh-checkout".to_string()
                };
                let r = self
                    .git(
                        fetch(vec![format!("+{s}:{target}")], false),
                        d,
                        step,
                        &cancel,
                        false,
                    )
                    .await;
                if r.is_err() {
                    self.git(fetch(all_refs.clone(), true), d, step, &cancel, false)
                        .await?;
                }
                return Ok(());
            }
            // A ref without a commit: branch, tag or other ref.
            let name = git_ref.clone();
            let candidates: Vec<(String, String)> =
                if let Some(b) = name.strip_prefix("refs/heads/") {
                    vec![(
                        format!("+refs/heads/{b}:refs/remotes/origin/{b}"),
                        format!("refs/remotes/origin/{b}"),
                    )]
                } else if let Some(t) = name.strip_prefix("refs/tags/") {
                    vec![(
                        format!("+refs/tags/{t}:refs/tags/{t}"),
                        format!("refs/tags/{t}"),
                    )]
                } else if name.starts_with("refs/") {
                    vec![(format!("+{name}:{name}"), name.clone())]
                } else {
                    vec![
                        (
                            format!("+refs/heads/{name}:refs/remotes/origin/{name}"),
                            format!("refs/remotes/origin/{name}"),
                        ),
                        (
                            format!("+refs/tags/{name}:refs/tags/{name}"),
                            format!("refs/tags/{name}"),
                        ),
                    ]
                };
            let mut last_err = String::new();
            for (refspec, local) in candidates {
                match self
                    .git(fetch(vec![refspec], false), d, step, &cancel, false)
                    .await
                {
                    Ok(_) => {
                        let out = self
                            .git(
                                vec!["rev-parse".into(), format!("{local}^{{commit}}")],
                                d,
                                step,
                                &cancel,
                                true,
                            )
                            .await?;
                        sha = Some(out.trim().to_string());
                        if local.starts_with("refs/remotes/origin/") && !name.starts_with("refs/") {
                            git_ref = format!("refs/heads/{name}");
                        } else if local.starts_with("refs/tags/") && !name.starts_with("refs/") {
                            git_ref = format!("refs/tags/{name}");
                        }
                        return Ok(());
                    }
                    Err(e) => last_err = e,
                }
            }
            Err(format!(
                "A branch or tag with the name '{name}' could not be found ({last_err})"
            ))
        }
        .await;
        self.log(step, "##[endgroup]");
        fetch_result?;
        let sha = sha.ok_or("Unable to determine the commit to check out")?;

        self.log(step, "##[group]Checking out the ref");
        let checkout_args = match git_ref.strip_prefix("refs/heads/") {
            Some(b) => vec![
                "checkout".to_string(),
                "--progress".into(),
                "--force".into(),
                "-B".into(),
                b.to_string(),
                sha.clone(),
            ],
            None => vec![
                "-c".to_string(),
                "advice.detachedHead=false".into(),
                "checkout".into(),
                "--progress".into(),
                "--force".into(),
                sha.clone(),
            ],
        };
        let r = self.git(checkout_args, d, step, &cancel, false).await;
        self.log(step, "##[endgroup]");
        r?;

        if persist && !token.is_empty() {
            self.git(
                vec![
                    "config".into(),
                    "--local".into(),
                    format!("http.{server}/.extraheader"),
                    format!("AUTHORIZATION: basic {}", basic_auth(&token)),
                ],
                d,
                step,
                &cancel,
                false,
            )
            .await?;
        }
        let head = self
            .git(
                vec!["log".into(), "-1".into(), "--format=%H".into()],
                d,
                step,
                &cancel,
                false,
            )
            .await?;
        let mut outputs = IndexMap::new();
        outputs.insert("ref".to_string(), git_ref);
        outputs.insert("commit".to_string(), head.trim().to_string());
        Ok(outputs)
    }
}

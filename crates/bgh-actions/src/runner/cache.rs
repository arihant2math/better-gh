//! Native `actions/cache`, `actions/cache/restore` and `actions/cache/save`.
//!
//! Archives are made and extracted with `tar -z` *inside the job
//! environment* (so `~/.npm` means the job container's home), in the same
//! layout as `@actions/cache` (`-P`, paths relative to the workspace, gzip),
//! and the version hash is `@actions/cache`'s for gzip, so entries are
//! interchangeable with toolkit-based actions using gzip. Entries are
//! looked up, reserved, uploaded and committed through the legacy cache
//! protocol (`ACTIONS_CACHE_URL`, [`crate::cache::v1`]) with the job's
//! runtime token, exactly like a JS action would.
//!
//! Like the real action, cache service failures are warnings (a miss, or
//! "not saved"), never step failures — except `fail-on-cache-miss`.

use std::path::PathBuf;

use indexmap::IndexMap;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::job::{Body, JobRunner, PostKind, PostStep, Scope, StepRun};

/// Upload chunk size (the toolkit's default).
const CHUNK_SIZE: usize = 32 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheMode {
    /// `actions/cache`: restore now, save in a post step.
    Main,
    Restore,
    Save,
}

/// `@actions/cache` `getCacheVersion(paths, gzip)` on Linux.
pub(super) fn cache_version(paths: &[String]) -> String {
    let mut components: Vec<&str> = paths.iter().map(String::as_str).collect();
    components.push("gzip");
    components.push("1.0");
    hex::encode(Sha256::digest(components.join("|").as_bytes()))
}

fn lines(v: Option<&String>) -> Vec<String> {
    v.map(|s| {
        s.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    })
    .unwrap_or_default()
}

fn flag(with: &IndexMap<String, String>, k: &str) -> bool {
    with.get(k)
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
}

const CREATE_SCRIPT: &str = r#"ws="$1"; list="$2"; manifest="$3"; archive="$4"
cd "$ws"
if [ -n "$BASH_VERSION" ]; then shopt -s globstar nullglob dotglob; fi
: > "$manifest"
: > "$manifest.exclude"
IFS='
'
while IFS= read -r p || [ -n "$p" ]; do
  case "$p" in
    '') continue ;;
    '!'*) printf '%s\n' "${p#!}" >> "$manifest.exclude"; continue ;;
    '~') p="$HOME" ;;
    '~/'*) p="$HOME/${p#??}" ;;
  esac
  for f in $p; do
    if [ -e "$f" ] || [ -L "$f" ]; then printf '%s\n' "$f" >> "$manifest"; fi
  done
done < "$list"
if [ -s "$manifest" ]; then
  tar -czf "$archive" -P --exclude-from "$manifest.exclude" --files-from "$manifest"
fi
"#;

const EXTRACT_SCRIPT: &str = r#"cd "$1"
tar -xzf "$2" -P
"#;

/// A cache entry found by a lookup.
struct Found {
    key: String,
    archive_location: String,
}

impl JobRunner {
    /// `ACTIONS_*` toolkit runtime variables of every step.
    pub(super) fn add_runtime_env(&self, e: &mut IndexMap<String, String>) {
        if self.spec.runtime_token.is_empty() {
            return;
        }
        for (k, v) in crate::runtime::job_env(&self.spec.server_url, &self.spec.runtime_token) {
            e.insert(k.to_string(), v);
        }
    }

    fn cache_api(&self, path: &str) -> String {
        format!(
            "{}{}_apis/artifactcache/{path}",
            self.spec.server_url.trim_end_matches('/'),
            crate::runtime::RUNTIME_PREFIX
        )
    }

    fn cache_client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .user_agent("bgh-runner (actions/cache)")
            .build()
            .unwrap_or_default()
    }

    pub(super) async fn cache_action(
        &mut self,
        scope: &Scope,
        run: &StepRun,
        with: &IndexMap<String, String>,
        mode: CacheMode,
    ) -> Body {
        let paths = lines(with.get("path"));
        let key = with
            .get("key")
            .map(|k| k.trim().to_string())
            .unwrap_or_default();
        if paths.is_empty() {
            self.log(
                run.log_step,
                "##[error]Input required and not supplied: path",
            );
            return Body::failure();
        }
        if key.is_empty() {
            self.log(
                run.log_step,
                "##[error]Input required and not supplied: key",
            );
            return Body::failure();
        }
        if self.spec.runtime_token.is_empty() {
            self.log(
                run.log_step,
                "##[warning]Cache service is not available for this job (no runtime token); continuing without cache",
            );
            return Body::success();
        }
        if mode == CacheMode::Save {
            self.cache_save(scope, run, &paths, &key).await;
            return Body::success();
        }
        let restore_keys = lines(with.get("restore-keys"));
        let mut body = Body::success();
        if mode == CacheMode::Restore {
            body.outputs.insert("cache-primary-key".into(), key.clone());
        }
        body.state.insert("CACHE_KEY".into(), key.clone());
        let result = self
            .cache_restore(
                scope,
                run,
                &paths,
                &key,
                &restore_keys,
                flag(with, "lookup-only"),
            )
            .await;
        match result {
            Ok(Some(matched)) => {
                body.outputs
                    .insert("cache-hit".into(), (matched == key).to_string());
                if mode == CacheMode::Restore {
                    body.outputs
                        .insert("cache-matched-key".into(), matched.clone());
                }
                body.state.insert("CACHE_RESULT".into(), matched);
            }
            Ok(None) => {
                let mut all = vec![key.clone()];
                all.extend(restore_keys.iter().cloned());
                let msg = format!("Cache not found for input keys: {}", all.join(", "));
                if flag(with, "fail-on-cache-miss") {
                    self.log(
                        run.log_step,
                        &format!(
                            "##[error]Failed to restore cache entry. Exiting as fail-on-cache-miss is set. Input key: {key}"
                        ),
                    );
                    return Body::failure();
                }
                self.log(run.log_step, &msg);
            }
            Err(e) => {
                if flag(with, "fail-on-cache-miss") {
                    self.log(run.log_step, &format!("##[error]Failed to restore: {e}"));
                    return Body::failure();
                }
                self.log(run.log_step, &format!("##[warning]Failed to restore: {e}"));
            }
        }
        if mode == CacheMode::Main {
            let cond = if flag(with, "save-always") {
                "always()"
            } else {
                "success()"
            };
            body.post = Some(PostStep {
                name: run.display.clone(),
                cond: cond.to_string(),
                kind: PostKind::CacheSave {
                    paths: paths.clone(),
                    key: key.clone(),
                },
                env: IndexMap::new(),
                state: IndexMap::new(),
                action_name: run.action_name.clone(),
                action_path: scope.action_path.clone(),
                action_repository: scope.action_repository.clone(),
                inputs: scope.inputs.clone(),
            });
        }
        body
    }

    /// Post step of `actions/cache`: save unless the primary key hit.
    pub(super) async fn cache_post_save(
        &mut self,
        scope: &Scope,
        run: &StepRun,
        paths: &[String],
        key: &str,
        state: &IndexMap<String, String>,
    ) -> Body {
        if state.get("CACHE_RESULT").is_some_and(|m| m == key) {
            self.log(
                run.log_step,
                &format!("Cache hit occurred on the primary key {key}, not saving cache."),
            );
            return Body::success();
        }
        self.cache_save(scope, run, paths, key).await;
        Body::success()
    }

    async fn cache_lookup(&self, keys: &[String], version: &str) -> Result<Option<Found>, String> {
        let url = reqwest::Url::parse_with_params(
            &self.cache_api("cache"),
            &[("keys", keys.join(",")), ("version", version.to_string())],
        )
        .map_err(|e| e.to_string())?;
        let resp = self
            .cache_client()
            .get(url)
            .bearer_auth(&self.spec.runtime_token)
            .header("Accept", "application/json;api-version=6.0-preview.1")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        match resp.status().as_u16() {
            204 => Ok(None),
            200 => {
                let v: Value = resp.json().await.map_err(|e| e.to_string())?;
                let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
                match (s("cacheKey"), s("archiveLocation")) {
                    (Some(key), Some(archive_location)) => Ok(Some(Found {
                        key,
                        archive_location,
                    })),
                    _ => Ok(None),
                }
            }
            code => Err(format!(
                "Cache service responded with {code}: {}",
                resp.text().await.unwrap_or_default()
            )),
        }
    }

    /// A fresh temp directory (guest path, host path).
    fn cache_temp(&self) -> (String, PathBuf) {
        let name = format!("cache-{}", uuid::Uuid::new_v4());
        let guest = format!("{}/{name}", self.paths.guest_temp());
        let host = self.paths.host_temp().join(&name);
        (guest, host)
    }

    /// Run one of the tar scripts in the job environment.
    async fn cache_script(
        &mut self,
        scope: &Scope,
        run: &StepRun,
        host_dir: &std::path::Path,
        guest_dir: &str,
        script: &str,
        args: &[String],
    ) -> bool {
        let script_name = "cache.sh";
        if let Err(e) = std::fs::write(host_dir.join(script_name), script) {
            self.log(run.log_step, &format!("##[warning]{e}"));
            return false;
        }
        let has_bash = self.exec.as_ref().is_some_and(|e| e.has_bash);
        let mut argv: Vec<String> = if has_bash {
            vec![
                "bash".into(),
                "--noprofile".into(),
                "--norc".into(),
                "-e".into(),
            ]
        } else {
            vec!["sh".into(), "-e".into()]
        };
        argv.push(format!("{guest_dir}/{script_name}"));
        argv.extend(args.iter().cloned());
        let ws = self.paths.guest_workspace();
        let env = self.process_env(scope, run, &IndexMap::new());
        let Some(exec) = self.exec.as_ref() else {
            return false;
        };
        let pspec = exec.command(&argv, &env, &ws, &self.paths);
        let (outcome, _) = self
            .exec_process(&pspec, run.log_step, run.deadline, &run.cancel)
            .await;
        matches!(outcome, super::process::ProcessOutcome::Exited(0))
    }

    async fn cache_restore(
        &mut self,
        scope: &Scope,
        run: &StepRun,
        paths: &[String],
        key: &str,
        restore_keys: &[String],
        lookup_only: bool,
    ) -> Result<Option<String>, String> {
        let mut keys = vec![key.to_string()];
        keys.extend(restore_keys.iter().cloned());
        if keys.len() > crate::cache::MAX_KEYS {
            return Err(format!(
                "Key Validation Error: Keys are limited to a maximum of {}.",
                crate::cache::MAX_KEYS
            ));
        }
        for k in &keys {
            crate::cache::validate_key(k)?;
        }
        let version = cache_version(paths);
        let Some(found) = self.cache_lookup(&keys, &version).await? else {
            return Ok(None);
        };
        if lookup_only {
            self.log(
                run.log_step,
                &format!("Cache found and can be restored from key: {}", found.key),
            );
            return Ok(Some(found.key));
        }
        let (guest_dir, host_dir) = self.cache_temp();
        std::fs::create_dir_all(&host_dir).map_err(|e| e.to_string())?;
        let result = async {
            let archive = host_dir.join("cache.tgz");
            let size = download(&self.cache_client(), &found.archive_location, &archive).await?;
            self.log(
                run.log_step,
                &format!("Cache Size: ~{} MB ({size} B)", size.div_ceil(1 << 20)),
            );
            let ws = self.paths.guest_workspace();
            let ok = self
                .cache_script(
                    scope,
                    run,
                    &host_dir,
                    &guest_dir,
                    EXTRACT_SCRIPT,
                    &[ws, format!("{guest_dir}/cache.tgz")],
                )
                .await;
            if !ok {
                return Err("extracting the cache archive failed".to_string());
            }
            self.log(
                run.log_step,
                &format!(
                    "Cache restored successfully\nCache restored from key: {}",
                    found.key
                ),
            );
            Ok(Some(found.key.clone()))
        }
        .await;
        let _ = std::fs::remove_dir_all(&host_dir);
        result
    }

    /// Archive `paths` and save them under `key`. Problems are warnings.
    async fn cache_save(&mut self, scope: &Scope, run: &StepRun, paths: &[String], key: &str) {
        if let Err(e) = crate::cache::validate_key(key) {
            self.log(run.log_step, &format!("##[warning]Failed to save: {e}"));
            return;
        }
        let (guest_dir, host_dir) = self.cache_temp();
        if let Err(e) = std::fs::create_dir_all(&host_dir) {
            self.log(run.log_step, &format!("##[warning]Failed to save: {e}"));
            return;
        }
        let result = self
            .cache_save_inner(scope, run, paths, key, &guest_dir, &host_dir)
            .await;
        let _ = std::fs::remove_dir_all(&host_dir);
        match result {
            Ok(true) => self.log(run.log_step, &format!("Cache saved with key: {key}")),
            Ok(false) => {}
            Err(e) => self.log(run.log_step, &format!("##[warning]Failed to save: {e}")),
        }
    }

    async fn cache_save_inner(
        &mut self,
        scope: &Scope,
        run: &StepRun,
        paths: &[String],
        key: &str,
        guest_dir: &str,
        host_dir: &std::path::Path,
    ) -> Result<bool, String> {
        std::fs::write(host_dir.join("paths.txt"), paths.join("\n") + "\n")
            .map_err(|e| e.to_string())?;
        let ws = self.paths.guest_workspace();
        let ok = self
            .cache_script(
                scope,
                run,
                host_dir,
                guest_dir,
                CREATE_SCRIPT,
                &[
                    ws,
                    format!("{guest_dir}/paths.txt"),
                    format!("{guest_dir}/manifest.txt"),
                    format!("{guest_dir}/cache.tgz"),
                ],
            )
            .await;
        if !ok {
            return Err("creating the cache archive failed".into());
        }
        let archive = host_dir.join("cache.tgz");
        let Ok(meta) = std::fs::metadata(&archive) else {
            self.log(
                run.log_step,
                "##[warning]Path Validation Error: Path(s) specified in the action for caching do(es) not exist, hence no cache is being saved.",
            );
            return Ok(false);
        };
        let size = meta.len();
        self.log(
            run.log_step,
            &format!("Cache Size: ~{} MB ({size} B)", size.div_ceil(1 << 20)),
        );
        let client = self.cache_client();
        let version = cache_version(paths);
        let resp = client
            .post(self.cache_api("caches"))
            .bearer_auth(&self.spec.runtime_token)
            .header("Accept", "application/json;api-version=6.0-preview.1")
            .json(&serde_json::json!({"key": key, "version": version, "cacheSize": size}))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        let cache_id = match (status, v.get("cacheId").and_then(Value::as_i64)) {
            (201, Some(id)) => id,
            (400, _) => {
                return Err(v
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the cache is too large")
                    .to_string());
            }
            _ => {
                return Err(format!(
                    "Unable to reserve cache with key {key}, another job may be creating this cache. More details: {}",
                    v.get("message").and_then(Value::as_str).unwrap_or("")
                ));
            }
        };
        let url = self.cache_api(&format!("caches/{cache_id}"));
        let mut file = tokio::fs::File::open(&archive)
            .await
            .map_err(|e| e.to_string())?;
        let mut offset = 0u64;
        let mut buf = vec![0u8; CHUNK_SIZE.min(size.max(1) as usize)];
        while offset < size {
            let mut n = 0;
            while n < buf.len() {
                let r = file.read(&mut buf[n..]).await.map_err(|e| e.to_string())?;
                if r == 0 {
                    break;
                }
                n += r;
            }
            if n == 0 {
                break;
            }
            let end = offset + n as u64 - 1;
            let resp = client
                .patch(&url)
                .bearer_auth(&self.spec.runtime_token)
                .header("Content-Type", "application/octet-stream")
                .header("Content-Range", format!("bytes {offset}-{end}/*"))
                .body(buf[..n].to_vec())
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!(
                    "Cache service responded with {} during upload chunk.",
                    resp.status().as_u16()
                ));
            }
            offset = end + 1;
        }
        let resp = client
            .post(&url)
            .bearer_auth(&self.spec.runtime_token)
            .json(&serde_json::json!({"size": size}))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!(
                "Cache service responded with {} during commit cache.",
                resp.status().as_u16()
            ));
        }
        Ok(true)
    }
}

/// Download `url` to `dest`; returns the size.
async fn download(client: &reqwest::Client, url: &str, dest: &PathBuf) -> Result<u64, String> {
    let mut resp = client.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "Cache service responded with {} while downloading the archive",
            resp.status().as_u16()
        ));
    }
    let mut f = tokio::fs::File::create(dest)
        .await
        .map_err(|e| e.to_string())?;
    let mut total = 0u64;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        f.write_all(&chunk).await.map_err(|e| e.to_string())?;
        total += chunk.len() as u64;
    }
    f.flush().await.map_err(|e| e.to_string())?;
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_actions_cache() {
        // @actions/cache getCacheVersion(['node_modules'], 'gzip') on Linux.
        let expected = hex::encode(Sha256::digest(b"node_modules|gzip|1.0"));
        assert_eq!(cache_version(&["node_modules".to_string()]), expected);
    }

    #[test]
    fn input_lines() {
        let v = "a\n  b \n\n".to_string();
        assert_eq!(lines(Some(&v)), vec!["a", "b"]);
        assert!(lines(None).is_empty());
    }
}

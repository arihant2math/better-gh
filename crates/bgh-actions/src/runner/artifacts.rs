//! Native `actions/upload-artifact` and `actions/download-artifact`.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use super::context::list_files;
use super::job::{Body, JobRunner, StepRun};
use crate::workflow::glob_match;

fn has_wildcard(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Files selected by upload-artifact's `path` input, and the root that
/// archive entries are relative to.
pub(super) fn collect_upload(
    patterns: &[String],
    include_hidden: bool,
) -> std::io::Result<(Vec<PathBuf>, PathBuf)> {
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    let mut search_paths: Vec<PathBuf> = Vec::new();
    let mut excludes = Vec::new();
    let mut includes = Vec::new();
    for p in patterns {
        match p.strip_prefix('!') {
            Some(e) => excludes.push(e.trim_end_matches('/').to_string()),
            None => includes.push(p.trim_end_matches('/').to_string()),
        }
    }
    for inc in &includes {
        if has_wildcard(inc) {
            let first = inc.find(['*', '?', '[']).unwrap_or(inc.len());
            let base = match inc[..first].rfind('/') {
                Some(0) => "/".to_string(),
                Some(i) => inc[..i].to_string(),
                None => ".".to_string(),
            };
            let base_path = PathBuf::from(&base);
            for rel in list_files(&base_path)? {
                let full = if base == "/" {
                    format!("/{rel}")
                } else {
                    format!("{base}/{rel}")
                };
                if glob_match(inc, &full) {
                    files.insert(PathBuf::from(full));
                }
            }
            search_paths.push(base_path);
        } else {
            let p = PathBuf::from(inc);
            if p.is_file() {
                files.insert(p.clone());
            } else if p.is_dir() {
                for rel in list_files(&p)? {
                    files.insert(p.join(rel));
                }
            }
            search_paths.push(p);
        }
    }
    let files: Vec<PathBuf> = files
        .into_iter()
        .filter(|f| {
            let s = f.to_string_lossy();
            !excludes.iter().any(|e| {
                glob_match(e, &s)
                    || s.strip_prefix(e.as_str())
                        .is_some_and(|r| r.starts_with('/'))
            })
        })
        .collect();
    // Root directory.
    let root = if includes.len() == 1 && !has_wildcard(&includes[0]) {
        let p = PathBuf::from(&includes[0]);
        if p.is_file() {
            p.parent().map(Path::to_path_buf).unwrap_or(p)
        } else {
            p
        }
    } else {
        let mut lca: Option<Vec<std::ffi::OsString>> = None;
        for sp in &search_paths {
            let comps: Vec<_> = sp.components().map(|c| c.as_os_str().to_owned()).collect();
            lca = Some(match lca {
                None => comps,
                Some(prev) => prev
                    .into_iter()
                    .zip(comps)
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a)
                    .collect(),
            });
        }
        let root: PathBuf = lca.unwrap_or_default().into_iter().collect();
        if root.is_file() {
            root.parent().map(Path::to_path_buf).unwrap_or(root)
        } else {
            root
        }
    };
    let files = files
        .into_iter()
        .filter(|f| {
            include_hidden
                || !f
                    .strip_prefix(&root)
                    .unwrap_or(f)
                    .components()
                    .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        })
        .collect();
    Ok((files, root))
}

pub(super) fn write_zip(files: &[PathBuf], root: &Path, dest: &Path) -> std::io::Result<u64> {
    let f = std::fs::File::create(dest)?;
    let mut zip = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let mut total = 0u64;
    for file in files {
        let rel = file.strip_prefix(root).unwrap_or(file);
        let name = rel.to_string_lossy().replace('\\', "/");
        zip.start_file(name, opts).map_err(std::io::Error::other)?;
        let mut src = std::fs::File::open(file)?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n])?;
            total += n as u64;
        }
    }
    zip.finish().map_err(std::io::Error::other)?;
    Ok(total)
}

pub(super) fn unzip(src: &Path, dest: &Path) -> std::io::Result<usize> {
    let f = std::fs::File::open(src)?;
    let mut ar = zip::ZipArchive::new(f).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(dest)?;
    let mut count = 0;
    for i in 0..ar.len() {
        let mut entry = ar.by_index(i).map_err(std::io::Error::other)?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut w = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut w)?;
        count += 1;
    }
    Ok(count)
}

impl JobRunner {
    pub(super) async fn upload_artifact(
        &mut self,
        run: &StepRun,
        with: &IndexMap<String, String>,
    ) -> Body {
        match self.upload_inner(run, with).await {
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

    async fn upload_inner(
        &mut self,
        run: &StepRun,
        with: &IndexMap<String, String>,
    ) -> Result<IndexMap<String, String>, String> {
        let get = |k: &str| {
            with.get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let name = get("name").unwrap_or_else(|| "artifact".to_string());
        let path = get("path").ok_or("Input required and not supplied: path")?;
        let if_no_files = get("if-no-files-found").unwrap_or_else(|| "warn".into());
        let retention = match get("retention-days") {
            Some(r) => Some(
                r.parse::<i64>()
                    .map_err(|_| format!("Invalid retention-days '{r}'"))?,
            )
            .filter(|d| *d > 0),
            None => None,
        };
        let include_hidden = get("include-hidden-files").is_some_and(|v| v == "true");
        let ws = self.paths.guest_workspace();
        let patterns: Vec<String> = path
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| {
                let (neg, body) = match l.strip_prefix('!') {
                    Some(b) => ("!", b.trim()),
                    None => ("", l),
                };
                let guest = if has_wildcard(body) && !body.starts_with('/') {
                    format!("{ws}/{}", body.trim_start_matches("./"))
                } else {
                    self.paths.resolve(&ws, body)
                };
                format!("{neg}{}", self.paths.to_host(&guest).to_string_lossy())
            })
            .collect();
        let (files, root) = {
            let patterns = patterns.clone();
            tokio::task::spawn_blocking(move || collect_upload(&patterns, include_hidden))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("Failed to search for files: {e}"))?
        };
        if files.is_empty() {
            let msg = format!(
                "No files were found with the provided path: {path}. No artifacts will be uploaded."
            );
            return match if_no_files.as_str() {
                "error" => Err(msg),
                "ignore" => {
                    self.log(run.log_step, &msg);
                    Ok(IndexMap::new())
                }
                _ => {
                    self.log(run.log_step, &format!("##[warning]{msg}"));
                    self.annotations.push(crate::protocol::Annotation {
                        level: "warning".into(),
                        message: msg,
                        ..Default::default()
                    });
                    Ok(IndexMap::new())
                }
            };
        }
        self.log(
            run.log_step,
            &format!(
                "With the provided path, there will be {} file(s) uploaded",
                files.len()
            ),
        );
        self.log(
            run.log_step,
            &format!(
                "Root directory of the artifact: {}",
                self.paths.to_guest(&root)
            ),
        );
        let zip_path = self
            .paths
            .host_temp()
            .join(format!("artifact-{}.zip", uuid::Uuid::new_v4()));
        let size = {
            let zp = zip_path.clone();
            tokio::task::spawn_blocking(move || write_zip(&files, &root, &zp))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| format!("Failed to create the artifact archive: {e}"))?
        };
        self.log(
            run.log_step,
            &format!("Uploading artifact '{name}' ({size} bytes before compression)"),
        );
        let info = self
            .backend
            .upload_artifact(self.spec.job_id, &name, &zip_path, retention)
            .await;
        let _ = std::fs::remove_file(&zip_path);
        let info = info.map_err(|e| format!("Failed to upload artifact '{name}': {e:#}"))?;
        self.log(
            run.log_step,
            &format!(
                "Artifact {} has been successfully uploaded! Final size is {} bytes. Artifact ID is {}",
                info.name, info.size_in_bytes, info.id
            ),
        );
        let url = format!(
            "{}/{}/actions/runs/{}/artifacts/{}",
            self.spec.server_url.trim_end_matches('/'),
            self.spec.repository,
            self.spec.run_id,
            info.id
        );
        let mut out = IndexMap::new();
        out.insert("artifact-id".to_string(), info.id.to_string());
        out.insert("artifact-url".to_string(), url);
        Ok(out)
    }

    pub(super) async fn download_artifact(
        &mut self,
        run: &StepRun,
        with: &IndexMap<String, String>,
    ) -> Body {
        match self.download_inner(run, with).await {
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

    async fn download_inner(
        &mut self,
        run: &StepRun,
        with: &IndexMap<String, String>,
    ) -> Result<IndexMap<String, String>, String> {
        let get = |k: &str| {
            with.get(k)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let name = get("name");
        let pattern = get("pattern");
        let merge = get("merge-multiple").is_some_and(|v| v == "true");
        let ws = self.paths.guest_workspace();
        let dest_guest = self
            .paths
            .resolve(&ws, get("path").as_deref().unwrap_or(""));
        let dest = self.paths.to_host(&dest_guest);
        let all = self
            .backend
            .list_artifacts(self.spec.job_id)
            .await
            .map_err(|e| format!("Failed to list artifacts: {e:#}"))?;
        let selected: Vec<_> = match &name {
            Some(n) => {
                let a = all
                    .into_iter()
                    .find(|a| &a.name == n)
                    .ok_or_else(|| format!("Artifact not found for name: {n}"))?;
                vec![a]
            }
            None => all
                .into_iter()
                .filter(|a| pattern.as_deref().is_none_or(|p| glob_match(p, &a.name)))
                .collect(),
        };
        if name.is_none() {
            self.log(
                run.log_step,
                &format!("Found {} artifact(s) to download", selected.len()),
            );
        }
        for a in selected {
            let target = if name.is_some() || merge {
                dest.clone()
            } else {
                dest.join(&a.name)
            };
            let tmp = self
                .paths
                .host_temp()
                .join(format!("download-{}.zip", uuid::Uuid::new_v4()));
            self.log(
                run.log_step,
                &format!("Downloading artifact '{}' (ID {})", a.name, a.id),
            );
            self.backend
                .download_artifact(self.spec.job_id, a.id, &tmp)
                .await
                .map_err(|e| format!("Failed to download artifact '{}': {e:#}", a.name))?;
            let t = target.clone();
            let tz = tmp.clone();
            let n = tokio::task::spawn_blocking(move || unzip(&tz, &t))
                .await
                .map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&tmp);
            let n = n.map_err(|e| format!("Failed to extract artifact '{}': {e}", a.name))?;
            self.log(
                run.log_step,
                &format!("Extracted {n} file(s) to {}", self.paths.to_guest(&target)),
            );
        }
        let mut out = IndexMap::new();
        out.insert("download-path".to_string(), dest_guest);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_roots() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("dist/sub")).unwrap();
        std::fs::create_dir_all(d.join("other")).unwrap();
        std::fs::write(d.join("dist/a.txt"), "a").unwrap();
        std::fs::write(d.join("dist/sub/b.js"), "b").unwrap();
        std::fs::write(d.join("dist/.hidden"), "h").unwrap();
        std::fs::write(d.join("other/c.txt"), "c").unwrap();
        let s = |p: &str| d.join(p).to_string_lossy().into_owned();

        // Single file → its directory.
        let (files, root) = collect_upload(&[s("dist/a.txt")], false).unwrap();
        assert_eq!(files, vec![d.join("dist/a.txt")]);
        assert_eq!(root, d.join("dist"));
        // Directory → the directory; hidden files skipped.
        let (files, root) = collect_upload(&[s("dist")], false).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(root, d.join("dist"));
        // Glob → up to the first wildcard.
        let (files, root) = collect_upload(&[s("dist/**/*.js")], false).unwrap();
        assert_eq!(files, vec![d.join("dist/sub/b.js")]);
        assert_eq!(root, d.join("dist"));
        // Multiple → common ancestor, with exclusion.
        let (files, root) = collect_upload(
            &[s("dist"), s("other/c.txt"), format!("!{}", s("dist/sub"))],
            false,
        )
        .unwrap();
        assert_eq!(files, vec![d.join("dist/a.txt"), d.join("other/c.txt")]);
        assert_eq!(root, d.to_path_buf());

        let zip = d.join("x.zip");
        let (files, root) = collect_upload(&[s("dist")], true).unwrap();
        write_zip(&files, &root, &zip).unwrap();
        let out = d.join("out");
        assert_eq!(unzip(&zip, &out).unwrap(), 3);
        assert_eq!(std::fs::read_to_string(out.join("sub/b.js")).unwrap(), "b");
    }
}

//! Job logs: one file per step under `{data_dir}/actions/logs/{job_id}/`,
//! every line prefixed with a GitHub-style timestamp
//! (`2024-01-01T00:00:00.0000000Z text`). Appends are also published on
//! the Redis channel `actions:logs:{job_id}` for live streaming (see
//! [`crate::web::stream_job_logs`]); job completion publishes `{"done":true}`.

use std::path::PathBuf;

use bgh_core::AppState;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// Message published for each appended chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    #[serde(default)]
    pub step: i64,
    /// Byte offset of `text` in the step's log file (for de-duplication).
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub text: String,
    /// Set once when the job completed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub done: bool,
}

pub fn logs_root(state: &AppState) -> PathBuf {
    state.config.data_dir.join("actions").join("logs")
}

pub fn job_dir(state: &AppState, job_id: i64) -> PathBuf {
    logs_root(state).join(job_id.to_string())
}

pub fn channel(state: &AppState, job_id: i64) -> String {
    state.redis_key(&format!("actions:logs:{job_id}"))
}

/// GitHub log timestamp: 7 fractional digits.
pub fn timestamp() -> String {
    let now = Utc::now();
    format!(
        "{}.{:07}Z",
        now.format("%Y-%m-%dT%H:%M:%S"),
        now.timestamp_subsec_nanos() / 100
    )
}

/// Prefix every line of `text` with a timestamp. A trailing partial line
/// is terminated.
pub fn stamp(text: &str) -> String {
    let ts = timestamp();
    let mut out = String::with_capacity(text.len() + 32);
    for line in text.lines() {
        out.push_str(&ts);
        out.push(' ');
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Append raw text to a step log and publish it.
pub async fn append(state: &AppState, job_id: i64, step: i64, text: &str) -> anyhow::Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let dir = job_dir(state, job_id);
    tokio::fs::create_dir_all(&dir).await?;
    let path = step_path(state, job_id, step);
    let stamped = stamp(text);
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    let offset = f.metadata().await?.len();
    f.write_all(stamped.as_bytes()).await?;
    f.flush().await?;
    publish(
        state,
        job_id,
        &LogEvent {
            step,
            offset,
            text: stamped,
            done: false,
        },
    )
    .await;
    Ok(())
}

pub async fn publish(state: &AppState, job_id: i64, ev: &LogEvent) {
    let msg = serde_json::to_string(ev).expect("serializable");
    let mut conn = state.redis.clone();
    let res: Result<i64, _> = redis::cmd("PUBLISH")
        .arg(channel(state, job_id))
        .arg(msg)
        .query_async(&mut conn)
        .await;
    if let Err(err) = res {
        tracing::warn!(?err, job_id, "publishing log chunk");
    }
}

/// Step numbers with a log file, ascending.
pub async fn steps(state: &AppState, job_id: i64) -> Vec<i64> {
    let mut out = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(job_dir(state, job_id)).await {
        while let Ok(Some(e)) = rd.next_entry().await {
            if let Some(n) = e
                .file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".log"))
                .and_then(|n| n.parse::<i64>().ok())
            {
                out.push(n);
            }
        }
    }
    out.sort_unstable();
    out
}

pub fn step_path(state: &AppState, job_id: i64, step: i64) -> PathBuf {
    job_dir(state, job_id).join(format!("{step}.log"))
}

/// A whole step log in memory. Small logs and tests only: serve downloads
/// with [`job_files`] and replays with [`read_chunk`].
pub async fn read_step(state: &AppState, job_id: i64, step: i64) -> String {
    tokio::fs::read_to_string(step_path(state, job_id, step))
        .await
        .unwrap_or_default()
}

/// The whole job log (steps concatenated in order), in memory; see
/// [`read_step`].
pub async fn read_job(state: &AppState, job_id: i64) -> String {
    let mut out = String::new();
    for step in steps(state, job_id).await {
        out.push_str(&read_step(state, job_id, step).await);
    }
    out
}

/// The step files making up the job log, in order, with their current
/// lengths (a snapshot: later appends are not part of it).
pub async fn job_files(state: &AppState, job_id: i64) -> Vec<(PathBuf, u64)> {
    let mut out = Vec::new();
    for step in steps(state, job_id).await {
        let path = step_path(state, job_id, step);
        if let Ok(meta) = tokio::fs::metadata(&path).await {
            out.push((path, meta.len()));
        }
    }
    out
}

/// Up to `max` bytes of a step log from byte `offset`, cut after the last
/// complete line (or UTF-8 character) so chunks concatenate exactly.
/// Returns the text and the offset just past it; `None` at the end.
pub async fn read_chunk(
    state: &AppState,
    job_id: i64,
    step: i64,
    offset: u64,
    max: usize,
) -> std::io::Result<Option<(String, u64)>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut f = match tokio::fs::File::open(step_path(state, job_id, step)).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    f.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = Vec::with_capacity(max.min(64 << 10));
    (&mut f).take(max as u64).read_to_end(&mut buf).await?;
    if buf.is_empty() {
        return Ok(None);
    }
    let full = buf.len() == max;
    let mut end = match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) if full => i + 1,
        _ => buf.len(),
    };
    if let Err(e) = std::str::from_utf8(&buf[..end]) {
        // Only a character cut at the chunk edge is held back.
        end = if e.error_len().is_none() && e.valid_up_to() > 0 {
            e.valid_up_to()
        } else {
            end
        };
    }
    buf.truncate(end);
    let next = offset + end as u64;
    let text = String::from_utf8(buf)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
    Ok(Some((text, next)))
}

pub async fn delete_job(state: &AppState, job_id: i64) {
    let _ = tokio::fs::remove_dir_all(job_dir(state, job_id)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_lines() {
        let s = stamp("a\nb");
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].ends_with("Z a"));
        assert_eq!(lines[0].split(' ').next().unwrap().len(), 28);
        assert!(stamp("").is_empty());
    }
}

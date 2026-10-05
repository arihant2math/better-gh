//! Batched, masked step logs. Lines are queued synchronously and shipped
//! by a single background task (preserving order) at least every 500 ms,
//! whenever 32 KiB are pending, and on [`Logger::flush`].

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::mask::Masker;
use crate::protocol::Backend;

const FLUSH_INTERVAL: Duration = Duration::from_millis(500);
const FLUSH_BYTES: usize = 32 * 1024;

enum Msg {
    Text(i64, String),
    Flush(oneshot::Sender<()>),
}

pub struct Logger {
    tx: mpsc::UnboundedSender<Msg>,
    masker: Arc<Masker>,
    task: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Logger {
    pub fn new(backend: Arc<dyn Backend>, job_id: i64, masker: Arc<Masker>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(backend, job_id, rx));
        Logger {
            tx,
            masker,
            task: std::sync::Mutex::new(Some(task)),
        }
    }

    pub fn masker(&self) -> &Arc<Masker> {
        &self.masker
    }

    /// Log one (or several) lines to `step`, masked.
    pub fn log(&self, step: i64, text: &str) {
        let mut masked = self.masker.mask(text);
        masked.push('\n');
        let _ = self.tx.send(Msg::Text(step, masked));
    }

    /// Wait until everything logged so far has been sent.
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Msg::Flush(tx)).is_ok() {
            let _ = rx.await;
        }
    }

    /// Flush and stop the background task.
    pub async fn close(&self) {
        self.flush().await;
        let task = self.task.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(task) = task {
            task.abort();
        }
    }
}

async fn send(backend: &Arc<dyn Backend>, job_id: i64, pending: &mut Vec<(i64, String)>) {
    for (step, text) in pending.drain(..) {
        if let Err(e) = backend.append_log(job_id, step, &text).await {
            tracing::warn!(job_id, step, "failed to upload log chunk: {e:#}");
        }
    }
}

async fn run(backend: Arc<dyn Backend>, job_id: i64, mut rx: mpsc::UnboundedReceiver<Msg>) {
    let mut pending: Vec<(i64, String)> = Vec::new();
    let mut size = 0usize;
    let mut tick = tokio::time::interval(FLUSH_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Some(Msg::Text(step, text)) => {
                    size += text.len();
                    match pending.last_mut() {
                        Some((s, buf)) if *s == step => buf.push_str(&text),
                        _ => pending.push((step, text)),
                    }
                    if size >= FLUSH_BYTES {
                        send(&backend, job_id, &mut pending).await;
                        size = 0;
                    }
                }
                Some(Msg::Flush(ack)) => {
                    send(&backend, job_id, &mut pending).await;
                    size = 0;
                    let _ = ack.send(());
                }
                None => {
                    send(&backend, job_id, &mut pending).await;
                    return;
                }
            },
            _ = tick.tick() => {
                if !pending.is_empty() {
                    send(&backend, job_id, &mut pending).await;
                    size = 0;
                }
            }
        }
    }
}

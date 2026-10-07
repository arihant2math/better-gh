//! Live job-log fan-out: one Redis pub/sub connection per process (per
//! Redis prefix), shared by every log viewer.
//!
//! [`LogHub::subscribe`] `SUBSCRIBE`s the job's channel on first use and
//! hands out a [`LogSub`] fed by a per-job `broadcast` channel; the last
//! subscriber of a job `UNSUBSCRIBE`s it, and the hub (and its connection)
//! goes away with its last subscriber. All (un)subscribes are serialized
//! through the sink lock and re-check the subscriber table under it, so a
//! late unsubscribe never drops a channel that was subscribed again.
//!
//! A subscriber that falls behind (`Lagged`), and every subscriber after a
//! Redis reconnect ([`LiveMsg::Resync`]), may have missed chunks and should
//! re-read the log files from its offsets.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use bgh_core::AppState;
use futures::StreamExt;
use redis::aio::{PubSubSink, PubSubStream};
use tokio::sync::{broadcast, oneshot};

use crate::logs::{self, LogEvent};

/// Per-job queue length before a slow viewer lags (and re-reads files).
const QUEUE: usize = 256;

/// What a subscriber receives.
#[derive(Debug, Clone)]
pub enum LiveMsg {
    Event(Arc<LogEvent>),
    /// Messages may have been lost (Redis reconnected).
    Resync,
}

struct Channel {
    tx: broadcast::Sender<LiveMsg>,
    subscribers: usize,
}

pub struct LogHub {
    state: AppState,
    sink: tokio::sync::Mutex<PubSubSink>,
    channels: Arc<Mutex<HashMap<String, Channel>>>,
    /// Dropped with the hub: stops the reader task (and the connection).
    _stop: oneshot::Sender<()>,
}

/// A live subscription to one job's log channel.
pub struct LogSub {
    pub rx: broadcast::Receiver<LiveMsg>,
    channel: String,
    hub: Arc<LogHub>,
}

impl Drop for LogSub {
    fn drop(&mut self) {
        let last = {
            let mut channels = lock(&self.hub.channels);
            match channels.get_mut(&self.channel) {
                Some(c) if c.subscribers > 1 => {
                    c.subscribers -= 1;
                    false
                }
                Some(_) => {
                    channels.remove(&self.channel);
                    true
                }
                None => false,
            }
        };
        if last && let Ok(rt) = tokio::runtime::Handle::try_current() {
            let hub = self.hub.clone();
            let channel = self.channel.clone();
            rt.spawn(async move { hub.unsubscribe(&channel).await });
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type Registry = tokio::sync::Mutex<HashMap<String, Weak<LogHub>>>;

fn registry() -> &'static Registry {
    static HUBS: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    HUBS.get_or_init(Default::default)
}

async fn connect(state: &AppState) -> redis::RedisResult<(PubSubSink, PubSubStream)> {
    let client = redis::Client::open(state.config.redis_url.as_str())?;
    Ok(client.get_async_pubsub().await?.split())
}

impl LogHub {
    /// The process's hub for this state's Redis prefix, connected on demand.
    pub async fn get(state: &AppState) -> anyhow::Result<Arc<LogHub>> {
        let key = state.config.redis_prefix.clone();
        let mut hubs = registry().lock().await;
        if let Some(hub) = hubs.get(&key).and_then(Weak::upgrade) {
            return Ok(hub);
        }
        let (sink, stream) = connect(state).await?;
        let (stop, stopped) = oneshot::channel();
        let hub = Arc::new(LogHub {
            state: state.clone(),
            sink: tokio::sync::Mutex::new(sink),
            channels: Default::default(),
            _stop: stop,
        });
        hubs.insert(key, Arc::downgrade(&hub));
        tokio::spawn(run(Arc::downgrade(&hub), stream, stopped));
        Ok(hub)
    }

    /// Subscribe to the live log of `job_id` (the log-owning job). Once this
    /// returns, every later publish on the channel is delivered.
    pub async fn subscribe(self: &Arc<Self>, job_id: i64) -> anyhow::Result<LogSub> {
        let channel = logs::channel(&self.state, job_id);
        let mut sink = self.sink.lock().await;
        let (rx, new) = {
            let mut channels = lock(&self.channels);
            match channels.get_mut(&channel) {
                Some(c) => {
                    c.subscribers += 1;
                    (c.tx.subscribe(), false)
                }
                None => {
                    let (tx, rx) = broadcast::channel(QUEUE);
                    channels.insert(channel.clone(), Channel { tx, subscribers: 1 });
                    (rx, true)
                }
            }
        };
        let sub = LogSub {
            rx,
            channel: channel.clone(),
            hub: self.clone(),
        };
        if new {
            // On failure `sub` drops and undoes the registration.
            sink.subscribe(&channel).await?;
        }
        Ok(sub)
    }

    async fn unsubscribe(&self, channel: &str) {
        let mut sink = self.sink.lock().await;
        if lock(&self.channels).contains_key(channel) {
            return; // subscribed again meanwhile
        }
        if let Err(err) = sink.unsubscribe(channel).await {
            tracing::debug!(?err, channel, "log hub: unsubscribe failed");
        }
    }

    /// Number of jobs with live subscribers (for tests and metrics).
    pub fn channels(&self) -> usize {
        lock(&self.channels).len()
    }

    fn dispatch(&self, msg: redis::Msg) {
        let channel = msg.get_channel_name();
        let Ok(payload) = msg.get_payload::<String>() else {
            return;
        };
        let Ok(ev) = serde_json::from_str::<LogEvent>(&payload) else {
            return;
        };
        if let Some(c) = lock(&self.channels).get(channel) {
            let _ = c.tx.send(LiveMsg::Event(Arc::new(ev)));
        }
    }

    /// Swap in a fresh connection and re-subscribe every live channel.
    async fn reconnect(&self) -> anyhow::Result<PubSubStream> {
        let (mut sink, stream) = connect(&self.state).await?;
        let mut current = self.sink.lock().await;
        let channels: Vec<String> = lock(&self.channels).keys().cloned().collect();
        for channel in &channels {
            sink.subscribe(channel).await?;
        }
        *current = sink;
        for c in lock(&self.channels).values() {
            let _ = c.tx.send(LiveMsg::Resync);
        }
        Ok(stream)
    }
}

async fn run(weak: Weak<LogHub>, mut stream: PubSubStream, mut stopped: oneshot::Receiver<()>) {
    loop {
        let msg = tokio::select! {
            msg = stream.next() => msg,
            _ = &mut stopped => return,
        };
        let Some(hub) = weak.upgrade() else { return };
        match msg {
            Some(msg) => hub.dispatch(msg),
            None => {
                tracing::warn!("log hub: redis pubsub disconnected; reconnecting");
                drop(hub);
                stream = loop {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(500)) => {}
                        _ = &mut stopped => return,
                    }
                    let Some(hub) = weak.upgrade() else { return };
                    match hub.reconnect().await {
                        Ok(s) => break s,
                        Err(err) => tracing::warn!(?err, "log hub: reconnect failed"),
                    }
                };
            }
        }
    }
}

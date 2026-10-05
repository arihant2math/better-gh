//! Shared application state handed to every handler, job and listener.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::config::Config;
use crate::events::EventBus;
use crate::urls::Urls;

/// Cheap-to-clone application state (all fields are reference counted).
#[derive(Clone)]
pub struct AppState {
    /// Postgres pool.
    pub db: PgPool,
    /// Multiplexed, auto-reconnecting Redis connection. Clone it per use.
    pub redis: ConnectionManager,
    pub config: Arc<Config>,
    /// URL builder for GitHub-style `url` / `html_url` / `*_url` fields.
    pub urls: Arc<Urls>,
    /// In-process domain event bus (see [`crate::events`]).
    pub events: EventBus,
    ext: Arc<http::Extensions>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("base_url", &self.config.base_url)
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// Assemble state from already-open connections.
    pub fn new(config: Config, db: PgPool, redis: ConnectionManager) -> Self {
        let config = Arc::new(config);
        Self {
            db,
            redis,
            urls: Arc::new(Urls::new(&config)),
            config,
            events: EventBus::new(),
            ext: Arc::new(http::Extensions::new()),
        }
    }

    /// Open the Postgres pool and Redis connection described by `config`.
    pub async fn connect(config: Config) -> anyhow::Result<Self> {
        let db = connect_db(&config.database_url, config.db_max_connections).await?;
        let redis = connect_redis(&config.redis_url).await?;
        Ok(Self::new(config, db, redis))
    }

    /// Prefix a Redis key / channel with the configured namespace.
    pub fn redis_key(&self, key: &str) -> String {
        format!("{}{}", self.config.redis_prefix, key)
    }

    /// Attach a typed extension (e.g. a crate-specific service handle) at
    /// startup. Extensions are immutable once the state is shared.
    pub fn with_extension<T: Clone + Send + Sync + 'static>(mut self, value: T) -> Self {
        Arc::make_mut(&mut self.ext).insert(value);
        self
    }

    /// Fetch a typed extension previously attached with [`Self::with_extension`].
    pub fn extension<T: Clone + Send + Sync + 'static>(&self) -> Option<&T> {
        self.ext.get::<T>()
    }
}

/// Create a Postgres pool.
pub async fn connect_db(url: &str, max_connections: u32) -> anyhow::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
        .with_context(|| format!("connecting to postgres at {}", redact(url)))
}

/// Create a Redis connection manager.
pub async fn connect_redis(url: &str) -> anyhow::Result<ConnectionManager> {
    let client = redis::Client::open(url).context("invalid REDIS_URL")?;
    ConnectionManager::new(client)
        .await
        .with_context(|| format!("connecting to redis at {}", redact(url)))
}

fn redact(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(s), Some(at)) if at > s => format!("{}://***{}", &url[..s], &url[at..]),
        _ => url.to_string(),
    }
}

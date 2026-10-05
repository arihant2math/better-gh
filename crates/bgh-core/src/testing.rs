//! Integration test harness (feature `testing`).
//!
//! ```ignore
//! #[tokio::test]
//! async fn creates_repo() {
//!     let app = bgh_server::test_app().await;          // full app, fresh DB
//!     let alice = app.create_user("alice").await;
//!     let res = app.post("/api/v3/user/repos").auth(&alice)
//!         .json(&json!({"name": "hello"})).send().await;
//!     res.assert_status(201);
//!     assert_eq!(res.json()["full_name"], "alice/hello");
//! }
//! ```
//!
//! Each [`TestApp`] gets its own Postgres database, cloned from the migrated
//! template [`template_db`] (fast: `CREATE DATABASE ... TEMPLATE`), its
//! own Redis key prefix, its own data directory, and is served on a real
//! `127.0.0.1` port (for git CLI tests) as well as in-process via
//! [`TestApp::request`]. The database is dropped when the `TestApp` is
//! dropped (set `BGH_TEST_KEEP_DB=1` to keep it for debugging).

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use base64::Engine;
use http_body_util::BodyExt;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{ConnectOptions, Connection, Executor, PgConnection};
use tokio::sync::OnceCell;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::config::Config;
use crate::models::db;
use crate::registry::{AppFactory, Registry, start_listeners};
use crate::state::{AppState, connect_redis};

/// Name of the template database holding the migrated schema:
/// `bgh_test_tpl_<hash of the embedded migrations>`. Keying it by the
/// migration set lets worktrees/branches with different migrations run
/// tests concurrently on one Postgres without rebuilding each other's
/// template.
pub fn template_db() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for m in crate::db::MIGRATOR.iter() {
            h.update(m.version.to_le_bytes());
            h.update(&*m.checksum);
        }
        format!("bgh_test_tpl_{}", &hex::encode(h.finalize())[..16])
    })
}
/// Password of users created by [`TestApp::create_user`].
pub const TEST_PASSWORD: &str = "correct-horse-battery";
/// Advisory lock key serializing template migration / database creation.
const LOCK_KEY: i64 = 0x6267_685f_7465_7374; // "bgh_test"

static TEMPLATE_READY: OnceCell<()> = OnceCell::const_new();
static DB_COUNTER: AtomicU64 = AtomicU64::new(0);
static PASSWORD_HASH: OnceLock<String> = OnceLock::new();

fn base_database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost/bgh".into())
}

fn options_for(db: &str) -> PgConnectOptions {
    PgConnectOptions::from_str(&base_database_url())
        .expect("valid DATABASE_URL")
        .database(db)
        .disable_statement_logging()
}

async fn admin_conn() -> PgConnection {
    PgConnection::connect_with(&options_for("postgres"))
        .await
        .expect("connect to postgres maintenance db (is ./scripts/dev-setup.sh running?)")
}

fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Migrate the template database once per process (under a cross-process
/// advisory lock) and clean up databases left by dead test processes.
async fn prepare_template() {
    TEMPLATE_READY
        .get_or_init(|| async {
            let mut admin = admin_conn().await;
            admin
                .execute(format!("SELECT pg_advisory_lock({LOCK_KEY})").as_str())
                .await
                .expect("advisory lock");

            // Drop databases of test processes that no longer exist.
            let stale: Vec<String> = sqlx::query_scalar(
                "SELECT datname FROM pg_database WHERE datname LIKE 'bgh\\_test\\_%' AND datname <> $1",
            )
            .bind(template_db())
            .fetch_all(&mut admin)
            .await
            .unwrap_or_default();
            for name in stale {
                let pid = name.split('_').nth(2).and_then(|p| p.parse::<u32>().ok());
                if pid.is_some_and(|p| !pid_alive(p)) {
                    let _ = admin
                        .execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
                        .await;
                }
            }

            let template = template_db();
            let mut attempt = 0;
            loop {
                attempt += 1;
                let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
                    .bind(template)
                    .fetch_one(&mut admin)
                    .await
                    .expect("query pg_database");
                if !exists {
                    admin
                        .execute(format!("CREATE DATABASE \"{template}\"").as_str())
                        .await
                        .expect("create template db");
                }
                let mut conn = PgConnection::connect_with(&options_for(template))
                    .await
                    .expect("connect template db");
                let result = crate::db::MIGRATOR.run(&mut conn).await;
                let _ = conn.close().await;
                match result {
                    Ok(()) => break,
                    Err(err) if attempt == 1 => {
                        // Edited/removed migrations: rebuild the template from scratch.
                        eprintln!("bgh testing: rebuilding {template}: {err}");
                        admin
                            .execute(format!("DROP DATABASE IF EXISTS \"{template}\" WITH (FORCE)").as_str())
                            .await
                            .expect("drop template db");
                    }
                    Err(err) => panic!("migrating {template}: {err}"),
                }
            }

            admin
                .execute(format!("SELECT pg_advisory_unlock({LOCK_KEY})").as_str())
                .await
                .expect("advisory unlock");
            let _ = admin.close().await;
        })
        .await;
}

async fn create_test_database() -> String {
    prepare_template().await;
    let name = format!(
        "bgh_test_{}_{}",
        std::process::id(),
        DB_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let mut admin = admin_conn().await;
    // CREATE DATABASE ... TEMPLATE fails if another session is copying the
    // same template, so serialize (cheap: a copy takes a few ms).
    admin
        .execute(format!("SELECT pg_advisory_lock({LOCK_KEY})").as_str())
        .await
        .expect("advisory lock");
    let created = admin
        .execute(format!("CREATE DATABASE \"{name}\" TEMPLATE \"{}\"", template_db()).as_str())
        .await;
    let _ = admin
        .execute(format!("SELECT pg_advisory_unlock({LOCK_KEY})").as_str())
        .await;
    let _ = admin.close().await;
    created.unwrap_or_else(|e| panic!("create test database {name}: {e}"));
    name
}

fn drop_database_blocking(name: String) {
    // Runs on a separate thread with its own runtime: Drop can't be async.
    let _ = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            if let Ok(mut admin) = PgConnection::connect_with(&options_for("postgres")).await {
                let _ = admin
                    .execute(format!("DROP DATABASE IF EXISTS \"{name}\" WITH (FORCE)").as_str())
                    .await;
                let _ = admin.close().await;
            }
        });
    })
    .join();
}

/// A user created by the harness, with a full-scope personal access token.
#[derive(Debug, Clone)]
pub struct TestUser {
    pub id: i64,
    pub login: String,
    pub password: String,
    /// PAT with all scopes (`repo`, `admin:org`, `user`, `delete_repo`, ...).
    pub token: String,
}

/// An organization created by the harness.
#[derive(Debug, Clone)]
pub struct TestOrg {
    pub id: i64,
    pub login: String,
}

/// A running application backed by a fresh database.
pub struct TestApp {
    pub state: AppState,
    pub router: Router,
    pub registry: Arc<Registry>,
    /// TCP address the app is served on.
    pub addr: SocketAddr,
    /// `http://127.0.0.1:{port}` (also `config.base_url`).
    pub base_url: String,
    pub db_name: String,
    shutdown: CancellationToken,
    /// Durable event consumers: their own token (child of `shutdown`) and
    /// task handles, so tests can stop and restart them.
    consumers: tokio::sync::Mutex<(CancellationToken, Vec<tokio::task::JoinHandle<()>>)>,
    _data_dir: tempfile::TempDir,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if std::env::var_os("BGH_TEST_KEEP_DB").is_none() {
            drop_database_blocking(self.db_name.clone());
        }
    }
}

/// Scopes of harness-created tokens.
pub const ALL_SCOPES: &[&str] = &[
    "repo",
    "admin:org",
    "user",
    "delete_repo",
    "workflow",
    "admin:repo_hook",
    "admin:org_hook",
    "admin:public_key",
    "admin:gpg_key",
    "notifications",
    "gist",
    "write:packages",
    "project",
];

impl TestApp {
    /// Spawn a full application built by `factory` against a fresh database.
    pub async fn spawn_with(factory: AppFactory) -> Self {
        Self::spawn_with_config(factory, |_| {}).await
    }

    /// Like [`Self::spawn_with`], letting the test adjust the config.
    pub async fn spawn_with_config(factory: AppFactory, tweak: impl FnOnce(&mut Config)) -> Self {
        let db_name = create_test_database().await;
        let data_dir = tempfile::tempdir().expect("tempdir");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("local addr");
        let base_url = format!("http://{addr}");

        let mut config = Config::from_env().unwrap_or_default();
        config.base_url = base_url.clone();
        config.listen_addr = addr;
        config.database_url = options_for(&db_name).to_url_lossy().to_string();
        config.data_dir = data_dir.path().to_path_buf();
        config.redis_prefix = format!("test:{db_name}:");
        config.job_workers = 0;
        config.signup_enabled = true;
        config.db_max_connections = 5;
        // In-process requests have no client IP, so every anonymous request
        // of a test shares one bucket; keep it well above GitHub's 60/h
        // (matters only for tests that enable enforcement).
        config.rate_limits.unauthenticated_per_hour = 5000;
        // Deterministic: no environment-provided SSO provider.
        config.oidc = None;
        // Tests stand in for a reverse proxy: `X-Forwarded-For` names the
        // client (audit IPs, per-IP rate-limit buckets).
        config.trust_proxy = true;
        tweak(&mut config);

        let pool = PgPoolOptions::new()
            .max_connections(config.db_max_connections)
            .connect_with(options_for(&db_name))
            .await
            .expect("connect test database");
        let redis = connect_redis(&config.redis_url)
            .await
            .expect("connect redis (is ./scripts/dev-setup.sh running?)");
        let state = AppState::new(config, pool, redis);

        let mut registry = Registry::new();
        (factory.register)(&mut registry);
        let registry = Arc::new(registry);
        let router = (factory.router)(state.clone());

        let shutdown = CancellationToken::new();
        let consumer_token = shutdown.child_token();
        let handles = start_listeners(&state, &registry.listeners, consumer_token.clone())
            .await
            .expect("start event listeners");
        {
            let router = router.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let _ = axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown.cancelled_owned())
                    .await;
            });
        }

        Self {
            state,
            router,
            registry,
            addr,
            base_url,
            db_name,
            shutdown,
            consumers: tokio::sync::Mutex::new((consumer_token, handles)),
            _data_dir: data_dir,
        }
    }

    /// Stop the registry's durable event consumers (after they drain what
    /// is committed). Events emitted afterwards stay in the outbox until
    /// [`Self::start_listeners`].
    pub async fn stop_listeners(&self) {
        let mut guard = self.consumers.lock().await;
        guard.0.cancel();
        for h in guard.1.drain(..) {
            let _ = h.await;
        }
    }

    /// (Re)start the registry's durable event consumers (no-op if running).
    pub async fn start_listeners(&self) {
        let mut guard = self.consumers.lock().await;
        if !guard.0.is_cancelled() {
            return;
        }
        let token = self.shutdown.child_token();
        guard.1 = start_listeners(&self.state, &self.registry.listeners, token.clone())
            .await
            .expect("start event listeners");
        guard.0 = token;
    }

    /// Wait until every registered listener has processed all events
    /// emitted so far (panics after 30 s).
    pub async fn settle_events(&self) {
        let names: Vec<&str> = self.registry.listeners.iter().map(|l| l.name).collect();
        assert!(
            crate::outbox::wait_caught_up(&self.state, &names, std::time::Duration::from_secs(30))
                .await,
            "event listeners did not catch up"
        );
    }

    /// Absolute URL for a path on the TCP listener.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Authenticated git remote URL (`http://login:token@host/owner/repo.git`).
    pub fn git_remote(&self, user: &TestUser, owner: &str, repo: &str) -> String {
        format!(
            "http://{}:{}@{}/{owner}/{repo}.git",
            user.login, user.token, self.addr
        )
    }

    /// Run all ready background jobs to completion; returns how many ran.
    pub async fn drain_jobs(&self) -> usize {
        crate::jobs::drain(&self.state, &self.registry.jobs)
            .await
            .expect("draining jobs")
    }

    // ----- fixtures -------------------------------------------------------

    /// Create a user with [`TEST_PASSWORD`] and a full-scope token.
    pub async fn create_user(&self, login: &str) -> TestUser {
        self.create_user_with(login, false).await
    }

    /// Create a site administrator (token also has the `site_admin` scope).
    pub async fn create_admin(&self, login: &str) -> TestUser {
        self.create_user_with(login, true).await
    }

    async fn create_user_with(&self, login: &str, site_admin: bool) -> TestUser {
        let hash = PASSWORD_HASH
            .get_or_init(|| crate::crypto::hash_password(TEST_PASSWORD).expect("hash"))
            .clone();
        let mut conn = self.state.db.acquire().await.expect("acquire");
        let email = format!("{login}@example.com");
        let user = db::NewUser {
            login,
            email: Some(&email),
            name: None,
            password_hash: Some(&hash),
            site_admin,
        }
        .insert(&mut conn)
        .await
        .expect("insert user");
        drop(conn);
        let mut scopes: Vec<&str> = ALL_SCOPES.to_vec();
        if site_admin {
            scopes.push("site_admin");
        }
        let token = self.create_token_for(user.id, &scopes).await;
        TestUser {
            id: user.id,
            login: user.login,
            password: TEST_PASSWORD.into(),
            token,
        }
    }

    /// Create an additional token for `user` with the given scopes.
    pub async fn create_token(&self, user: &TestUser, scopes: &[&str]) -> String {
        self.create_token_for(user.id, scopes).await
    }

    async fn create_token_for(&self, user_id: i64, scopes: &[&str]) -> String {
        let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
        crate::auth::create_access_token(&self.state.db, user_id, "test", &scopes, None)
            .await
            .expect("create token")
            .1
    }

    /// Create a browser session; returns a `Cookie` header value.
    pub async fn session_cookie(&self, user: &TestUser) -> String {
        let token = crate::auth::create_session(&self.state, user.id, Some("test"), None)
            .await
            .expect("create session");
        format!("{}={token}", crate::auth::SESSION_COOKIE)
    }

    /// Create an organization with `admin` as its admin member.
    pub async fn create_org(&self, login: &str, admin: &TestUser) -> TestOrg {
        let mut conn = self.state.db.acquire().await.expect("acquire");
        let org = db::insert_org(&mut conn, login, None, admin.id)
            .await
            .expect("insert org");
        TestOrg {
            id: org.id,
            login: org.login,
        }
    }

    /// Add `user` to `org` with `role` (`member` | `admin`).
    pub async fn add_org_member(&self, org: &TestOrg, user: &TestUser, role: &str) {
        sqlx::query(
            "INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, $3)
             ON CONFLICT (org_id, user_id) DO UPDATE SET role = EXCLUDED.role",
        )
        .bind(org.id)
        .bind(user.id)
        .bind(role)
        .execute(&self.state.db)
        .await
        .expect("add org member");
    }

    /// Create a repository through the API (`POST /api/v3/user/repos` or
    /// `/orgs/{org}/repos`), panicking unless it returns 201.
    pub async fn create_repo_with(&self, user: &TestUser, org: Option<&str>, body: Value) -> Value {
        let path = match org {
            Some(org) => format!("/api/v3/orgs/{org}/repos"),
            None => "/api/v3/user/repos".to_string(),
        };
        let res = self.post(&path).auth(user).json(&body).send().await;
        res.assert_status(201);
        res.json()
    }

    /// Create a public repository owned by `user`.
    pub async fn create_repo(&self, user: &TestUser, name: &str) -> Value {
        self.create_repo_with(user, None, json!({ "name": name }))
            .await
    }

    /// Create a private repository owned by `user`.
    pub async fn create_private_repo(&self, user: &TestUser, name: &str) -> Value {
        self.create_repo_with(user, None, json!({ "name": name, "private": true }))
            .await
    }

    // ----- requests -------------------------------------------------------

    pub fn request(&self, method: Method, path: &str) -> TestRequest<'_> {
        TestRequest {
            app: self,
            method,
            path: path.to_string(),
            headers: HeaderMap::new(),
            body: Vec::new(),
        }
    }

    pub fn get(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::GET, path)
    }

    pub fn post(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::POST, path)
    }

    pub fn patch(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::PATCH, path)
    }

    pub fn put(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::PUT, path)
    }

    pub fn delete(&self, path: &str) -> TestRequest<'_> {
        self.request(Method::DELETE, path)
    }
}

/// Builder for an in-process request (`router.oneshot`).
pub struct TestRequest<'a> {
    app: &'a TestApp,
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl TestRequest<'_> {
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.append(
            HeaderName::from_bytes(name.as_bytes()).expect("header name"),
            HeaderValue::from_str(value).expect("header value"),
        );
        self
    }

    /// `Authorization: token <t>`
    pub fn token(self, token: &str) -> Self {
        self.header("authorization", &format!("token {token}"))
    }

    /// Authenticate as `user` with their full-scope token.
    pub fn auth(self, user: &TestUser) -> Self {
        let token = user.token.clone();
        self.token(&token)
    }

    /// `Authorization: Basic base64(user:secret)`
    pub fn basic(self, user: &str, secret: &str) -> Self {
        let enc = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{secret}"));
        self.header("authorization", &format!("Basic {enc}"))
    }

    /// `Cookie: ...` (see [`TestApp::session_cookie`]).
    /// `Cookie` header. For a `bgh_session` cookie the matching
    /// `X-CSRF-Token` is added too, like the web client does; send a raw
    /// `.header("cookie", ..)` to test CSRF rejection.
    pub fn cookie(self, cookie: &str) -> Self {
        let csrf = cookie
            .split(';')
            .filter_map(|p| p.trim().split_once('='))
            .find(|(k, _)| *k == crate::auth::SESSION_COOKIE)
            .map(|(_, v)| crate::auth::csrf_token(v));
        let req = self.header("cookie", cookie);
        match csrf {
            Some(t) => req.header(crate::auth::CSRF_HEADER, &t),
            None => req,
        }
    }

    pub fn json(mut self, body: &impl Serialize) -> Self {
        self.body = serde_json::to_vec(body).expect("serialize body");
        self.header("content-type", "application/json")
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    pub async fn send(self) -> TestResponse {
        let mut req = Request::builder()
            .method(self.method)
            .uri(&self.path)
            .body(Body::from(self.body))
            .expect("request");
        *req.headers_mut() = self.headers;
        req.headers_mut()
            .entry(header::HOST)
            .or_insert_with(|| HeaderValue::from_str(&self.app.addr.to_string()).expect("host"));
        let resp = self
            .app
            .router
            .clone()
            .oneshot(req)
            .await
            .expect("infallible router");
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = resp
            .into_body()
            .collect()
            .await
            .expect("read body")
            .to_bytes();
        TestResponse {
            status,
            headers,
            body,
        }
    }
}

/// A buffered response.
#[derive(Debug, Clone)]
pub struct TestResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl TestResponse {
    pub fn status(&self) -> u16 {
        self.status.as_u16()
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Parse the body as JSON (panics with the body on failure).
    #[track_caller]
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("invalid JSON ({e}): {}", self.text()))
    }

    #[track_caller]
    pub fn json_as<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("unexpected JSON ({e}): {}", self.text()))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// Assert the status code, printing the body on mismatch.
    #[track_caller]
    pub fn assert_status(&self, expected: u16) -> &Self {
        assert_eq!(
            self.status.as_u16(),
            expected,
            "unexpected status; body: {}",
            self.text()
        );
        self
    }
}

//! `bgh backup`, `bgh backup verify` and `bgh restore`.
//!
//! A backup directory holds one snapshot per run:
//!
//! ```text
//! DIR/20261007T120000Z/manifest.json   version, migration level, every entry + sha256
//! DIR/20261007T120000Z/db.dump         pg_dump --format=custom (one consistent snapshot)
//! DIR/20261007T120000Z/data/...        BGH_DATA_DIR without caches
//! ```
//!
//! The database is dumped first, then the data directory is copied (git
//! maintenance keeps unreachable objects for the prune grace period, so
//! everything the dump references is still on disk). Files whose size and
//! mtime match the previous snapshot are hard links to it (rsync
//! `--link-dest` style): every snapshot is complete on its own, but only
//! changed files take space. A snapshot is written as `NAME.partial` and
//! renamed when complete.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail, ensure};
use bgh_core::Config;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Manifest format version.
pub const FORMAT: u32 = 1;
pub const MANIFEST: &str = "manifest.json";
pub const DB_DUMP: &str = "db.dump";
pub const DATA: &str = "data";

/// Regenerable or transient paths under the data directory (relative,
/// `/`-separated) that backups skip.
pub const EXCLUDED: &[&str] = &["cache", "actions/caches", "actions/tmp", "files/tmp"];

/// External programs used by backup and restore.
#[derive(Clone, Debug)]
pub struct Tools {
    pub pg_dump: String,
    pub pg_restore: String,
    pub git: String,
}

impl Tools {
    pub fn from_config(config: &Config) -> Self {
        Self {
            pg_dump: std::env::var("BGH_PG_DUMP").unwrap_or_else(|_| "pg_dump".into()),
            pg_restore: std::env::var("BGH_PG_RESTORE").unwrap_or_else(|_| "pg_restore".into()),
            git: config.git_bin.clone(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Relative to the data directory, `/`-separated.
    pub path: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub size: u64,
    /// Modification time, nanoseconds since the epoch.
    #[serde(default, skip_serializing_if = "is_zero_i")]
    pub mtime_ns: i128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// Symlink target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}
fn is_zero_i(n: &i128) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub files: u64,
    pub bytes: u64,
    /// Files hard-linked to the previous snapshot.
    pub linked_files: u64,
    pub linked_bytes: u64,
    /// Files that disappeared while being copied (e.g. repacked).
    pub vanished: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub bgh_version: String,
    /// Highest applied migration in the dumped database.
    pub migration_version: i64,
    /// Absolute data directory the snapshot was taken from (alternates
    /// files of forks name it; restore rewrites them).
    pub source_data_dir: String,
    /// Whether the server key came from `BGH_ACTIONS_SECRET_KEY` (then it is
    /// not in the snapshot and must be kept with the configuration).
    pub server_key_from_env: bool,
    pub previous: Option<String>,
    pub database: Entry,
    pub excluded: Vec<String>,
    pub stats: Stats,
    pub entries: Vec<Entry>,
}

impl Manifest {
    pub fn read(snapshot: &Path) -> anyhow::Result<Self> {
        let path = snapshot.join(MANIFEST);
        let text = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let m: Self =
            serde_json::from_slice(&text).with_context(|| format!("parsing {}", path.display()))?;
        ensure!(
            m.format == FORMAT,
            "{}: unsupported manifest format {}",
            path.display(),
            m.format
        );
        Ok(m)
    }
}

/// Highest migration this binary knows.
pub fn known_migration_version() -> i64 {
    bgh_core::db::MIGRATOR
        .iter()
        .map(|m| m.version)
        .max()
        .unwrap_or(0)
}

async fn applied_migration_version(db: &sqlx::PgPool) -> anyhow::Result<i64> {
    let v: Option<i64> =
        sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations WHERE success")
            .fetch_one(db)
            .await
            .context("reading _sqlx_migrations")?;
    Ok(v.unwrap_or(0))
}

/// Completed snapshots in `dir`, oldest first.
pub fn snapshots(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_none_or(|x| x != "partial") && p.join(MANIFEST).is_file())
        .collect();
    out.sort();
    out
}

/// `path` itself when it is a snapshot, else the newest snapshot in it.
pub fn resolve_snapshot(path: &Path) -> anyhow::Result<PathBuf> {
    if path.join(MANIFEST).is_file() {
        return Ok(path.to_path_buf());
    }
    snapshots(path)
        .pop()
        .with_context(|| format!("no backup snapshot in {}", path.display()))
}

/// `DATABASE_URL` for libpq tools: sqlx-only query parameters (such as
/// `statement-cache-capacity`) are dropped and sqlx's spellings mapped.
pub fn libpq_url(database_url: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(database_url).context("parsing DATABASE_URL")?;
    let params: Vec<(String, String)> = url
        .query_pairs()
        .filter_map(|(k, v)| {
            let k = match k.as_ref() {
                "ssl-mode" => "sslmode",
                "ssl-root-cert" => "sslrootcert",
                "ssl-client-cert" => "sslcert",
                "ssl-client-key" => "sslkey",
                "application-name" => "application_name",
                k @ ("host" | "hostaddr" | "port" | "dbname" | "user" | "password" | "sslmode"
                | "sslrootcert" | "sslcert" | "sslkey" | "options" | "application_name"
                | "connect_timeout") => k,
                _ => return None,
            };
            Some((k.to_string(), v.into_owned()))
        })
        .collect();
    url.set_query(None);
    if !params.is_empty() {
        url.query_pairs_mut().extend_pairs(params);
    }
    Ok(url.into())
}

fn run(cmd: &mut Command, what: &str) -> anyhow::Result<()> {
    let out = cmd
        .output()
        .with_context(|| format!("running {what} (is it installed?)"))?;
    ensure!(
        out.status.success(),
        "{what} failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// `bgh backup --to DIR`: returns the new snapshot's path and manifest.
pub async fn backup(
    config: &Config,
    db: &sqlx::PgPool,
    tools: &Tools,
    to: &Path,
) -> anyhow::Result<(PathBuf, Manifest)> {
    // Snapshots hold credentials (the dump, actions/server.key, signing and
    // SSH host keys): owner-only from the start.
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    set_mode(to, Some(0o700))?;
    ensure!(
        !is_inside(to, &config.data_dir),
        "the backup directory {} must not be inside the data directory",
        to.display()
    );
    let previous = snapshots(to).pop();
    let prev_manifest = match &previous {
        Some(p) => Some(Manifest::read(p)?),
        None => None,
    };
    // Names sort chronologically; a second backup within the same second
    // waits for the next one.
    let (created_at, name) = loop {
        let now = chrono::Utc::now();
        let name = now.format("%Y%m%dT%H%M%SZ").to_string();
        if !to.join(&name).exists() {
            break (now, name);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let partial = to.join(format!("{name}.partial"));
    if partial.exists() {
        std::fs::remove_dir_all(&partial)?;
    }
    std::fs::create_dir_all(&partial)?;
    set_mode(&partial, Some(0o700))?;
    std::fs::create_dir_all(partial.join(DATA))?;

    // 1. Database: one consistent MVCC snapshot.
    let migration_version = applied_migration_version(db).await?;
    let dump = partial.join(DB_DUMP);
    create_private(&dump)?;
    {
        let (pg_dump, url, dump) = (
            tools.pg_dump.clone(),
            libpq_url(&config.database_url)?,
            dump.clone(),
        );
        tokio::task::spawn_blocking(move || {
            run(
                Command::new(&pg_dump)
                    .arg("--format=custom")
                    .arg("--no-owner")
                    .arg("--file")
                    .arg(&dump)
                    .arg("--dbname")
                    .arg(&url),
                "pg_dump",
            )
        })
        .await??;
    }
    let database = file_entry(&dump, DB_DUMP)?;

    // 2. Data directory, hard-linking unchanged files.
    let data_dir = config.data_dir.clone();
    let dest = partial.join(DATA);
    let prev_data = previous.as_ref().map(|p| p.join(DATA));
    let prev_index: HashMap<String, Entry> = prev_manifest
        .map(|m| {
            m.entries
                .into_iter()
                .filter(|e| e.kind == Kind::File)
                .map(|e| (e.path.clone(), e))
                .collect()
        })
        .unwrap_or_default();
    let (entries, stats) = tokio::task::spawn_blocking(move || {
        let mut copier = Copier {
            dest,
            prev_data,
            prev_index,
            entries: Vec::new(),
            stats: Stats::default(),
        };
        if data_dir.is_dir() {
            copier.walk(&data_dir, "")?;
        }
        anyhow::Ok((copier.entries, copier.stats))
    })
    .await??;

    let source_data_dir = std::path::absolute(&config.data_dir)
        .unwrap_or_else(|_| config.data_dir.clone())
        .to_string_lossy()
        .into_owned();
    let manifest = Manifest {
        format: FORMAT,
        created_at,
        bgh_version: env!("CARGO_PKG_VERSION").to_string(),
        migration_version,
        source_data_dir,
        server_key_from_env: config.actions.secret_key.is_some(),
        previous: previous
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned()),
        database,
        excluded: EXCLUDED.iter().map(|s| s.to_string()).collect(),
        stats,
        entries,
    };
    let mut f = create_private(&partial.join(MANIFEST))?;
    serde_json::to_writer(&mut f, &manifest)?;
    f.sync_all()?;
    let final_path = to.join(&name);
    std::fs::rename(&partial, &final_path)?;
    Ok((final_path, manifest))
}

struct Copier {
    dest: PathBuf,
    prev_data: Option<PathBuf>,
    prev_index: HashMap<String, Entry>,
    entries: Vec<Entry>,
    stats: Stats,
}

fn join_rel(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{base}/{name}")
    }
}

fn mtime_ns(meta: &std::fs::Metadata) -> i128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0)
}

#[cfg(unix)]
fn mode(meta: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o7777)
}
#[cfg(not(unix))]
fn mode(_: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    match mode {
        Some(m) => std::fs::set_permissions(path, std::fs::Permissions::from_mode(m)),
        None => Ok(()),
    }
}
#[cfg(not(unix))]
fn set_mode(_: &Path, _: Option<u32>) -> std::io::Result<()> {
    Ok(())
}

/// Create (or truncate) `path` with mode 0600.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::File::options();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let f = opts.open(path)?;
    set_mode(path, Some(0o600))?;
    Ok(f)
}

/// Copy order of a directory's children: inside a bare repository, `HEAD`,
/// `refs/` and `packed-refs` before everything else and `objects/` last, so
/// that a push (or `git pack-refs`) during the copy cannot leave refs
/// pointing at objects the snapshot lacks (objects are append-only within
/// the prune grace period). Elsewhere, name order.
fn copy_order(names: &mut [String], is_repo: bool) {
    let rank = |n: &str| match n {
        _ if !is_repo => 0,
        "HEAD" => 0,
        "refs" => 1,
        "packed-refs" => 2,
        "objects" => 4,
        _ => 3,
    };
    names.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
}

fn to_systime(ns: i128) -> SystemTime {
    UNIX_EPOCH + Duration::from_nanos(ns.max(0) as u64)
}

/// Copy `from` to `to` (a new file), returning its sha256.
fn copy_hashing(from: &Path, to: &Path) -> std::io::Result<String> {
    let mut src = std::fs::File::open(from)?;
    let mut dst = std::fs::File::create(to)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        dst.write_all(&buf[..n])?;
    }
    dst.flush()?;
    Ok(hex::encode(hasher.finalize()))
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn file_entry(path: &Path, rel: &str) -> anyhow::Result<Entry> {
    let meta = std::fs::metadata(path)?;
    Ok(Entry {
        path: rel.to_string(),
        kind: Kind::File,
        size: meta.len(),
        mtime_ns: mtime_ns(&meta),
        sha256: Some(hash_file(path)?),
        mode: mode(&meta),
        target: None,
    })
}

impl Copier {
    fn walk(&mut self, dir: &Path, rel: &str) -> anyhow::Result<()> {
        let children: Vec<_> = match std::fs::read_dir(dir) {
            Ok(rd) => rd.flatten().collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
        };
        let is_repo = dir.join("HEAD").is_file() && dir.join("objects").is_dir();
        let mut names: Vec<String> = children
            .iter()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        copy_order(&mut names, is_repo);
        for name in names {
            let rel = join_rel(rel, &name);
            if EXCLUDED.contains(&rel.as_str()) {
                continue;
            }
            let path = dir.join(&name);
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.stats.vanished += 1;
                    continue;
                }
                Err(e) => return Err(e).with_context(|| format!("stat {}", path.display())),
            };
            let out = self.dest.join(&rel);
            if meta.is_dir() {
                std::fs::create_dir_all(&out)?;
                self.entries.push(Entry {
                    path: rel.clone(),
                    kind: Kind::Dir,
                    size: 0,
                    mtime_ns: 0,
                    sha256: None,
                    mode: mode(&meta),
                    target: None,
                });
                self.walk(&path, &rel)?;
                set_mode(&out, mode(&meta))?;
            } else if meta.is_symlink() {
                let target = std::fs::read_link(&path)?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &out)?;
                self.entries.push(Entry {
                    path: rel,
                    kind: Kind::Symlink,
                    size: 0,
                    mtime_ns: 0,
                    sha256: None,
                    mode: None,
                    target: Some(target.to_string_lossy().into_owned()),
                });
            } else if meta.is_file() {
                self.copy_file(&path, &out, rel, &meta)?;
            }
            // Sockets, fifos, ...: skipped.
        }
        Ok(())
    }

    fn copy_file(
        &mut self,
        path: &Path,
        out: &Path,
        rel: String,
        meta: &std::fs::Metadata,
    ) -> anyhow::Result<()> {
        let (size, mtime) = (meta.len(), mtime_ns(meta));
        if let (Some(prev), Some(pe)) = (&self.prev_data, self.prev_index.get(&rel))
            && pe.size == size
            && pe.mtime_ns == mtime
            && std::fs::hard_link(prev.join(&rel), out).is_ok()
        {
            self.stats.files += 1;
            self.stats.bytes += size;
            self.stats.linked_files += 1;
            self.stats.linked_bytes += size;
            self.entries.push(pe.clone());
            return Ok(());
        }
        let sha = match copy_hashing(path, out) {
            Ok(sha) => sha,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let _ = std::fs::remove_file(out);
                self.stats.vanished += 1;
                return Ok(());
            }
            Err(e) => return Err(e).with_context(|| format!("copying {}", path.display())),
        };
        // Size and mtime as of the copy (the file may have grown since the stat).
        let copied = std::fs::metadata(out)?;
        let f = std::fs::File::options().write(true).open(out)?;
        f.set_modified(to_systime(mtime))?;
        drop(f);
        set_mode(out, mode(meta))?;
        self.stats.files += 1;
        self.stats.bytes += copied.len();
        self.entries.push(Entry {
            path: rel,
            kind: Kind::File,
            size: copied.len(),
            mtime_ns: mtime,
            sha256: Some(sha),
            mode: mode(meta),
            target: None,
        });
        Ok(())
    }
}

/// Result of [`verify`].
#[derive(Debug, Default)]
pub struct VerifyReport {
    pub files: u64,
    pub bytes: u64,
    pub problems: Vec<String>,
}

/// `bgh backup verify`: every manifest entry exists with its size and
/// checksum, and `pg_restore --list` can read the dump.
pub fn verify(snapshot: &Path, tools: &Tools) -> anyhow::Result<(Manifest, VerifyReport)> {
    let manifest = Manifest::read(snapshot)?;
    let mut report = VerifyReport::default();
    let check_file =
        |path: &Path, e: &Entry, report: &mut VerifyReport| match std::fs::metadata(path) {
            Ok(meta) if meta.len() != e.size => {
                report
                    .problems
                    .push(format!("{}: size {} != {}", e.path, meta.len(), e.size))
            }
            Ok(_) => match hash_file(path) {
                Ok(h) if Some(&h) == e.sha256.as_ref() => {
                    report.files += 1;
                    report.bytes += e.size;
                }
                Ok(_) => report
                    .problems
                    .push(format!("{}: checksum mismatch", e.path)),
                Err(err) => report.problems.push(format!("{}: {err}", e.path)),
            },
            Err(err) => report.problems.push(format!("{}: {err}", e.path)),
        };
    check_file(&snapshot.join(DB_DUMP), &manifest.database, &mut report);
    let data = snapshot.join(DATA);
    for e in &manifest.entries {
        let path = data.join(&e.path);
        match e.kind {
            Kind::File => check_file(&path, e, &mut report),
            Kind::Dir if !path.is_dir() => report
                .problems
                .push(format!("{}: missing directory", e.path)),
            Kind::Symlink if std::fs::read_link(&path).is_err() => {
                report.problems.push(format!("{}: missing symlink", e.path))
            }
            _ => {}
        }
    }
    if let Err(err) = run(
        Command::new(&tools.pg_restore)
            .arg("--list")
            .arg(snapshot.join(DB_DUMP)),
        "pg_restore --list",
    ) {
        report.problems.push(format!("{DB_DUMP}: {err:#}"));
    }
    Ok((manifest, report))
}

/// Options of [`restore`].
#[derive(Clone, Debug)]
pub struct RestoreOptions {
    /// Replace a non-empty database and data directory.
    pub force: bool,
    /// Repositories checked with `git fsck --connectivity-only` afterwards.
    pub fsck_sample: usize,
}

/// Result of [`restore`].
#[derive(Debug)]
pub struct RestoreReport {
    pub snapshot: PathBuf,
    pub manifest_version: i64,
    pub files: u64,
    pub fsck_checked: Vec<String>,
}

/// `bgh restore --from DIR` into the configured database and data
/// directory (with the server stopped), then apply newer migrations and
/// `git fsck --connectivity-only` a sample of repositories.
pub async fn restore(
    config: &Config,
    db: &sqlx::PgPool,
    tools: &Tools,
    from: &Path,
    opts: &RestoreOptions,
) -> anyhow::Result<RestoreReport> {
    let snapshot = resolve_snapshot(from)?;
    ensure!(
        !is_inside(&snapshot, &config.data_dir),
        "the snapshot {} must not be inside the data directory",
        snapshot.display()
    );
    let manifest = Manifest::read(&snapshot)?;
    let known = known_migration_version();
    ensure!(
        manifest.migration_version <= known,
        "backup {} was taken by bgh {} at migration {}, newer than this binary knows ({known}); \
         restore it with that version or later",
        snapshot.display(),
        manifest.bgh_version,
        manifest.migration_version,
    );

    // Nothing is replaced unless the whole snapshot checks out.
    let (_, report) = {
        let (snapshot, tools) = (snapshot.clone(), tools.clone());
        tokio::task::spawn_blocking(move || verify(&snapshot, &tools)).await??
    };
    ensure!(
        report.problems.is_empty(),
        "snapshot {} failed verification, nothing was changed: {}",
        snapshot.display(),
        report.problems.join("; ")
    );

    // Refuse to clobber live data unless asked to.
    let tables: i64 =
        sqlx::query_scalar("SELECT count(*) FROM pg_tables WHERE schemaname = 'public'")
            .fetch_one(db)
            .await?;
    let data_dir = config.data_dir.clone();
    let data_nonempty = std::fs::read_dir(&data_dir).is_ok_and(|mut d| d.next().is_some());
    if !opts.force {
        ensure!(
            tables == 0,
            "the database is not empty ({tables} tables); restore into an empty database or pass --force to replace it"
        );
        ensure!(
            !data_nonempty,
            "{} is not empty; restore into an empty data directory or pass --force to replace it",
            data_dir.display()
        );
    }

    // Database.
    if tables > 0 {
        sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
            .execute(db)
            .await
            .context("clearing the database")?;
    }
    {
        let (pg_restore, url, dump) = (
            tools.pg_restore.clone(),
            libpq_url(&config.database_url)?,
            snapshot.join(DB_DUMP),
        );
        tokio::task::spawn_blocking(move || {
            run(
                Command::new(&pg_restore)
                    .args([
                        "--no-owner",
                        "--no-privileges",
                        "--exit-on-error",
                        "--single-transaction",
                    ])
                    .arg("--dbname")
                    .arg(&url)
                    .arg(&dump),
                "pg_restore",
            )
        })
        .await??;
    }

    // Files.
    let src = snapshot.join(DATA);
    let entries = manifest.entries.clone();
    let old_root = manifest.source_data_dir.clone();
    let files = tokio::task::spawn_blocking(move || {
        if data_nonempty {
            for e in std::fs::read_dir(&data_dir)?.flatten() {
                let p = e.path();
                if e.file_type()?.is_dir() {
                    std::fs::remove_dir_all(&p)?;
                } else {
                    std::fs::remove_file(&p)?;
                }
            }
        }
        std::fs::create_dir_all(&data_dir)?;
        let new_root = std::path::absolute(&data_dir)?
            .to_string_lossy()
            .into_owned();
        let mut files = 0u64;
        let mut dirs = Vec::new();
        for e in &entries {
            let to = data_dir.join(&e.path);
            match e.kind {
                Kind::Dir => {
                    std::fs::create_dir_all(&to)?;
                    dirs.push((to, e.mode));
                }
                Kind::Symlink => {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(e.target.as_deref().unwrap_or_default(), &to)?;
                }
                Kind::File => {
                    let from = src.join(&e.path);
                    std::fs::copy(&from, &to).with_context(|| format!("restoring {}", e.path))?;
                    if e.path.ends_with("objects/info/alternates") && old_root != new_root {
                        let text = std::fs::read_to_string(&to)?;
                        std::fs::write(&to, text.replace(&old_root, &new_root))?;
                    }
                    let f = std::fs::File::options().write(true).open(&to)?;
                    f.set_modified(to_systime(e.mtime_ns))?;
                    drop(f);
                    set_mode(&to, e.mode)?;
                    files += 1;
                }
            }
        }
        // Directory modes last (a read-only directory would block its children).
        for (dir, mode) in dirs.into_iter().rev() {
            set_mode(&dir, mode)?;
        }
        anyhow::Ok(files)
    })
    .await??;

    bgh_core::db::migrate(db)
        .await
        .context("applying migrations after restore")?;

    // Sample fsck.
    let repos_root = config.repos_dir();
    let git = tools.git.clone();
    let sample = opts.fsck_sample;
    let fsck_checked = tokio::task::spawn_blocking(move || {
        let repos = git_dirs(&repos_root);
        let picked = sample_evenly(&repos, sample);
        let mut checked = Vec::new();
        for repo in picked {
            run(
                Command::new(&git).arg("--git-dir").arg(repo).args([
                    "fsck",
                    "--connectivity-only",
                    "--no-progress",
                ]),
                &format!("git fsck {}", repo.display()),
            )?;
            checked.push(repo.display().to_string());
        }
        anyhow::Ok(checked)
    })
    .await??;

    Ok(RestoreReport {
        snapshot,
        manifest_version: manifest.migration_version,
        files,
        fsck_checked,
    })
}

/// Whether `path` is `dir` or below it (after resolving symlinks).
fn is_inside(path: &Path, dir: &Path) -> bool {
    match (std::fs::canonicalize(path), std::fs::canonicalize(dir)) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => false,
    }
}

/// Bare repositories (`*.git` with a `HEAD`) under `root`, sorted.
pub fn git_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if p.extension().is_some_and(|x| x == "git") && p.join("HEAD").is_file() {
                out.push(p);
            } else {
                stack.push(p);
            }
        }
    }
    out.sort();
    out
}

fn sample_evenly<T>(items: &[T], n: usize) -> Vec<&T> {
    if n == 0 || items.is_empty() {
        return Vec::new();
    }
    if items.len() <= n {
        return items.iter().collect();
    }
    (0..n).map(|i| &items[i * items.len() / n]).collect()
}

/// Human-readable byte count.
pub fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Fail early with a clear message when an external tool is missing.
pub fn require_tool(bin: &str) -> anyhow::Result<()> {
    match Command::new(bin).arg("--version").output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => bail!(
            "{bin} --version failed: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => bail!(
            "{bin} not found ({e}); install the PostgreSQL client tools or set BGH_PG_DUMP / BGH_PG_RESTORE"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_is_even_and_bounded() {
        let v: Vec<u32> = (0..10).collect();
        assert_eq!(sample_evenly(&v, 3), vec![&0, &3, &6]);
        assert_eq!(sample_evenly(&v, 20).len(), 10);
        assert!(sample_evenly(&v, 0).is_empty());
    }

    #[test]
    fn libpq_url_drops_sqlx_parameters() {
        assert_eq!(
            libpq_url("postgres://u:p@h:5432/db?statement-cache-capacity=0&ssl-mode=require")
                .unwrap(),
            "postgres://u:p@h:5432/db?sslmode=require"
        );
        assert_eq!(libpq_url("postgres://h/db").unwrap(), "postgres://h/db");
    }

    #[test]
    fn repositories_copy_refs_before_objects() {
        let mut names: Vec<String> = ["objects", "config", "packed-refs", "refs", "HEAD", "hooks"]
            .map(String::from)
            .to_vec();
        copy_order(&mut names, true);
        assert_eq!(
            names,
            ["HEAD", "refs", "packed-refs", "config", "hooks", "objects"]
        );
        let mut names: Vec<String> = ["objects", "HEAD", "b"].map(String::from).to_vec();
        copy_order(&mut names, false);
        assert_eq!(names, ["HEAD", "b", "objects"]);
    }

    #[test]
    fn human_bytes_units() {
        assert_eq!(human_bytes(10), "10 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
    }
}

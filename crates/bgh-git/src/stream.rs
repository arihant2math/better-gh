//! Streaming object contents through `git cat-file` (for blobs too large
//! to load into memory).

use std::io;
use std::process::Stdio;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio_util::io::ReaderStream;

use crate::storage::RepoStore;
use crate::{GitError, GitResult, cmd, is_sha};

/// Stream the contents of blob `sha`.
pub async fn blob_stream(
    store: &RepoStore,
    repo_id: i64,
    sha: &str,
) -> GitResult<BoxStream<'static, io::Result<Bytes>>> {
    if !is_sha(sha) {
        return Err(GitError::InvalidInput(format!("invalid object id {sha:?}")));
    }
    let dir = store.git_dir(repo_id)?;
    let mut c = cmd::git(&store.git_bin, Some(&dir));
    c.args(["cat-file", "blob", sha])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = c.spawn()?;
    let stdout = child.stdout.take().expect("piped stdout");
    // Reap the child when the stream is done (kill_on_drop covers aborts).
    let reader = ReaderStream::with_capacity(stdout, 64 * 1024);
    let s = reader.chain(futures::stream::once(async move {
        let _ = child.wait().await;
        Ok(Bytes::new())
    }));
    Ok(
        s.filter(|r| futures::future::ready(!matches!(r, Ok(b) if b.is_empty())))
            .boxed(),
    )
}

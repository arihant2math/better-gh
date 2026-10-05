//! Storage, write plumbing and gix-backed reads against real repositories.

use bgh_git::write::{self, CommitRequest, FileChange, Identity};
use bgh_git::{GitError, PathLookup, RepoStore, TreeEntryKind};

fn store(dir: &tempfile::TempDir) -> RepoStore {
    RepoStore::new(dir.path().join("repos"), "git")
}

async fn commit(
    store: &RepoStore,
    id: i64,
    parent: Option<&str>,
    files: &[(&str, &str)],
    msg: &str,
) -> String {
    let changes: Vec<FileChange> = files
        .iter()
        .map(|(p, c)| FileChange::write(*p, *c))
        .collect();
    write::commit_changes(
        store,
        id,
        CommitRequest {
            branch: "main",
            parent,
            changes: &changes,
            message: msg,
            author: &Identity::new("Ada", "ada@example.com"),
            committer: None,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn init_commit_and_read() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp);
    let path = store.init(300, "main").await.unwrap();
    assert!(path.ends_with("2c/300.git"), "{path:?}");
    assert!(store.exists(300));

    let empty = store.read(300, |r| r.is_empty()).await.unwrap();
    assert!(empty);
    assert_eq!(
        store
            .read(300, |r| r.head_branch())
            .await
            .unwrap()
            .as_deref(),
        Some("main")
    );

    let c1 = commit(
        &store,
        300,
        None,
        &[("README.md", "# hello\n")],
        "Initial commit",
    )
    .await;
    let c2 = commit(
        &store,
        300,
        Some(&c1),
        &[("src/lib.rs", "fn main() {}\n")],
        "Add lib",
    )
    .await;

    // Stale parent is rejected by the old-value check.
    let stale = write::commit_changes(
        &store,
        300,
        CommitRequest {
            branch: "main",
            parent: Some(&c1),
            changes: &[FileChange::write("x", "y")],
            message: "racy",
            author: &Identity::new("Ada", "ada@example.com"),
            committer: None,
        },
    )
    .await;
    assert!(matches!(stale, Err(GitError::Command { .. })));

    let (branches, head, commit2, log, root, file, nested) = store
        .read(300, {
            let c2 = c2.clone();
            move |r| {
                Ok((
                    r.branches()?,
                    r.resolve_commit("HEAD")?,
                    r.commit(&c2)?,
                    r.log("main", None, 0, 10)?,
                    r.lookup_path("main", "")?,
                    r.lookup_path("main", "README.md")?,
                    r.log("main", Some("src"), 0, 10)?,
                ))
            }
        })
        .await
        .unwrap();
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].short_name(), "main");
    assert_eq!(branches[0].target, c2);
    assert_eq!(head, c2);
    assert_eq!(commit2.parents, vec![c1.clone()]);
    assert_eq!(commit2.author.name, "Ada");
    assert_eq!(commit2.summary(), "Add lib");
    assert_eq!(
        log.iter().map(|c| c.sha.clone()).collect::<Vec<_>>(),
        vec![c2.clone(), c1.clone()]
    );
    assert_eq!(nested.len(), 1);
    assert_eq!(nested[0].sha, c2);

    let PathLookup::Tree { entries, .. } = root else {
        panic!("root should be a tree")
    };
    let names: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(
        names,
        vec![
            ("README.md", TreeEntryKind::Blob),
            ("src", TreeEntryKind::Tree)
        ]
    );

    let PathLookup::Entry(readme) = file else {
        panic!("README should be a file")
    };
    let blob = store.read(300, move |r| r.blob(&readme.sha)).await.unwrap();
    assert_eq!(blob.data, b"# hello\n");
    assert!(!blob.is_binary());

    let missing = store.read(300, |r| r.lookup_path("main", "nope.txt")).await;
    assert!(matches!(missing, Err(GitError::NotFound(_))));
    let too_big = store
        .read(300, move |r| r.blob_with_limit(&blob.sha, 2))
        .await;
    assert!(matches!(too_big, Err(GitError::TooLarge { .. })));

    // Fork shares objects and copies refs.
    store.fork(300, 301).await.unwrap();
    let fork_head = store.read(301, |r| r.resolve_commit("main")).await.unwrap();
    assert_eq!(fork_head, c2);
    assert!(store.path(301).join("objects/info/alternates").exists());

    // Default branch switch.
    write::update_ref(&store, 300, "refs/heads/dev", &c1, None)
        .await
        .unwrap();
    write::set_head(&store, 300, "dev").await.unwrap();
    assert_eq!(
        store.read(300, |r| r.resolve_commit("HEAD")).await.unwrap(),
        c1
    );
    write::delete_ref(&store, 300, "refs/heads/dev", Some(&c1))
        .await
        .unwrap();
    assert!(
        store
            .read(300, |r| r.find_ref("refs/heads/dev"))
            .await
            .unwrap()
            .is_none()
    );

    store.delete(300).await.unwrap();
    assert!(!store.exists(300));
    store.delete(300).await.unwrap();
}

#[tokio::test]
async fn rejects_invalid_input() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store(&tmp);
    assert!(matches!(
        store.init(1, "bad..name").await,
        Err(GitError::InvalidInput(_))
    ));
    store.init(1, "main").await.unwrap();
    let r = write::commit_changes(
        &store,
        1,
        CommitRequest {
            branch: "main",
            parent: None,
            changes: &[FileChange::write("../escape", "x")],
            message: "m",
            author: &Identity::new("A", "a@example.com"),
            committer: None,
        },
    )
    .await;
    assert!(matches!(r, Err(GitError::InvalidInput(_))));
    assert!(matches!(
        store.read(2, |r| r.is_empty()).await,
        Err(GitError::NotFound(_))
    ));
}

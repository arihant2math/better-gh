//! GitHub JSON shapes for git data: commits (REST and git database), diff
//! entries, refs, tags, trees, and the short `{sha, url}` commit pointer.

use std::collections::HashMap;

use bgh_core::models::api::SimpleUser;
use bgh_core::node_id::{self, NodeType};
use bgh_core::prelude::*;
use bgh_core::urls::{Urls, encode_path};
use bgh_git::ops::DiffFile;
use bgh_git::{Commit, Signature, Tag};
use serde::{Deserialize, Serialize};

/// URL context for one repository.
#[derive(Clone, Copy)]
pub struct RepoRef<'a> {
    pub urls: &'a Urls,
    pub owner: &'a str,
    pub name: &'a str,
    pub id: i64,
}

impl<'a> RepoRef<'a> {
    pub fn new(urls: &'a Urls, access: &'a RepoAccess) -> Self {
        Self {
            urls,
            owner: &access.owner.login,
            name: &access.repo.name,
            id: access.repo.id,
        }
    }

    /// `{api}/repos/{o}/{r}{path}`
    pub fn api(&self, path: &str) -> String {
        format!("{}{path}", self.urls.repo(self.owner, self.name))
    }

    /// `{base}/{o}/{r}{path}`
    pub fn html(&self, path: &str) -> String {
        format!("{}{path}", self.urls.repo_html(self.owner, self.name))
    }

    fn node(&self, ty: NodeType, key: &str) -> String {
        node_id::encode_str(ty, &format!("{}:{key}", self.id))
    }
}

/// `{name, email, date}` of git commits and tags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitActor {
    pub name: String,
    pub email: String,
    pub date: Timestamp,
}

impl From<&Signature> for GitActor {
    fn from(s: &Signature) -> Self {
        Self {
            name: s.name.clone(),
            email: s.email.clone(),
            date: s.when.into(),
        }
    }
}

/// Signature verification status. Signatures are reported but not
/// verified (`reason: unknown_key` for signed objects).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verification {
    pub verified: bool,
    pub reason: String,
    pub signature: Option<String>,
    pub payload: Option<String>,
    pub verified_at: Option<Timestamp>,
}

impl Verification {
    pub fn for_signature(sig: Option<&str>) -> Self {
        Self {
            verified: false,
            reason: if sig.is_some() {
                "unknown_key"
            } else {
                "unsigned"
            }
            .into(),
            signature: sig.map(str::to_string),
            payload: None,
            verified_at: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShaUrl {
    pub sha: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParentRef {
    pub sha: String,
    pub url: String,
    pub html_url: String,
}

/// `git-commit` (git database API).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommit {
    pub sha: String,
    pub node_id: String,
    pub url: String,
    pub html_url: String,
    pub author: GitActor,
    pub committer: GitActor,
    pub tree: ShaUrl,
    pub message: String,
    pub parents: Vec<ParentRef>,
    pub verification: Verification,
}

pub fn git_commit(r: &RepoRef, c: &Commit) -> GitCommit {
    GitCommit {
        sha: c.sha.clone(),
        node_id: r.node(NodeType::Commit, &c.sha),
        url: r.api(&format!("/git/commits/{}", c.sha)),
        html_url: r.html(&format!("/commit/{}", c.sha)),
        author: (&c.author).into(),
        committer: (&c.committer).into(),
        tree: ShaUrl {
            sha: c.tree.clone(),
            url: r.api(&format!("/git/trees/{}", c.tree)),
        },
        message: c.message.clone(),
        parents: c
            .parents
            .iter()
            .map(|p| ParentRef {
                sha: p.clone(),
                url: r.api(&format!("/git/commits/{p}")),
                html_url: r.html(&format!("/commit/{p}")),
            })
            .collect(),
        verification: Verification::for_signature(c.signature.as_deref()),
    }
}

/// The `commit` object nested in REST commits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitInner {
    pub url: String,
    pub author: GitActor,
    pub committer: GitActor,
    pub message: String,
    pub tree: ShaUrl,
    pub comment_count: i64,
    pub verification: Verification,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub total: u64,
    pub additions: u64,
    pub deletions: u64,
}

/// `diff-entry`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffEntry {
    pub sha: String,
    pub filename: String,
    pub status: String,
    pub additions: u64,
    pub deletions: u64,
    pub changes: u64,
    pub blob_url: String,
    pub raw_url: String,
    pub contents_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_filename: Option<String>,
}

/// Render diff entries; URLs point at `at` (the head commit).
pub fn diff_entries(r: &RepoRef, at: &str, files: &[DiffFile]) -> (Vec<DiffEntry>, Stats) {
    let mut stats = Stats {
        total: 0,
        additions: 0,
        deletions: 0,
    };
    let entries = files
        .iter()
        .map(|f| {
            stats.additions += f.additions;
            stats.deletions += f.deletions;
            let path = encode_path(&f.path);
            DiffEntry {
                sha: f.sha().to_string(),
                filename: f.path.clone(),
                status: f.status.clone(),
                additions: f.additions,
                deletions: f.deletions,
                changes: f.additions + f.deletions,
                blob_url: r.html(&format!("/blob/{at}/{path}")),
                raw_url: r.html(&format!("/raw/{at}/{path}")),
                contents_url: r.api(&format!("/contents/{path}?ref={at}")),
                patch: f.patch.clone(),
                previous_filename: f.previous_path.clone(),
            }
        })
        .collect();
    stats.total = stats.additions + stats.deletions;
    (entries, stats)
}

/// `commit` (REST commits API). `stats`/`files` only on single commits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitJson {
    pub url: String,
    pub sha: String,
    pub node_id: String,
    pub html_url: String,
    pub comments_url: String,
    pub commit: CommitInner,
    pub author: Option<SimpleUser>,
    pub committer: Option<SimpleUser>,
    pub parents: Vec<ParentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<Stats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<DiffEntry>>,
}

/// Render a REST commit. `users` maps lowercased emails to accounts (see
/// [`crate::identity::users_by_email`]).
pub fn commit_json(r: &RepoRef, c: &Commit, users: &HashMap<String, db::User>) -> CommitJson {
    let user = |email: &str| {
        users
            .get(&email.to_ascii_lowercase())
            .map(|u| SimpleUser::new(r.urls, u))
    };
    CommitJson {
        url: r.api(&format!("/commits/{}", c.sha)),
        sha: c.sha.clone(),
        node_id: r.node(NodeType::Commit, &c.sha),
        html_url: r.html(&format!("/commit/{}", c.sha)),
        comments_url: r.api(&format!("/commits/{}/comments", c.sha)),
        commit: CommitInner {
            url: r.api(&format!("/git/commits/{}", c.sha)),
            author: (&c.author).into(),
            committer: (&c.committer).into(),
            message: c.message.trim_end_matches('\n').to_string(),
            tree: ShaUrl {
                sha: c.tree.clone(),
                url: r.api(&format!("/git/trees/{}", c.tree)),
            },
            comment_count: 0,
            verification: Verification::for_signature(c.signature.as_deref()),
        },
        author: user(&c.author.email),
        committer: user(&c.committer.email),
        parents: c
            .parents
            .iter()
            .map(|p| ParentRef {
                sha: p.clone(),
                url: r.api(&format!("/commits/{p}")),
                html_url: r.html(&format!("/commit/{p}")),
            })
            .collect(),
        stats: None,
        files: None,
    }
}

/// Emails of commits' authors and committers (for [`crate::identity::users_by_email`]).
pub fn commit_emails(commits: &[Commit]) -> impl Iterator<Item = &str> {
    commits
        .iter()
        .flat_map(|c| [c.author.email.as_str(), c.committer.email.as_str()])
}

/// `{sha, url}` pointer to a REST commit.
pub fn short_commit(r: &RepoRef, sha: &str) -> ShaUrl {
    ShaUrl {
        sha: sha.to_string(),
        url: r.api(&format!("/commits/{sha}")),
    }
}

/// `git-ref`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitRef {
    #[serde(rename = "ref")]
    pub refname: String,
    pub node_id: String,
    pub url: String,
    pub object: GitRefObject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitRefObject {
    #[serde(rename = "type")]
    pub kind: String,
    pub sha: String,
    pub url: String,
}

/// API URL of a git object by type (`commit` | `tag` | `tree` | `blob`).
pub fn object_url(r: &RepoRef, kind: &str, sha: &str) -> String {
    let coll = match kind {
        "tag" => "tags",
        "tree" => "trees",
        "blob" => "blobs",
        _ => "commits",
    };
    r.api(&format!("/git/{coll}/{sha}"))
}

pub fn git_ref(r: &RepoRef, refname: &str, kind: &str, sha: &str) -> GitRef {
    GitRef {
        refname: refname.to_string(),
        node_id: r.node(NodeType::Ref, refname),
        url: r.api(&format!("/git/{}", encode_path(refname))),
        object: GitRefObject {
            kind: kind.to_string(),
            sha: sha.to_string(),
            url: object_url(r, kind, sha),
        },
    }
}

/// `git-tag` (annotated tag object).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitTag {
    pub node_id: String,
    pub tag: String,
    pub sha: String,
    pub url: String,
    pub message: String,
    pub tagger: Option<GitActor>,
    pub object: GitRefObject,
    pub verification: Verification,
}

pub fn git_tag(r: &RepoRef, t: &Tag) -> GitTag {
    GitTag {
        node_id: r.node(NodeType::Ref, &format!("tag:{}", t.sha)),
        tag: t.name.clone(),
        sha: t.sha.clone(),
        url: r.api(&format!("/git/tags/{}", t.sha)),
        message: t.message.clone(),
        tagger: t.tagger.as_ref().map(GitActor::from),
        object: GitRefObject {
            kind: t.object_type.clone(),
            sha: t.object.clone(),
            url: object_url(r, &t.object_type, &t.object),
        },
        verification: Verification::for_signature(t.signature.as_deref()),
    }
}

/// `git-tree` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitTreeEntry {
    pub path: String,
    pub mode: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `git-tree`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitTree {
    pub sha: String,
    pub url: String,
    pub tree: Vec<GitTreeEntry>,
    pub truncated: bool,
}

pub fn tree_entry(
    r: &RepoRef,
    path: String,
    mode: &str,
    kind: bgh_git::TreeEntryKind,
    sha: &str,
    size: Option<u64>,
) -> GitTreeEntry {
    let ty = kind.object_type();
    GitTreeEntry {
        path,
        mode: mode.to_string(),
        kind: ty.to_string(),
        sha: sha.to_string(),
        size: if ty == "blob" { size } else { None },
        url: match ty {
            "commit" => None,
            other => Some(object_url(r, other, sha)),
        },
    }
}

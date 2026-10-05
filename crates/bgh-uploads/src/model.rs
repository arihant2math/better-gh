//! The `attachments` row and its JSON.

use bgh_core::prelude::*;
use bgh_core::urls::encode_segment;
use serde::Serialize;

use crate::policy::{self, Kind};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AttachmentRow {
    pub id: i64,
    pub uuid: uuid::Uuid,
    pub uploader_id: Option<i64>,
    pub owner_id: i64,
    pub repo_id: Option<i64>,
    pub name: String,
    pub content_type: String,
    pub size: i64,
    pub sha256: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl AttachmentRow {
    pub const COLUMNS: &'static str =
        "id, uuid, uploader_id, owner_id, repo_id, name, content_type, size, sha256, created_at";

    pub fn kind(&self) -> Kind {
        policy::classify(&self.name).map_or(Kind::File, |(k, _)| k)
    }

    /// Canonical URL: `/user-attachments/assets/{uuid}` for images and
    /// videos, `/user-attachments/files/{id}/{name}` for other files.
    pub fn href(&self, state: &AppState) -> String {
        if self.kind().is_media() {
            state
                .urls
                .html(&format!("/user-attachments/assets/{}", self.uuid))
        } else {
            state.urls.html(&format!(
                "/user-attachments/files/{}/{}",
                self.id,
                encode_segment(&self.name)
            ))
        }
    }

    /// What the editor inserts: `![name](url)` for images, the bare URL for
    /// videos (rendered as a player), `[name](url)` otherwise.
    pub fn markdown(&self, state: &AppState) -> String {
        let href = self.href(state);
        let alt = escape_label(&self.name);
        match self.kind() {
            Kind::Image | Kind::Svg => format!("![{alt}]({href})"),
            Kind::Video => href,
            Kind::File => format!("[{alt}]({href})"),
        }
    }
}

/// Markdown link text: brackets and backslashes escaped.
fn escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Response of `POST /_bgh/uploads`.
#[derive(Debug, Serialize)]
pub struct Attachment {
    pub id: i64,
    pub uuid: String,
    pub name: String,
    pub content_type: String,
    pub size: i64,
    pub href: String,
    pub markdown: String,
    pub repository_id: Option<i64>,
    pub created_at: Timestamp,
}

impl Attachment {
    pub fn new(state: &AppState, row: &AttachmentRow) -> Self {
        Self {
            id: row.id,
            uuid: row.uuid.to_string(),
            name: row.name.clone(),
            content_type: row.content_type.clone(),
            size: row.size,
            href: row.href(state),
            markdown: row.markdown(state),
            repository_id: row.repo_id,
            created_at: row.created_at.into(),
        }
    }
}

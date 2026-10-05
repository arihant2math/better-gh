# P6 attachments — status

**Done.** Branch `bgh/p06-attachments`, self-integrated (fast-forward) into `claude/sleepy-cray-9jj0t3` at `93ed214` with the full gate green. Image, video and file uploads in
issue/PR comments, review comments, issue bodies, release notes and the
wiki editor.

## Backend: new crate `bgh-uploads` (registered in bgh-server)

| Endpoint | Notes |
|----------|-------|
| `POST /_bgh/uploads?repository_id=&repository=&owner_id=&name=` | Auth required (401). Body: `multipart/form-data` (`file` part or first part with a filename) or the raw content with `?name=`. `201 {id, uuid, name, content_type, size, href, markdown, repository_id, created_at}`. `repository_id` or `repository=owner/name` ties the upload to a repo (caller needs read access, else 404; archived → 403); `owner_id` charges an org the caller belongs to (else 404); default owner is the uploader. |
| `GET /user-attachments/assets/{uuid}` | Canonical URL for images and videos. |
| `GET /user-attachments/files/{id}/{name}` | Canonical URL for other files; the name must match (no id enumeration). |

* **Limits / types** (`policy.rs`): GitHub's list — images png/gif/jpg/jpeg
  (10 MB), svg (10 MB), video mp4/mov/webm (100 MB), files 25 MB (log, txt,
  md, patch/diff, pdf, zip/gz/tgz, docx/pptx/xlsx, OpenDocument, rtf, json,
  csv/tsv, html, eml/msg, dmp, cpuprofile, common code files). Anything else,
  empty files, oversize files and raster images whose magic bytes don't
  match the extension → 422 `Validation Failed` with
  `errors[{resource: "Attachment", field: "file", code: "custom", message}]`.
  The stored content type comes from the extension, never the client.
* **Serving:** private repo's attachment → read access required, else 404
  (sessions work, so `<img>` tags render for signed-in users). Always
  `X-Content-Type-Options: nosniff`, `Content-Security-Policy: default-src
  'none'; sandbox`, ETag (sha256, `If-None-Match` → 304),
  `Accept-Ranges: bytes` with single-range support (206/416). Raster images
  and videos are `inline`; SVG, HTML and every other file are
  `attachment`. `Cache-Control: public, immutable` for public repos/no repo,
  `private` otherwise.
* **Markdown inserted:** `![name](href)` for images/SVG, the bare URL for
  videos, `[name](href)` for files.
* **Storage:** content-addressed `{data_dir}/files/attachments/{sha[..2]}/{sha}`
  (identical uploads share a blob), spooled through `files/attachments/tmp`.
* **Quota:** new `bgh_core::settings::check_upload_quota(state, owner_id,
  bytes)` (additive): repos (incl. LFS, as P2 counts them) + attachments against `storage_quotas.max_total_size_mb`
  → 403 when over.
* **Repo deletion:** rows cascade with the repository (and owner); the
  `RepositoryDeleted` listener enqueues job `uploads.gc`, which removes
  unreferenced blobs older than 1 h (grace for in-flight uploads).

## Tables / migrations

`migrations/1800_attachments.sql`: `attachments(id, uuid, uploader_id,
owner_id, repo_id NULL, name, content_type, size, sha256, created_at)` with
indexes on owner, repo, uploader and sha256.

## Shared-code changes

* `bgh-core/src/settings.rs`: `check_upload_quota` (new fn).
* Workspace `Cargo.toml` / `bgh-server`: `bgh-uploads` member, router and
  `register` wiring. `bgh-uploads` enables axum's `multipart` feature.
* Docs: ARCHITECTURE.md crate list, SELF_HOSTING.md data dir line.

## Web

* `api/uploads.ts`: `uploadAttachment(file, target, onProgress, signal)`
  (XHR with progress + CSRF; transport in mock mode).
* `components/editor/attachments.ts`: placeholder logic (`![Uploading
  name…]()` inserted at the caret, replaced with the server markdown, removed
  on failure, selection preserved) — `attachments.test.ts`.
* `components/editor/useAttachments.tsx`: paste / drop / paperclip "Attach
  files" button, drag highlight, `Uploading N files… x%` status, error toast.
  Used by `MarkdownEditor` (issue/PR comments, issue bodies, review comments,
  milestones), `WikiEditPage` and `ReleaseEditPage` (release notes; drops on
  the notes textarea attach to the notes instead of adding a release asset).
* `ui/markdown/video.ts`: a paragraph that is only a
  `/user-attachments/assets/{uuid}` URL renders as `<video controls>`
  (`video.test.ts`).
* Mock: `mock/extra/uploads.ts` (+ test); images get `data:` URLs so they
  render in `dev:mock`.

## Tests

* `cargo test -p bgh-uploads`: upload/download, raw-body upload and files
  path, private-repo 404 for outsiders (token, anonymous, session),
  disallowed/oversize/mismatched → 422, SVG/HTML `attachment` + CSP +
  nosniff, video Range 206/416, org owner + quota 403, repo deletion + GC;
  unit tests for policy and Range parsing.
* Vitest: placeholder replaced / removed on failure, video embed, mock.
* Manual (Playwright, real server): `web/scripts/attachments-e2e.mjs` —
  pasted screenshot renders in an issue comment after reload, failed upload
  removes its placeholder with a toast, anonymous fetch of the private
  attachment is 404, wiki and release-notes editors upload.

## Known gaps / TODOs

* The server markdown renderer (`body_html`) doesn't turn bare video URLs
  into `<video>` (client only); P35 owns renderer parity.
* No S3-compatible storage backend (disk only), no attachment listing/
  deletion UI; attachments not tied to a repository are public by URL (like
  GitHub's).
* PR creation form (`ComparePage`) still uses a plain textarea (P55 brings
  the full `MarkdownEditor`, which then gets attachments for free).

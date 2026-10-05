import { getBoot } from '../boot';
import { browserTransport, transport } from './transport';

/** `POST /_bgh/uploads` response (crates/bgh-uploads). */
export interface Attachment {
  id: number;
  uuid: string;
  name: string;
  content_type: string;
  size: number;
  /** Absolute URL of the attachment. */
  href: string;
  /** What to insert: `![name](href)`, a bare video URL, or `[name](href)`. */
  markdown: string;
  repository_id: number | null;
  created_at: string;
}

export interface UploadTarget {
  /** Repository the attachment belongs to (private repos gate access). */
  repositoryId?: number;
  /** `owner/name`, when the repository id isn't known locally. */
  repository?: string;
  /** Account charged for the upload (defaults to the uploader). */
  ownerId?: number;
}

function uploadPath(file: File, target: UploadTarget): string {
  const q = new URLSearchParams({ name: file.name });
  if (target.repositoryId != null && target.repositoryId > 0) q.set('repository_id', String(target.repositoryId));
  else if (target.repository) q.set('repository', target.repository);
  if (target.ownerId != null && target.ownerId > 0) q.set('owner_id', String(target.ownerId));
  return `/_bgh/uploads?${q}`;
}

function errorMessage(data: unknown, status: number): string {
  const d = data as { message?: string; errors?: { message?: string }[] } | null;
  return d?.errors?.find((e) => e.message)?.message ?? d?.message ?? `Upload failed (${status})`;
}

/**
 * Upload one attachment (raw body, name in the query), reporting progress
 * (0..1). XHR against the real server (fetch has no upload progress), the
 * transport in mock mode.
 */
export function uploadAttachment(file: File, target: UploadTarget, onProgress: (fraction: number) => void = () => {}, signal?: AbortSignal): Promise<Attachment> {
  const path = uploadPath(file, target);
  const contentType = file.type || 'application/octet-stream';
  if (transport() !== browserTransport) {
    onProgress(0);
    return transport()
      .fetch(path, { method: 'POST', body: file, headers: { 'Content-Type': contentType }, signal })
      .then(async (r) => {
        const data: unknown = await r.json().catch(() => null);
        if (!r.ok) throw new Error(errorMessage(data, r.status));
        onProgress(1);
        return data as Attachment;
      });
  }
  return new Promise<Attachment>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', path);
    xhr.withCredentials = true;
    xhr.setRequestHeader('Content-Type', contentType);
    xhr.setRequestHeader('Accept', 'application/json');
    const csrf = getBoot().csrf;
    if (csrf) xhr.setRequestHeader('X-CSRF-Token', csrf);
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress(e.loaded / e.total);
    xhr.onload = () => {
      let data: unknown = null;
      try {
        data = JSON.parse(xhr.responseText);
      } catch {
        /* non-JSON error page */
      }
      if (xhr.status >= 200 && xhr.status < 300) {
        onProgress(1);
        resolve(data as Attachment);
      } else {
        reject(new Error(errorMessage(data, xhr.status)));
      }
    };
    xhr.onerror = () => reject(new Error('Network error during upload'));
    xhr.onabort = () => reject(new DOMException('Aborted', 'AbortError'));
    signal?.addEventListener('abort', () => xhr.abort());
    xhr.send(file);
  });
}

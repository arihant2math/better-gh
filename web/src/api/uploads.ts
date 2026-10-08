import { getBoot } from '../boot';
import { ApiError, api, errorMessageOf, parseRetryAfter } from './client';
import { validationErrors } from './errors';
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

/**
 * POST `file` with XHR (fetch has no upload progress), reporting progress
 * (0..1). Sends the current CSRF token and rejects with `ApiError` (status
 * 0 for a network error) or an `AbortError` when `signal` fires.
 */
export function xhrUpload<T>(path: string, file: Blob, headers: Record<string, string>, onProgress: (fraction: number) => void = () => {}, signal?: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', path);
    xhr.withCredentials = true;
    for (const [k, v] of Object.entries(headers)) xhr.setRequestHeader(k, v);
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
        resolve(data as T);
      } else {
        reject(new ApiError(errorMessageOf(data, `Upload failed (${xhr.status})`), xhr.status, data, parseRetryAfter(xhr.getResponseHeader('retry-after'))));
      }
    };
    xhr.onerror = () => reject(new ApiError('Network error during upload', 0, null));
    xhr.onabort = () => reject(new DOMException('Aborted', 'AbortError'));
    signal?.addEventListener('abort', () => xhr.abort());
    xhr.send(file);
  });
}

/**
 * Upload `file` as a raw POST body with progress: XHR against the real
 * server, `api.raw` through the transport in mock mode (no progress events).
 */
export async function uploadFile<T>(path: string, file: Blob, opts: { accept: string; headers?: Record<string, string>; onProgress?: (fraction: number) => void; signal?: AbortSignal }): Promise<T> {
  const contentType = file.type || 'application/octet-stream';
  const onProgress = opts.onProgress ?? (() => {});
  if (transport() === browserTransport) return xhrUpload<T>(path, file, { 'Content-Type': contentType, Accept: opts.accept, ...opts.headers }, onProgress, opts.signal);
  onProgress(0);
  const data = await api.raw<T>(path, { method: 'POST', body: file, contentType, accept: opts.accept, headers: opts.headers, signal: opts.signal });
  onProgress(1);
  return data;
}

/** Upload one attachment (raw body, name in the query), reporting progress (0..1). */
export function uploadAttachment(file: File, target: UploadTarget, onProgress: (fraction: number) => void = () => {}, signal?: AbortSignal): Promise<Attachment> {
  return uploadFile<Attachment>(uploadPath(file, target), file, { accept: 'application/json', onProgress, signal }).catch((e: unknown) => {
    // The upload endpoint explains rejected files in a field error ("Validation Failed").
    const detail = e instanceof ApiError ? validationErrors(e).find((x) => x.message)?.message : undefined;
    throw detail && e instanceof ApiError ? new ApiError(detail, e.status, e.body, e.retryAfterMs) : e;
  });
}

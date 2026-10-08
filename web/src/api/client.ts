import { getBoot } from '../boot';
import { onReset, sameSession } from './reset';
import { transport } from './transport';

export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly body: unknown,
    /** The response's `Retry-After`, in ms from when it arrived (429/503). */
    readonly retryAfterMs?: number,
  ) {
    super(message);
  }
}

/** `Retry-After` (delay-seconds or HTTP-date) → ms from `now`; `undefined` when absent or invalid. */
export function parseRetryAfter(value: string | null | undefined, now = Date.now()): number | undefined {
  if (!value) return undefined;
  const v = value.trim();
  if (/^\d+$/.test(v)) return Number(v) * 1000;
  const at = Date.parse(v);
  return Number.isNaN(at) ? undefined : Math.max(0, at - now);
}

export interface RequestOptions {
  method?: 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE';
  body?: unknown;
  /** Accept header; defaults to `application/vnd.github+json`. */
  accept?: string;
  /** `X-Client-Tx` for optimistic mutations (docs/SYNC_PROTOCOL.md §7). */
  tx?: string;
  headers?: Record<string, string>;
  signal?: AbortSignal;
  /** Parse the body as text instead of JSON. */
  text?: boolean;
  /** Don't prompt for sudo mode on a sudo 401 (the sudo endpoints themselves). */
  noSudoPrompt?: boolean;
  /** Send this body as is (Blob, File, …) instead of JSON-encoding `body`; see `ApiClient.raw`. */
  rawBody?: BodyInit;
  /** Content-Type of `rawBody`; defaults to `application/octet-stream`. */
  contentType?: string;
}

/** Options for `ApiClient.raw`: a binary body instead of a JSON one. */
export interface RawOptions extends Omit<RequestOptions, 'body' | 'rawBody'> {
  body?: BodyInit;
}

/** The error message of a GitHub-style error body, else `fallback`. */
export function errorMessageOf(data: unknown, fallback: string): string {
  const body = data as { message?: unknown } | null;
  return body && typeof body === 'object' && body.message ? String(body.message) : fallback;
}

/** Whether an error is a 404 from the server. */
export function isNotFound(e: unknown): boolean {
  return e instanceof ApiError && e.status === 404;
}

/** Resolve to null on a 404; every other failure (5xx, auth, network) still rejects. */
export function orNullOn404<T>(p: Promise<T>): Promise<T | null> {
  return p.catch((e: unknown) => {
    if (isNotFound(e)) return null;
    throw e;
  });
}

/** Prefix of the server's 401 for sessions without a fresh sudo mode (`bgh_core::sudo::SUDO_REQUIRED`). */
export const SUDO_REQUIRED_PREFIX = 'Sudo mode required';

/** Whether an error is the server asking for sudo mode. */
export function isSudoRequired(e: unknown): boolean {
  return e instanceof ApiError && e.status === 401 && e.message.startsWith(SUDO_REQUIRED_PREFIX);
}

/** Asks the user to re-authenticate; resolves true when sudo mode was granted. */
export type SudoHandler = () => Promise<boolean>;
let sudoHandler: SudoHandler | null = null;

/** Install the sudo prompt (`app/sudoPrompt`, installed by `App`). Returns an uninstall function. */
export function setSudoHandler(handler: SudoHandler | null): () => void {
  sudoHandler = handler;
  return () => {
    if (sudoHandler === handler) sudoHandler = null;
  };
}

/**
 * Show the installed sudo prompt (for write paths outside `ApiClient`, e.g.
 * the sync tx queue). Resolves false when no prompt is installed or the user
 * cancels.
 */
export function requestSudo(): Promise<boolean> {
  return sudoHandler ? sudoHandler() : Promise.resolve(false);
}

export interface ApiResponse<T> {
  status: number;
  data: T;
  headers: Headers;
}

const ETAG_CACHE_MAX = 300;

const signedOut = () => new DOMException('Signed out while the request was in flight', 'AbortError');

/**
 * Fetch-based client for `/api/v3` (GitHub REST) and `/_bgh` (private).
 * - sends CSRF + X-Client-Tx headers;
 * - reuses ETags for GETs (`If-None-Match` → 304 → cached body), bypassing the browser HTTP cache;
 * - throws `ApiError` with GitHub's `message` on non-2xx.
 */
export class ApiClient {
  private etags = new Map<string, { etag: string; data: unknown }>();

  constructor() {
    // ETag bodies are the previous viewer's responses.
    onReset(() => this.etags.clear());
  }

  async request<T>(path: string, opts: RequestOptions = {}): Promise<ApiResponse<T>> {
    try {
      return await this.send<T>(path, opts);
    } catch (e) {
      // Sensitive actions need a recent re-authentication: prompt, then retry once.
      if (!opts.noSudoPrompt && sudoHandler && isSudoRequired(e) && (await sudoHandler())) return this.send<T>(path, { ...opts, noSudoPrompt: true });
      throw e;
    }
  }

  private async send<T>(path: string, opts: RequestOptions): Promise<ApiResponse<T>> {
    const method = opts.method ?? 'GET';
    const headers: Record<string, string> = {
      Accept: opts.accept ?? 'application/vnd.github+json',
      ...opts.headers,
    };
    if (method !== 'GET') {
      const csrf = getBoot().csrf;
      if (csrf) headers['X-CSRF-Token'] = csrf;
    }
    if (opts.tx) headers['X-Client-Tx'] = opts.tx;
    let body: BodyInit | undefined;
    if (opts.rawBody !== undefined) {
      headers['Content-Type'] = opts.contentType ?? 'application/octet-stream';
      body = opts.rawBody;
    } else if (opts.body !== undefined) {
      headers['Content-Type'] = 'application/json';
      body = JSON.stringify(opts.body);
    }
    const cacheKey = `${headers.Accept} ${path}`;
    const cached = method === 'GET' ? this.etags.get(cacheKey) : undefined;
    if (cached) headers['If-None-Match'] = cached.etag;

    // Responses carry `max-age` (REST 60 s like GitHub, ref-based `/_bgh` 30 s):
    // always revalidate so the browser's HTTP cache can't serve data we just
    // changed. ETags make that a cheap 304; immutable SHA-addressed data is
    // kept by the in-memory resource cache and never refetched anyway.
    const live = sameSession();
    const res = await transport().fetch(path, { method, headers, body, signal: opts.signal, cache: 'no-cache' });
    // Signed out (or switched account) while in flight: never hand the
    // previous viewer's response to callers that cache it.
    if (!live()) throw signedOut();

    if (res.status === 304 && cached) {
      // Refresh LRU position.
      this.etags.delete(cacheKey);
      this.etags.set(cacheKey, cached);
      return { status: 200, data: cached.data as T, headers: res.headers };
    }
    const isJson = (res.headers.get('content-type') ?? '').includes('json');
    const data: unknown =
      res.status === 204 ? null : opts.text || !isJson ? await res.text() : await res.json().catch(() => null);
    if (!live()) throw signedOut();
    if (!res.ok) {
      const message = errorMessageOf(data, `${method} ${path} failed (${res.status})`);
      throw new ApiError(message, res.status, data, parseRetryAfter(res.headers.get('retry-after')));
    }
    const etag = res.headers.get('etag');
    if (method === 'GET' && etag) {
      this.etags.set(cacheKey, { etag, data });
      if (this.etags.size > ETAG_CACHE_MAX) {
        const oldest = this.etags.keys().next();
        if (!oldest.done) this.etags.delete(oldest.value);
      }
    }
    return { status: res.status, data: data as T, headers: res.headers };
  }

  async get<T>(path: string, opts?: Omit<RequestOptions, 'method' | 'body'>): Promise<T> {
    return (await this.request<T>(path, opts)).data;
  }

  async post<T>(path: string, body?: unknown, opts?: RequestOptions): Promise<T> {
    return (await this.request<T>(path, { ...opts, method: 'POST', body })).data;
  }

  async patch<T>(path: string, body?: unknown, opts?: RequestOptions): Promise<T> {
    return (await this.request<T>(path, { ...opts, method: 'PATCH', body })).data;
  }

  async put<T>(path: string, body?: unknown, opts?: RequestOptions): Promise<T> {
    return (await this.request<T>(path, { ...opts, method: 'PUT', body })).data;
  }

  async delete<T>(path: string, opts?: RequestOptions): Promise<T> {
    return (await this.request<T>(path, { ...opts, method: 'DELETE' })).data;
  }

  /** A request with a binary body (uploads), with the same CSRF, sudo retry and errors as JSON ones. */
  async raw<T>(path: string, opts: RawOptions = {}): Promise<T> {
    const { body, ...rest } = opts;
    return (await this.request<T>(path, { ...rest, rawBody: body })).data;
  }
}

export const api = new ApiClient();

/** Build an `/api/v3` path with encoded segments: `v3('repos', owner, repo, 'issues', 4)`. */
export function v3(...segments: (string | number)[]): string {
  return `/api/v3/${segments.map((s) => encodeURIComponent(String(s))).join('/')}`;
}

/** Encode a file path but keep its slashes. */
export function encodePath(path: string): string {
  return path.split('/').map(encodeURIComponent).join('/');
}

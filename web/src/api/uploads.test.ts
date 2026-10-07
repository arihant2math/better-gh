// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { getBoot, setBoot } from '../boot';
import { TxRejectedError } from '../sync/transactions';
import { ApiError, api, isNotFound, orNullOn404 } from './client';
import { uploadReleaseAsset } from './code';
import { browserTransport, setTransport, type Transport } from './transport';
import { uploadAttachment } from './uploads';
import { uploadAvatar } from './userSettings';

// #247: binary requests share ApiClient's CSRF handling and ApiError.

/** Records one XHR and answers it with `next`. */
class FakeXhr {
  static last: FakeXhr | null = null;
  static next: { status: number; body: string } = { status: 201, body: '{}' };
  headers: Record<string, string> = {};
  status = 0;
  responseText = '';
  upload = { onprogress: null as ((e: ProgressEvent) => void) | null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  withCredentials = false;
  open() {}
  setRequestHeader(k: string, v: string) {
    this.headers[k] = v;
  }
  getResponseHeader() {
    return null;
  }
  abort() {}
  send() {
    FakeXhr.last = this;
    this.status = FakeXhr.next.status;
    this.responseText = FakeXhr.next.body;
    queueMicrotask(() => this.onload?.());
  }
}

const file = () => new File(['x'], 'a.bin', { type: 'application/octet-stream' });
const initialBoot = getBoot();

beforeEach(() => {
  vi.stubGlobal('XMLHttpRequest', FakeXhr);
  FakeXhr.last = null;
  FakeXhr.next = { status: 201, body: '{}' };
});
afterEach(() => {
  vi.unstubAllGlobals();
  setTransport(browserTransport);
  setBoot(initialBoot);
  delete window.__BGH_BOOT__;
});

describe('release asset upload (XHR)', () => {
  it('sends the current boot CSRF token, not the stale inline one', async () => {
    window.__BGH_BOOT__ = { ...initialBoot, csrf: 'stale' };
    setBoot({ ...initialBoot, csrf: 'fresh' });
    await uploadReleaseAsset('o', 'r', 1, file(), () => {});
    expect(FakeXhr.last?.headers['X-CSRF-Token']).toBe('fresh');
  });

  it.each([413, 415, 403])('rejects with ApiError carrying status %i', async (status) => {
    FakeXhr.next = { status, body: JSON.stringify({ message: `nope ${status}` }) };
    const err = await uploadReleaseAsset('o', 'r', 1, file(), () => {}).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(status);
    expect((err as ApiError).message).toBe(`nope ${status}`);
  });

  it('keeps the status for a non-JSON error page', async () => {
    FakeXhr.next = { status: 413, body: '<html>Request Entity Too Large</html>' };
    const err = await uploadAttachment(file(), {}).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(413);
  });

  it('attachment errors surface the field error message', async () => {
    FakeXhr.next = { status: 422, body: JSON.stringify({ message: 'Validation Failed', errors: [{ message: 'File type not allowed' }] }) };
    const err = await uploadAttachment(file(), {}).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).message).toBe('File type not allowed');
    expect((err as ApiError).status).toBe(422);
  });
});

describe('uploads through the transport (mock mode)', () => {
  const seen: { path: string; init?: RequestInit }[] = [];
  const respond = (status: number, body: string, type = 'application/json'): Transport => ({
    fetch: async (path, init) => {
      seen.push({ path, init });
      return new Response(body, { status, headers: { 'Content-Type': type } });
    },
    socket: () => {
      throw new Error('no socket');
    },
  });
  beforeEach(() => {
    seen.length = 0;
    setBoot({ ...initialBoot, csrf: 'tok' });
  });

  it('release asset: non-JSON 415 is an ApiError, not a SyntaxError', async () => {
    setTransport(respond(415, 'unsupported', 'text/plain'));
    const err = await uploadReleaseAsset('o', 'r', 1, file(), () => {}).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(415);
  });

  it('avatar upload sends the raw body with CSRF and content type', async () => {
    setTransport(respond(200, JSON.stringify({ avatar_url: '/a.png' })));
    const blob = new Blob(['img'], { type: 'image/webp' });
    await expect(uploadAvatar(blob)).resolves.toEqual({ avatar_url: '/a.png' });
    const headers = seen[0]!.init!.headers as Record<string, string>;
    expect(seen[0]!.init!.method).toBe('PUT');
    expect(seen[0]!.init!.body).toBe(blob);
    expect(headers['X-CSRF-Token']).toBe('tok');
    expect(headers['Content-Type']).toBe('image/webp');
  });

  it('api.raw surfaces 403 as ApiError', async () => {
    setTransport(respond(403, JSON.stringify({ message: 'Forbidden' })));
    await expect(api.raw('/x', { method: 'PUT', body: file() })).rejects.toMatchObject({ status: 403, message: 'Forbidden' });
  });
});

describe('error helpers', () => {
  it('TxRejectedError is an ApiError', () => {
    const e = new TxRejectedError('gone', 410);
    expect(e).toBeInstanceOf(ApiError);
    expect(e.status).toBe(410);
  });

  it('orNullOn404 maps 404 to null and rethrows everything else', async () => {
    await expect(orNullOn404(Promise.reject(new ApiError('nf', 404, null)))).resolves.toBeNull();
    await expect(orNullOn404(Promise.resolve(1))).resolves.toBe(1);
    const boom = new ApiError('boom', 500, null);
    await expect(orNullOn404(Promise.reject(boom))).rejects.toBe(boom);
    expect(isNotFound(new ApiError('nf', 404, null))).toBe(true);
    expect(isNotFound({ status: 404 })).toBe(false);
  });
});

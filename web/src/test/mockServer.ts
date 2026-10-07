/**
 * Shared helpers for tests that drive the mock backend. Importing this module
 * loads the lazy mock feature chunks, so only mock-using tests pay for them
 * (the global `setup.ts` stays light for pure-logic tests).
 */
import { loadMockFeatures } from '../mock/features';
import { MockServer, type MockOptions } from '../mock/server';

await loadMockFeatures();

/** Loose JSON object for asserting on mock responses. */
export type Json = Record<string, any>; // eslint-disable-line @typescript-eslint/no-explicit-any
/** Default response body type: untyped, so assertions can index freely. */
type Loose = any; // eslint-disable-line @typescript-eslint/no-explicit-any

export interface MockResponse<T> {
  status: number;
  /** Parsed JSON, the raw text for non-JSON responses, or `null` when empty. */
  body: T;
  headers: Headers;
}

/** A fresh seeded mock server; `signedIn: false` starts it signed out. */
export function newServer(opts: MockOptions & { signedIn?: boolean } = {}): MockServer {
  const { signedIn, ...rest } = opts;
  const s = new MockServer(null, rest);
  if (signedIn !== undefined) s.signedIn = signedIn;
  return s;
}

/**
 * One request against the mock server. A `Blob` body is sent as is, anything
 * else is JSON-encoded. `T` defaults to an untyped body.
 */
export async function call<T = Loose>(s: MockServer, method: string, path: string, body?: unknown, headers: Record<string, string> = {}): Promise<MockResponse<T>> {
  const init: RequestInit = { method, headers };
  if (body instanceof Blob) init.body = body;
  else if (body !== undefined) {
    init.body = JSON.stringify(body);
    init.headers = { 'content-type': 'application/json', ...headers };
  }
  const res = await s.fetch(path, init);
  const text = await res.text();
  return { status: res.status, body: parse(text, res.headers.get('content-type')) as T, headers: res.headers };
}

export function get<T = Loose>(s: MockServer, path: string, headers?: Record<string, string>): Promise<MockResponse<T>> {
  return call<T>(s, 'GET', path, undefined, headers);
}

export function post<T = Loose>(s: MockServer, path: string, body?: unknown, headers?: Record<string, string>): Promise<MockResponse<T>> {
  return call<T>(s, 'POST', path, body, headers);
}

function parse(text: string, contentType: string | null): unknown {
  if (!text) return null;
  if (contentType?.includes('json')) return JSON.parse(text);
  if (contentType) return text;
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

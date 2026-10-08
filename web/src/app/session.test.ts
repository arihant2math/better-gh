import { runInAction } from 'mobx';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { load, peek } from '../api/cache';
import { api } from '../api/client';
import { browserTransport, setTransport, type Transport } from '../api/transport';
import { setBoot, type BootData } from '../boot';
import { feedFor } from '../pages/dashboard/feed';
import { threadSubs } from '../pages/notifications/actions';
import { paletteSearch, peekPalette } from '../search/api';
import { setViewerReaction, viewerReactions } from '../sync/viewerReactions';
import { arrivals } from './unread';
import { session } from './session';

// Session teardown without a real sync client, IndexedDB or router.
vi.mock('../router', () => ({ navigate: vi.fn(), prefetch: vi.fn() }));
vi.mock('../sync/persistence', () => ({
  openPersistence: () => Promise.resolve({ close: () => undefined }),
  IdbPersistence: { destroy: () => Promise.resolve() },
}));
vi.mock('../sync/client', () => ({
  SyncClient: class {
    ready = Promise.resolve();
    start = () => Promise.resolve();
    stop = () => undefined;
  },
}));

const boot = (id: number, login: string): BootData => ({
  user: { id, login, name: login, avatarUrl: '' },
  csrf: `csrf-${login}`,
  config: { siteName: 'x', signupEnabled: true, version: 'test' },
});

const json = (body: unknown, headers: Record<string, string> = {}) =>
  new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json', ...headers } });

/** Fake server: `who` is the account the cookie belongs to; `held` requests wait for a manual release. */
let who = '';
const held = new Map<string, () => void>();

const server: Transport = {
  async fetch(path, init) {
    const method = init?.method ?? 'GET';
    if (method === 'POST' && path === '/_bgh/auth/login') {
      const { login } = JSON.parse(String(init?.body)) as { login: string };
      who = login;
      return json(boot(login === 'alice' ? 1 : 2, login));
    }
    if (method === 'POST' && path === '/_bgh/auth/logout') {
      who = '';
      return new Response(null, { status: 204 });
    }
    const viewer = who;
    if (path.startsWith('/slow/') || path.includes('q=late')) await new Promise<void>((r) => held.set(path, r));
    if (path === '/etag') {
      // Answers 304 to any matching ETag, like a server whose ETag only hashes the path.
      const inm = (init?.headers as Record<string, string> | undefined)?.['If-None-Match'];
      return inm === '"v1"' ? new Response(null, { status: 304 }) : json({ viewer }, { etag: '"v1"' });
    }
    return json({ viewer, path, repos: [{ full_name: `${viewer}/private` }], issues: [], users: [] });
  },
  socket: () => {
    throw new Error('no sockets in tests');
  },
};

async function signIn(login: string) {
  await session.login(login, 'pw');
  expect(session.user?.login).toBe(login);
}

beforeEach(() => {
  vi.stubGlobal('location', { pathname: '/' });
  setTransport(server);
  setBoot({ ...boot(0, 'nobody'), user: null });
  session.init();
});

afterEach(async () => {
  if (session.user) await session.logout();
  setTransport(browserTransport);
  vi.unstubAllGlobals();
});

describe('resetClientState on sign-out', () => {
  it("serves none of account A's cached data to account B", async () => {
    await signIn('alice');
    await load('user:emails', () => api.get('/user/emails'));
    await load('blob:abc', () => api.get('/blob/abc'), { immutable: true });
    await paletteSearch('secret', { kind: 'global' });
    await api.get('/etag');
    const aliceFeed = feedFor(null);
    setViewerReaction({ kind: 'issue', id: 7 }, '+1', true);
    runInAction(() => {
      threadSubs.set(9, false);
      arrivals.set(9, Date.now());
    });
    expect(peek('user:emails')).toMatchObject({ viewer: 'alice' });

    await session.logout();
    await signIn('bob');

    expect(peek('user:emails')).toBeUndefined();
    expect(peek('blob:abc')).toBeUndefined();
    expect(peekPalette('secret', { kind: 'global' })).toBeUndefined();
    // The ETag cache would otherwise answer a 304 with Alice's body.
    expect(await api.get('/etag')).toEqual({ viewer: 'bob' });
    expect(feedFor(null)).not.toBe(aliceFeed);
    expect(viewerReactions({ kind: 'issue', id: 7 })).toEqual([]);
    expect(threadSubs.has(9)).toBe(false);
    expect(arrivals.has(9)).toBe(false);
    expect(await load('user:emails', () => api.get('/user/emails'))).toMatchObject({ viewer: 'bob' });
  });

  it("drops A's in-flight requests that settle after sign-out", async () => {
    await signIn('alice');
    const cached = load('slow:cache', () => api.get('/slow/cache'));
    const palette = paletteSearch('late', { kind: 'global' });
    await vi.waitFor(() => expect(held.size).toBe(2));

    await session.logout();
    await signIn('bob');
    for (const release of held.values()) release();
    held.clear();

    await expect(cached).rejects.toMatchObject({ name: 'AbortError' });
    await expect(palette).rejects.toMatchObject({ name: 'AbortError' });
    expect(peek('slow:cache')).toBeUndefined();
    expect(peekPalette('late', { kind: 'global' })).toBeUndefined();
  });

  it('also resets when the session expires', async () => {
    await signIn('alice');
    await load('orgs:settings', () => api.get('/orgs/acme/settings'));
    await paletteSearch('mine', { kind: 'global' });

    session.expired();

    expect(session.user).toBeNull();
    expect(peek('orgs:settings')).toBeUndefined();
    expect(peekPalette('mine', { kind: 'global' })).toBeUndefined();
  });
});

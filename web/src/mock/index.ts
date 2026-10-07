/**
 * Installs the in-browser mock backend as the app's transport.
 * Loaded via dynamic import only in mock mode (never part of the main bundle).
 *
 * URL flags: `?mock` enable · `?mock=0` disable · `&live=0` no simulated
 * activity · `&fail=0.2` 20% retryable failures · `&latency=0` instant ·
 * `&reset` wipe mock + client databases.
 */
import { setTransport } from '../api/transport';
import { setBoot } from '../boot';
import { MockServer, resetMockState } from './server';

export async function installMock(): Promise<MockServer> {
  const params = new URLSearchParams(window.location.search);
  if (params.has('reset')) {
    await resetMockState();
    try {
      localStorage.removeItem('bgh-mock-commit-comments'); // mock/commitComments.ts
    } catch {
      /* ignore */
    }
    const { deleteDB } = await import('idb');
    for (const db of (await indexedDB.databases?.()) ?? []) {
      if (db.name?.startsWith('bgh-')) await deleteDB(db.name);
    }
  }
  const latency = params.get('latency');
  const server = await MockServer.create({
    persist: true,
    live: params.get('live') !== '0',
    failRate: Number(params.get('fail') ?? 0),
    latency: latency === '0' ? [0, 0] : [40, 160],
  });
  setTransport(server);
  setBoot(server.boot());
  (window as unknown as { __bghMock?: MockServer }).__bghMock = server;
  return server;
}

export { MockServer } from './server';

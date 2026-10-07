import { afterEach, describe, expect, it, vi } from 'vitest';
import { SyncClient } from '../sync/client';
import { setSyncClient } from '../sync/index';
import { MemoryPersistence } from '../sync/persistence';
import { SUDO_REQUIRED_PREFIX, requestSudo, setSudoHandler } from './client';
import { deleteRepo, transferRepo } from './repoSettings';
import type { Transport } from './transport';
import { newServer } from '../test/mockServer';

// #242: repo delete/transfer go through the sync tx queue; an expired sudo
// mode must prompt and retry, never log the user out.

const until = async (cond: () => boolean, ms = 3000) => {
  const start = Date.now();
  while (!cond()) {
    if (Date.now() - start > ms) throw new Error('timeout waiting for condition');
    await new Promise((r) => setTimeout(r, 5));
  }
};

const cleanups: (() => void)[] = [];
afterEach(() => {
  cleanups.splice(0).forEach((f) => f());
  setSyncClient(null);
});

/** Mock server whose sudo mode has expired: `path` answers 401 SUDO_REQUIRED until `grant()`. */
async function setup(method: string, path: string) {
  const server = newServer();
  let sudo = false;
  const hits: number[] = [];
  const transport: Transport = {
    socket: (p) => server.socket(p),
    fetch: async (p, init) => {
      if (p === path && (init?.method ?? 'GET') === method) {
        hits.push(sudo ? 1 : 0);
        if (!sudo)
          return new Response(JSON.stringify({ message: `${SUDO_REQUIRED_PREFIX}: confirm your password and retry.` }), {
            status: 401,
            headers: { 'Content-Type': 'application/json' },
          });
      }
      return server.fetch(p, init);
    },
  };
  const onUnauthenticated = vi.fn();
  const client = new SyncClient({
    userId: server.db.viewerId,
    transport,
    persistence: new MemoryPersistence(),
    // As wired by `app/session.ts`.
    hooks: { onUnauthenticated, onSudoRequired: requestSudo },
  });
  cleanups.push(() => client.stop());
  await client.start();
  await until(() => client.status === 'live');
  setSyncClient(client);
  const prompt = vi.fn(async (ok: boolean) => {
    sudo = ok;
    return ok;
  });
  const repo = client.pool.all('repo').find((r) => r.owner === 'ada' && r.name === 'dotfiles')!;
  return { server, client, repo, hits, onUnauthenticated, prompt };
}

describe('sudo-protected repo actions with expired sudo (#242)', () => {
  it('delete prompts for sudo, retries and deletes', async () => {
    const { server, client, repo, hits, onUnauthenticated, prompt } = await setup('DELETE', '/api/v3/repos/ada/dotfiles');
    cleanups.push(setSudoHandler(() => prompt(true)));
    await deleteRepo(repo);
    expect(prompt).toHaveBeenCalledTimes(1);
    expect(hits).toEqual([0, 1]);
    expect(server.repo('ada', 'dotfiles')).toBeFalsy();
    expect(onUnauthenticated).not.toHaveBeenCalled();
    expect(client.queue.paused).toBe(false);
  });

  it('delete is cancelled (not logged out) when the prompt is dismissed', async () => {
    const { server, client, repo, onUnauthenticated, prompt } = await setup('DELETE', '/api/v3/repos/ada/dotfiles');
    cleanups.push(setSudoHandler(() => prompt(false)));
    await expect(deleteRepo(repo)).rejects.toThrow(SUDO_REQUIRED_PREFIX);
    expect(server.repo('ada', 'dotfiles')).toBeTruthy();
    expect(onUnauthenticated).not.toHaveBeenCalled();
    expect(client.queue.paused).toBe(false);
  });

  it('transfer prompts for sudo, retries and transfers', async () => {
    const { server, client, repo, hits, onUnauthenticated, prompt } = await setup('POST', '/api/v3/repos/ada/dotfiles/transfer');
    cleanups.push(setSudoHandler(() => prompt(true)));
    const done = transferRepo(repo, 'acme');
    // Optimistic owner change applies right away.
    expect(client.pool.get('repo', repo.id)?.owner).toBe('acme');
    await done;
    expect(prompt).toHaveBeenCalledTimes(1);
    expect(hits).toEqual([0, 1]);
    expect(server.repo('acme', 'dotfiles')).toBeTruthy();
    expect(onUnauthenticated).not.toHaveBeenCalled();
    expect(client.queue.paused).toBe(false);
  });

  it('transfer rolls back (not logged out) when the prompt is dismissed', async () => {
    const { server, client, repo, onUnauthenticated, prompt } = await setup('POST', '/api/v3/repos/ada/dotfiles/transfer');
    cleanups.push(setSudoHandler(() => prompt(false)));
    await expect(transferRepo(repo, 'acme')).rejects.toThrow(SUDO_REQUIRED_PREFIX);
    expect(client.pool.get('repo', repo.id)?.owner).toBe('ada');
    expect(server.repo('ada', 'dotfiles')).toBeTruthy();
    expect(onUnauthenticated).not.toHaveBeenCalled();
    expect(client.queue.paused).toBe(false);
  });
});

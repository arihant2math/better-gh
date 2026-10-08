/**
 * Public entry point of the local-first store.
 *
 *   import { store, sync } from './';
 *   const issue = store().byKey('issue', 'number', `${repoId}#${n}`);
 *
 * Read in `observer` components; they re-render on exactly what they read.
 */
import type { SyncClient } from './client';
import type { ObjectPool } from './pool';

let current: SyncClient | null = null;

export function setSyncClient(c: SyncClient | null): void {
  current = c;
}

/** The running sync client (throws before sign-in). */
export function sync(): SyncClient {
  if (!current) throw new Error('sync client not started');
  return current;
}

export function hasSync(): boolean {
  return current !== null;
}

/** The object pool (normalized reactive store). */
export function store(): ObjectPool {
  return sync().pool;
}

export type { SyncClient } from './client';
export type { ObjectPool } from './pool';
export * from './models';

/** Wire types for docs/SYNC_PROTOCOL.md. */
import type { ID, ModelMap, ModelName, User } from './models';

export const PROTOCOL_SCHEMA_VERSION = 1;

export type ModelRows = { [M in ModelName]?: ModelMap[M][] };

export interface BootstrapResponse {
  schemaVersion: number;
  lastSyncId: number;
  userId: ID;
  scopes: string[];
  denied: string[];
  models: ModelRows;
}

export interface PartialResponse {
  lastSyncId: number;
  models: ModelRows;
}

export type DeltaAction = 'I' | 'U' | 'D';

export interface Delta {
  t?: 'delta';
  id: number;
  scope: string;
  model: ModelName;
  mid: ID;
  a: DeltaAction;
  d: Record<string, unknown> | null;
  tx?: string;
  refs?: { user?: User[] };
}

export type ServerMessage =
  | { t: 'hello'; userId: ID; head: number }
  | (Delta & { t: 'delta' })
  | { t: 'batch'; id: number; items: Delta[] }
  | { t: 'ready'; scopes: string[]; id: number }
  | { t: 'revoke'; scope: string; reason?: string }
  | { t: 'rebootstrap'; reason: 'too_old' | 'schema' | string }
  | { t: 'pong' }
  | { t: 'error'; code: string; message: string; scope?: string };

export type ClientMessage =
  | { t: 'sub'; scopes: string[]; since: number }
  | { t: 'unsub'; scopes: string[] }
  | { t: 'ping' };

export const WS_CLOSE_UNAUTHENTICATED = 4001;
export const WS_CLOSE_REBOOTSTRAP = 4009;

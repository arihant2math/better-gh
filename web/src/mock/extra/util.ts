import type { Ctx, MockServer, Resp } from '../server';

export type { Ctx, Resp };

const states = new WeakMap<MockServer, Map<string, unknown>>();

/** Per-server, per-area state, created on first use. */
export function state<T>(server: MockServer, area: string, init: () => T): T {
  let m = states.get(server);
  if (!m) states.set(server, (m = new Map()));
  if (!m.has(area)) m.set(area, init());
  return m.get(area) as T;
}

export const ok = (body: unknown, status = 200): Resp => ({ status, body });
export const noContent = (): Resp => ({ status: 204 });
export const notFound = (): Resp => ({ status: 404, body: { message: 'Not Found', documentation_url: 'https://docs.github.com/rest' } });

/** GitHub-style validation error. */
export function invalid(message: string, field?: string, code = 'invalid', resource = 'Resource'): Resp {
  return { status: 422, body: { message, errors: field ? [{ resource, field, code, message }] : [], documentation_url: 'https://docs.github.com/rest' } };
}

export const param = (ctx: Ctx, i: number): string => decodeURIComponent(ctx.m[i] ?? '');

export function simpleUser(server: MockServer, id: number): Record<string, unknown> | null {
  const u = server.db.tables.user.get(id);
  if (!u) return null;
  return { login: u.login, id: u.id, node_id: btoa(`04:User${u.id}`), avatar_url: u.avatarUrl, type: u.type, site_admin: false, name: u.name };
}

/**
 * Mock issue types, dependencies and close-as-duplicate (P41). Shapes
 * follow crates/bgh-issues (issue_types.rs, dependencies.rs, issues.rs):
 * `/orgs/{org}/issue-types` CRUD, `…/issues/{n}/dependencies/blocked_by`
 * and `/blocking`, and the `type` / `duplicate_of` fields of
 * `PATCH …/issues/{n}` (handled here, the rest falls through to the
 * built-in route).
 */
import type { ID, Issue, IssueEvent, IssueTypeColor, Repo } from '../../sync/models';
import { pass } from '../pass';
import type { Ctx, MockServer, Resp } from '../server';
import { invalid, noContent, notFound, ok, param, state } from './util';

interface IssueTypeRow {
  id: number;
  node_id: string;
  name: string;
  description: string | null;
  color: IssueTypeColor | null;
  created_at: string;
  updated_at: string;
  is_enabled: boolean;
}

const COLORS: IssueTypeColor[] = ['gray', 'blue', 'green', 'yellow', 'orange', 'red', 'pink', 'purple'];

const S = (server: MockServer) => state<{ types: Map<ID, IssueTypeRow[]> }>(server, 'issue-types', () => ({ types: new Map() }));

function orgByLogin(server: MockServer, login: string) {
  const l = login.toLowerCase();
  for (const o of server.db.tables.org.values()) if (o.login.toLowerCase() === l) return o;
  return undefined;
}

/** An organization's types, seeded with GitHub's defaults on first use. */
function typesFor(server: MockServer, orgId: ID): IssueTypeRow[] {
  const s = S(server);
  let list = s.types.get(orgId);
  if (!list) {
    const now = server.now();
    const mk = (name: string, description: string, color: IssueTypeColor): IssueTypeRow => {
      const id = server.nextId();
      return { id, node_id: btoa(`09:IssueType${id}`), name, description, color, created_at: now, updated_at: now, is_enabled: true };
    };
    list = [mk('Task', 'A specific piece of work', 'yellow'), mk('Bug', 'An unexpected problem or behavior', 'red'), mk('Feature', 'A request, idea, or new functionality', 'blue')];
    s.types.set(orgId, list);
  }
  return list;
}

function event(server: MockServer, issue: Issue, kind: IssueEvent['event'], data: IssueEvent['data']): void {
  server.put('issueEvent', { id: server.nextId(), repoId: issue.repoId, issueId: issue.id, actorId: server.db.viewerId, event: kind, data, createdAt: server.now() });
}

function ref(server: MockServer, i: Issue): IssueEvent['data'] {
  const r = server.db.tables.repo.get(i.repoId);
  return { otherIssueId: i.id, otherIssueNumber: i.number, otherIssueRepository: r ? `${r.owner}/${r.name}` : undefined };
}

function issueAt(server: MockServer, ctx: Ctx): [Repo, Issue] | Resp {
  const repo = server.repo(param(ctx, 1), param(ctx, 2));
  if (!repo) return notFound();
  const issue = server.issue(repo, Number(ctx.m[3]));
  return issue ? [repo, issue] : notFound();
}

const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !Array.isArray(x);

export function installRelationshipMocks(server: MockServer): void {
  const R = (method: string, pattern: string, handler: (ctx: Ctx) => Resp, override = false) => server.route(method, pattern, handler, { override });

  // ---------------- organization issue types
  R('GET', '/api/v3/orgs/:org/issue-types', (ctx) => {
    const org = orgByLogin(server, param(ctx, 1));
    return org ? ok(typesFor(server, org.id)) : notFound();
  });
  const validate = (ctx: Ctx, list: IssueTypeRow[], self?: number): Resp | null => {
    const name = String(ctx.body.name ?? '').trim();
    if (!name) return invalid('Validation Failed', 'name', 'missing_field', 'IssueType');
    if (typeof ctx.body.is_enabled !== 'boolean') return invalid('Validation Failed', 'is_enabled', 'missing_field', 'IssueType');
    if (ctx.body.color != null && !COLORS.includes(ctx.body.color as IssueTypeColor)) return invalid('Validation Failed', 'color', 'invalid', 'IssueType');
    if (list.some((t) => t.id !== self && t.name.toLowerCase() === name.toLowerCase())) return invalid('Validation Failed', 'name', 'already_exists', 'IssueType');
    return null;
  };
  R('POST', '/api/v3/orgs/:org/issue-types', (ctx) => {
    const org = orgByLogin(server, param(ctx, 1));
    if (!org) return notFound();
    const list = typesFor(server, org.id);
    const err = validate(ctx, list);
    if (err) return err;
    const id = server.nextId();
    const now = server.now();
    const row: IssueTypeRow = {
      id,
      node_id: btoa(`09:IssueType${id}`),
      name: String(ctx.body.name).trim(),
      description: (ctx.body.description as string | null) || null,
      color: (ctx.body.color as IssueTypeColor | null) ?? null,
      created_at: now,
      updated_at: now,
      is_enabled: ctx.body.is_enabled as boolean,
    };
    list.push(row);
    return ok(row);
  });
  R('PUT', '/api/v3/orgs/:org/issue-types/:id', (ctx) => {
    const org = orgByLogin(server, param(ctx, 1));
    if (!org) return notFound();
    const list = typesFor(server, org.id);
    const at = list.findIndex((t) => t.id === Number(ctx.m[2]));
    if (at < 0) return notFound();
    const err = validate(ctx, list, list[at]!.id);
    if (err) return err;
    const row: IssueTypeRow = {
      ...list[at]!,
      name: String(ctx.body.name).trim(),
      description: (ctx.body.description as string | null) || null,
      color: (ctx.body.color as IssueTypeColor | null) ?? null,
      is_enabled: ctx.body.is_enabled as boolean,
      updated_at: server.now(),
    };
    list[at] = row;
    for (const i of server.db.tables.issue.values())
      if (i.issueType?.id === row.id) server.put('issue', { ...i, issueType: { id: row.id, name: row.name, color: row.color } });
    return ok(row);
  });
  R('DELETE', '/api/v3/orgs/:org/issue-types/:id', (ctx) => {
    const org = orgByLogin(server, param(ctx, 1));
    if (!org) return notFound();
    const list = typesFor(server, org.id);
    const id = Number(ctx.m[2]);
    const at = list.findIndex((t) => t.id === id);
    if (at < 0) return notFound();
    list.splice(at, 1);
    for (const i of server.db.tables.issue.values()) if (i.issueType?.id === id) server.put('issue', { ...i, issueType: null });
    return noContent();
  });

  function refreshDependents(id: ID, state: 'open' | 'closed') {
    const self = server.db.tables.issue.get(id);
    for (const dep of self?.blockingIds ?? []) {
      const d = server.db.tables.issue.get(dep);
      if (!d) continue;
      const open = (d.blockedByIds ?? []).filter((b) => (b === id ? state === 'open' : server.db.tables.issue.get(b)?.state === 'open')).length;
      if (open !== (d.openBlockedBy ?? 0)) server.put('issue', { ...d, openBlockedBy: open });
    }
  }

  // ---------------- PATCH issue: `type` and `duplicate_of`
  R(
    'PATCH',
    '/api/v3/repos/:owner/:repo/issues/:number',
    (ctx) => {
      const b = ctx.body;
      const r = issueAt(server, ctx);
      if (isResp(r)) return pass();
      const [repo, issue] = r;
      // Dependents show how many of their blockers are open.
      const newState = b.duplicate_of != null ? 'closed' : b.state;
      if ((newState === 'open' || newState === 'closed') && newState !== issue.state) refreshDependents(issue.id, newState);
      if (!('type' in b) && b.duplicate_of == null) return pass();
      if ('type' in b) {
        const org = server.db.tables.org.get(repo.ownerId);
        const t = b.type == null ? null : org ? typesFor(server, org.id).find((x) => x.is_enabled && x.name.toLowerCase() === String(b.type).toLowerCase()) : undefined;
        if (t === undefined) return invalid('Validation Failed', 'type', 'invalid', 'Issue');
        const old = issue.issueType ?? null;
        if ((old?.id ?? null) !== (t?.id ?? null)) {
          const next = t ? { id: t.id, name: t.name, color: t.color } : null;
          if (!old && next) event(server, issue, 'issue_type_added', { issueTypeName: next.name, issueTypeColor: next.color ?? undefined });
          else if (old && next)
            event(server, issue, 'issue_type_changed', {
              issueTypeName: next.name,
              issueTypeColor: next.color ?? undefined,
              prevIssueTypeName: old.name,
              prevIssueTypeColor: old.color ?? undefined,
            });
          else if (old) event(server, issue, 'issue_type_removed', { issueTypeName: old.name, issueTypeColor: old.color ?? undefined });
          server.put('issue', { ...issue, issueType: next, updatedAt: server.now() });
        }
        if (b.duplicate_of == null) {
          const { type: _t, ...rest } = b;
          if (Object.keys(rest).length) {
            ctx.body = rest;
            return pass();
          }
          return ok(server.restIssue(server.db.tables.issue.get(issue.id)!));
        }
      }
      // Close as duplicate.
      const cur = server.db.tables.issue.get(issue.id)!;
      const original = server.db.tables.issue.get(Number(b.duplicate_of));
      if (!original || original.isPr || original.id === cur.id) return invalid('Validation Failed', 'duplicate_of', 'invalid', 'Issue');
      const now = server.now();
      if (cur.state === 'open') {
        event(server, cur, 'closed', { stateReason: 'duplicate', ...ref(server, original) });
        const repoRow = server.db.tables.repo.get(cur.repoId)!;
        server.put('repo', { ...repoRow, openIssues: Math.max(0, repoRow.openIssues - 1) });
      }
      event(server, cur, 'marked_as_duplicate', ref(server, original));
      const next: Issue = { ...cur, state: 'closed', stateReason: 'duplicate', closedAt: cur.closedAt ?? now, updatedAt: now, duplicateOfId: original.id };
      server.put('issue', next);
      return ok(server.restIssue(next));
    },
    true,
  );

  // ---------------- dependencies
  const restList = (ids: ID[]) => ids.map((id) => server.db.tables.issue.get(id)).filter((i): i is Issue => !!i).map((i) => server.restIssue(i));
  R('GET', '/api/v3/repos/:owner/:repo/issues/:number/dependencies/blocked_by', (ctx) => {
    const r = issueAt(server, ctx);
    return isResp(r) ? r : ok(restList(r[1].blockedByIds ?? []));
  });
  R('GET', '/api/v3/repos/:owner/:repo/issues/:number/dependencies/blocking', (ctx) => {
    const r = issueAt(server, ctx);
    return isResp(r) ? r : ok(restList(r[1].blockingIds ?? []));
  });
  const openCount = (ids: ID[]) => ids.filter((id) => server.db.tables.issue.get(id)?.state === 'open').length;
  R('POST', '/api/v3/repos/:owner/:repo/issues/:number/dependencies/blocked_by', (ctx) => {
    const r = issueAt(server, ctx);
    if (isResp(r)) return r;
    const [, blocked] = r;
    const blocker = server.db.tables.issue.get(Number(ctx.body.issue_id));
    if (!blocker || blocker.isPr || blocker.id === blocked.id) return invalid('Validation Failed', 'issue_id', 'invalid', 'Issue');
    if ((blocked.blockedByIds ?? []).includes(blocker.id)) return invalid('Issue is already blocked by this issue');
    // Cycle: `blocked` already (transitively) blocks `blocker`.
    const seen = new Set<ID>();
    const queue = [...(blocker.blockedByIds ?? [])];
    while (queue.length) {
      const id = queue.pop()!;
      if (id === blocked.id) return invalid('Adding this dependency would create a circular dependency');
      if (seen.has(id)) continue;
      seen.add(id);
      queue.push(...(server.db.tables.issue.get(id)?.blockedByIds ?? []));
    }
    const ids = [...(blocked.blockedByIds ?? []), blocker.id];
    event(server, blocked, 'blocked_by_added', ref(server, blocker));
    event(server, blocker, 'blocking_added', ref(server, blocked));
    const now = server.now();
    server.put('issue', { ...blocker, blockingIds: [...(blocker.blockingIds ?? []), blocked.id], updatedAt: now });
    const next: Issue = { ...blocked, blockedByIds: ids, openBlockedBy: openCount(ids), updatedAt: now };
    server.put('issue', next);
    return ok(server.restIssue(next), 201);
  });
  R('DELETE', '/api/v3/repos/:owner/:repo/issues/:number/dependencies/blocked_by/:id', (ctx) => {
    const r = issueAt(server, ctx);
    if (isResp(r)) return r;
    const [, blocked] = r;
    const id = Number(ctx.m[4]);
    if (!(blocked.blockedByIds ?? []).includes(id)) return notFound();
    const blocker = server.db.tables.issue.get(id);
    const ids = (blocked.blockedByIds ?? []).filter((x) => x !== id);
    const now = server.now();
    if (blocker) {
      event(server, blocked, 'blocked_by_removed', ref(server, blocker));
      event(server, blocker, 'blocking_removed', ref(server, blocked));
      server.put('issue', { ...blocker, blockingIds: (blocker.blockingIds ?? []).filter((x) => x !== blocked.id), updatedAt: now });
    }
    const next: Issue = { ...blocked, blockedByIds: ids, openBlockedBy: openCount(ids), updatedAt: now };
    server.put('issue', next);
    return ok(server.restIssue(next));
  });
}

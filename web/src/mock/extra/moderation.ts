/**
 * Moderation mocks (P42): hide / unhide comments, edit history and issue
 * deletion. Mirrors crates/bgh-issues/src/moderation.rs. Edit history is
 * recorded by wrapping `server.put`: a changed `body` of an existing
 * issue, comment or review comment becomes one revision.
 */
import type { ID, Issue, MinimizedReason, ModelMap, ModelName } from '../../sync/models';
import type { Ctx, MockServer, Resp } from '../server';
import { invalid, noContent, notFound, ok, param, simpleUser, state } from './util';

type Kind = 'issue' | 'comment' | 'review' | 'review_comment' | 'commit_comment';

interface Edit {
  id: number;
  kind: Kind;
  targetId: ID;
  editorId: ID;
  body: string | null;
  previousBody: string | null;
  editedAt: string;
  deletedAt: string | null;
  deletedById: ID | null;
}

interface ModState {
  nextId: number;
  edits: Edit[];
  /** Hidden commit comments (not a synced model). */
  commitHidden: Map<number, MinimizedReason>;
  /** `repoId#number` of deleted issues. */
  deleted: Set<string>;
}

const S = (server: MockServer) => state<ModState>(server, 'moderation', () => ({ nextId: 1, edits: [], commitHidden: new Map(), deleted: new Set() }));

const REASONS: readonly MinimizedReason[] = ['spam', 'abuse', 'off-topic', 'outdated', 'duplicate', 'resolved'];
const KIND_MODEL: Partial<Record<Kind, 'comment' | 'review' | 'reviewComment'>> = { comment: 'comment', review: 'review', review_comment: 'reviewComment' };
const MODEL_KIND: Partial<Record<ModelName, Kind>> = { issue: 'issue', comment: 'comment', reviewComment: 'review_comment' };

function parseReason(raw: unknown): MinimizedReason | null {
  if (typeof raw !== 'string') return null;
  const r = raw.trim().toLowerCase().replace(/_/g, '-');
  return (REASONS as readonly string[]).includes(r) ? (r as MinimizedReason) : null;
}

export function installModerationMocks(server: MockServer): void {
  const st = () => S(server);

  // Record edits: wrap `put` (the mock's single write path for synced rows).
  const put = server.put.bind(server);
  server.put = <M extends ModelName>(model: M, row: ModelMap[M], opts?: { includeLazy?: boolean }) => {
    const kind = MODEL_KIND[model];
    if (kind) {
      const prev = server.db.tables[model].get(row.id) as { body?: string | null } | undefined;
      const next = (row as { body?: string | null }).body;
      if (prev && prev.body !== undefined && next !== undefined && (prev.body ?? '') !== (next ?? '')) {
        const now = server.now();
        st().edits.push({ id: st().nextId++, kind, targetId: row.id, editorId: server.db.viewerId, body: next ?? '', previousBody: prev.body ?? '', editedAt: now, deletedAt: null, deletedById: null });
        if (model === 'issue') (row as Issue).bodyEditedAt = now;
      }
    }
    put(model, row, opts);
  };

  const repoOf = (ctx: Ctx) => server.repo(param(ctx, 1), param(ctx, 2));
  const canTriage = (repoId: ID) => {
    const p = server.db.tables.viewerRepo.get(repoId)?.permission;
    return p === 'triage' || p === 'write' || p === 'maintain' || p === 'admin';
  };
  const isAdmin = (repoId: ID) => server.db.tables.viewerRepo.get(repoId)?.permission === 'admin';
  const target = (kind: Kind, id: number): { repoId: ID; authorId: ID } | undefined => {
    switch (kind) {
      case 'issue':
        return server.db.tables.issue.get(id);
      case 'comment':
        return server.db.tables.comment.get(id);
      case 'review':
        return server.db.tables.review.get(id);
      case 'review_comment':
        return server.db.tables.reviewComment.get(id);
      default:
        return undefined;
    }
  };

  const minimize = (ctx: Ctx, reason: MinimizedReason | null): Resp => {
    const repo = repoOf(ctx);
    const kind = param(ctx, 3) as Kind;
    const id = Number(param(ctx, 4));
    if (!repo || kind === 'issue') return notFound();
    if (!canTriage(repo.id)) return { status: 403, body: { message: 'Resource not accessible by integration' } };
    if (kind === 'commit_comment') {
      if (reason) st().commitHidden.set(id, reason);
      else st().commitHidden.delete(id);
      return ok({ id, minimizedReason: reason });
    }
    const model = KIND_MODEL[kind];
    const row = model ? server.db.tables[model].get(id) : undefined;
    if (!model || !row || row.repoId !== repo.id) return notFound();
    server.put(model, { ...row, minimizedReason: reason });
    return ok({ id, minimizedReason: reason });
  };

  server.route('PUT', '/_bgh/repos/:owner/:repo/minimized/:kind/:id', (ctx) => {
    const reason = parseReason(ctx.body.reason);
    if (!reason) return invalid('Validation Failed', 'reason', ctx.body.reason == null ? 'missing_field' : 'invalid', 'Comment');
    return minimize(ctx, reason);
  });
  server.route('DELETE', '/_bgh/repos/:owner/:repo/minimized/:kind/:id', (ctx) => minimize(ctx, null));
  server.route('GET', '/_bgh/repos/:owner/:repo/minimized/:kind', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound();
    const kind = param(ctx, 3) as Kind;
    const ids = (ctx.url.searchParams.get('ids') ?? '').split(',').map(Number).filter(Boolean);
    const out: { id: number; minimizedReason: string }[] = [];
    for (const id of ids) {
      const model = KIND_MODEL[kind];
      const reason = kind === 'commit_comment' ? st().commitHidden.get(id) : model ? server.db.tables[model].get(id)?.minimizedReason : undefined;
      if (reason) out.push({ id, minimizedReason: reason });
    }
    return ok(out);
  });

  const editJson = (e: Edit) => ({
    id: e.id,
    editor: simpleUser(server, e.editorId),
    body: e.body,
    previous_body: e.previousBody,
    edited_at: e.editedAt,
    deleted_at: e.deletedAt,
    deleted_by: e.deletedById != null ? simpleUser(server, e.deletedById) : null,
  });
  const editsOf = (kind: Kind, id: number) => st().edits.filter((e) => e.kind === kind && e.targetId === id);

  server.route('GET', '/_bgh/repos/:owner/:repo/edits/:kind/:id', (ctx) => {
    const repo = repoOf(ctx);
    const kind = param(ctx, 3) as Kind;
    const id = Number(param(ctx, 4));
    if (!repo || !['issue', 'comment', 'review', 'review_comment', 'commit_comment'].includes(kind)) return notFound();
    if (kind !== 'commit_comment' && target(kind, id)?.repoId !== repo.id) return notFound();
    return ok(
      editsOf(kind, id)
        .sort((a, b) => b.id - a.id)
        .map(editJson),
    );
  });
  server.route('DELETE', '/_bgh/repos/:owner/:repo/edits/:kind/:id/:editId', (ctx) => {
    const repo = repoOf(ctx);
    const kind = param(ctx, 3) as Kind;
    const id = Number(param(ctx, 4));
    const editId = Number(param(ctx, 5));
    if (!repo) return notFound();
    const t = target(kind, id);
    if (t && t.authorId !== server.db.viewerId && !isAdmin(repo.id)) return { status: 403, body: { message: 'Only the author or a repository admin can delete edit history.' } };
    const list = editsOf(kind, id).sort((a, b) => a.id - b.id);
    if (editId === 0) {
      if (!list[0]) return notFound();
      list[0].previousBody = null;
      return noContent();
    }
    const i = list.findIndex((e) => e.id === editId);
    if (i < 0) return notFound();
    if (i === list.length - 1) return invalid('Validation Failed', 'id', 'custom', 'UserContentEdit');
    const e = list[i]!;
    e.body = null;
    e.deletedAt ??= server.now();
    e.deletedById ??= server.db.viewerId;
    if (list[i + 1]) list[i + 1]!.previousBody = null;
    return noContent();
  });

  server.route('DELETE', '/_bgh/repos/:owner/:repo/issues/:number', (ctx) => {
    const repo = repoOf(ctx);
    if (!repo) return notFound();
    const number = Number(param(ctx, 3));
    if (st().deleted.has(`${repo.id}#${number}`)) return { status: 410, body: { message: 'This issue was deleted' } };
    const issue = server.issue(repo, number);
    if (!issue) return notFound();
    if (!isAdmin(repo.id)) return { status: 403, body: { message: 'Must have admin rights to Repository.' } };
    if (issue.isPr) return { status: 422, body: { message: 'Pull requests cannot be deleted.' } };
    for (const c of server.db.tables.comment.values()) if (c.issueId === issue.id) server.db.tables.comment.delete(c.id);
    server.remove('issue', issue.id);
    st().deleted.add(`${repo.id}#${number}`);
    if (issue.state === 'open') server.put('repo', { ...repo, openIssues: Math.max(0, repo.openIssues - 1) });
    return noContent();
  });
}

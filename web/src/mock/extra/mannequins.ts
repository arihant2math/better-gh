/**
 * Mannequin reclaim mock (P51, crates/bgh-import `reclaim.rs`): two seeded
 * mannequins, invitations by login, and the viewer's own invitations
 * (invite the viewer's login to try accepting).
 */
import type { Mannequin, Reclaim, SimpleAccount } from '../../api/mannequins';
import type { MockServer } from '../server';
import { invalid, noContent, notFound, ok, param, state } from './util';

interface Row {
  id: number;
  mannequinId: number;
  target: SimpleAccount;
  status: Reclaim['status'];
  createdAt: string;
  completedAt: string | null;
}

export function installMannequinMocks(server: MockServer): void {
  const now = () => new Date().toISOString();
  const st = () =>
    state(server, 'mannequins', () => ({
      mannequins: [
        { id: 990001, login: 'monalisa-imported', source: 'github.com', source_login: 'monalisa', created: '2024-03-01T10:00:00Z', reclaimedBy: null as SimpleAccount | null },
        { id: 990002, login: 'mona-imported', source: 'gitlab.example', source_login: 'mona', created: '2024-03-02T10:00:00Z', reclaimedBy: null as SimpleAccount | null },
      ],
      reclaims: [] as Row[],
      next: 1,
    }));
  const R = server.route.bind(server);
  const account = (login: string, id: number): SimpleAccount => ({ id, login, avatar_url: '', html_url: `/${login}` });
  const pendingOf = (id: number) => st().reclaims.find((r) => r.mannequinId === id && r.status === 'pending');
  const viewMannequin = (m: ReturnType<typeof st>['mannequins'][number]): Mannequin => {
    const p = pendingOf(m.id);
    return {
      id: m.id,
      login: m.login,
      source: m.source,
      source_login: m.source_login,
      avatar_url: '',
      html_url: `/${m.login}`,
      reclaimed_by: m.reclaimedBy,
      pending_reclaim: p ? { id: p.id, target: p.target, created_at: p.createdAt } : null,
      created_at: m.created,
    };
  };
  const viewReclaim = (r: Row): Reclaim => {
    const m = st().mannequins.find((x) => x.id === r.mannequinId)!;
    return {
      id: r.id,
      status: r.status,
      mannequin: viewMannequin(m),
      target: r.target,
      invited_by: account(server.viewer.login, server.viewer.id),
      organization: null,
      moved: r.status === 'accepted' ? { 'issues.author_id': 3, 'comments.author_id': 5, 'pr_reviews.user_id': 2 } : {},
      created_at: r.createdAt,
      updated_at: r.completedAt ?? r.createdAt,
      completed_at: r.completedAt,
    };
  };
  const list = () => ok(st().mannequins.map(viewMannequin));

  R('GET', '/_bgh/orgs/:org/mannequins', list);
  R('GET', '/_bgh/admin/mannequins', list);
  R('POST', '/_bgh/mannequins/:id/reclaims', (ctx) => {
    const m = st().mannequins.find((x) => x.id === Number(param(ctx, 1)));
    if (!m) return notFound();
    if (m.reclaimedBy) return invalid('This mannequin was already reclaimed');
    const login = String((ctx.body as { login?: string }).login ?? '').trim();
    const user = [...server.db.tables.user.values()].find((u) => u.login.toLowerCase() === login.toLowerCase() && u.type === 'User');
    if (!user) return invalid('Validation Failed', 'login', 'custom', 'MannequinReclaim');
    if (pendingOf(m.id)) return invalid('A reclaim of this mannequin is already pending');
    const row: Row = { id: st().next++, mannequinId: m.id, target: account(user.login, user.id), status: 'pending', createdAt: now(), completedAt: null };
    st().reclaims.push(row);
    return ok(viewReclaim(row), 201);
  });
  R('DELETE', '/_bgh/mannequin-reclaims/:id', (ctx) => {
    const r = st().reclaims.find((x) => x.id === Number(param(ctx, 1)));
    if (!r) return notFound();
    if (r.status !== 'pending') return invalid('Only a pending reclaim can be cancelled');
    r.status = 'cancelled';
    r.completedAt = now();
    return noContent();
  });
  R('GET', '/_bgh/user/mannequin-reclaims', () =>
    ok(
      st()
        .reclaims.filter((r) => r.target.id === server.viewer.id)
        .sort((a, b) => Number(a.status !== 'pending') - Number(b.status !== 'pending') || b.id - a.id)
        .map(viewReclaim),
    ),
  );
  const answer = (status: 'accepted' | 'declined') =>
    R('POST', `/_bgh/user/mannequin-reclaims/:id/${status === 'accepted' ? 'accept' : 'decline'}`, (ctx) => {
      const r = st().reclaims.find((x) => x.id === Number(param(ctx, 1)) && x.target.id === server.viewer.id);
      if (!r) return notFound();
      if (r.status !== 'pending') return invalid('This reclaim is not pending');
      r.status = status;
      r.completedAt = now();
      if (status === 'accepted') st().mannequins.find((m) => m.id === r.mannequinId)!.reclaimedBy = r.target;
      return ok(viewReclaim(r));
    });
  answer('accepted');
  answer('declined');
}

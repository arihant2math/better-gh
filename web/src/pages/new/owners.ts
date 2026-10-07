import { session } from '../../app/session';
import { store } from '../../sync';

export interface OwnerOption {
  login: string;
  avatarUrl: string;
  name: string | null;
  isOrg: boolean;
  role?: 'admin' | 'member';
}

/** Owners the viewer can create repositories for: themself + their organizations. */
export function useOwners(): OwnerOption[] {
  const me = session.user;
  if (!me) return [];
  const s = store();
  const viewerRow = s.get('user', me.id);
  const orgs = s
    .byIndex('membership', 'userId', me.id)
    .map((m): OwnerOption | null => {
      const o = s.get('org', m.orgId);
      return o ? { login: o.login, avatarUrl: o.avatarUrl, name: o.name, isOrg: true, role: m.role } : null;
    })
    .filter((o): o is OwnerOption => !!o)
    .sort((a, b) => a.login.localeCompare(b.login));
  return [{ login: me.login, avatarUrl: viewerRow?.avatarUrl ?? me.avatarUrl ?? '', name: me.name ?? null, isOrg: false }, ...orgs];
}

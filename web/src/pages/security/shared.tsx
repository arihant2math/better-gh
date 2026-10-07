/**
 * Pieces shared by the repository Security pages: the left navigation (P66
 * adds code scanning / Dependabot entries to `SECURITY_NAV`), state badges,
 * user links and the settings resource.
 */
import type { ReactNode } from 'react';
import { useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { getSettings, ssKeys } from '../../api/secretScanning';
import type { RestUser } from '../../api/types';
import { Link, useLocation } from '../../router';
import { store } from '../../sync';
import { repoByName } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { cx } from '../../ui/Button';
import { CheckCircleIcon, KeyAsteriskIcon, ShieldIcon, ShieldLockIcon, type Icon } from '../../ui/icons';
import styles from './Security.module.css';

export interface SecurityNavItem {
  id: string;
  label: string;
  icon: Icon;
  /** Path below `/{owner}/{repo}/security`. */
  path: string;
  group?: string;
}

/** Left navigation of the Security tab (extend for code scanning, advisories, …). */
export const SECURITY_NAV: SecurityNavItem[] = [
  { id: 'overview', label: 'Overview', icon: ShieldIcon, path: '' },
  { id: 'secret-scanning', label: 'Secret scanning', icon: KeyAsteriskIcon, path: '/secret-scanning', group: 'Vulnerability alerts' },
];

/** Page frame: security nav on the left, page content on the right. */
export function SecurityFrame({ owner, repo, children }: { owner: string; repo: string; children: ReactNode }) {
  const { pathname } = useLocation();
  const base = `/${owner}/${repo}/security`;
  const rest = pathname.slice(base.length);
  const current = [...SECURITY_NAV].reverse().find((i) => (i.path ? rest === i.path || rest.startsWith(`${i.path}/`) : rest === '' || rest === '/'));
  return (
    <div className={styles.page}>
      <nav className={styles.nav} aria-label="Security">
        {SECURITY_NAV.map((i) => (
          <span key={i.id} style={{ display: 'contents' }}>
            {i.group && <div className={styles.navHeading}>{i.group}</div>}
            <Link to={`${base}${i.path}`} className={styles.navItem} aria-current={current === i ? 'page' : undefined}>
              <i.icon size={16} />
              {i.label}
            </Link>
          </span>
        ))}
      </nav>
      <div className={styles.main}>{children}</div>
    </div>
  );
}

/** Whether the viewer administers `owner/repo` (synced `viewerRepo`). */
export function useCanAdmin(owner: string, repo: string): boolean {
  const r = repoByName(owner, repo);
  return !!r && store().get('viewerRepo', r.id)?.permission === 'admin';
}

/** Effective secret scanning settings (`/_bgh`). */
export function useSecretSettings(owner: string, repo: string) {
  return useResource(ssKeys.settings(owner, repo), () => getSettings(owner, repo), { ttlMs: 30_000 });
}

/** The API's answer when secret scanning is off for the repository. */
export const isDisabledError = (e: unknown) => e instanceof ApiError && e.status === 404 && /disabled/i.test(e.message);

export function AlertStateBadge({ state, resolution }: { state: 'open' | 'resolved'; resolution?: string | null }) {
  return state === 'open' ? (
    <span className={cx(styles.badge, styles.badgeOpen)}>
      <ShieldIcon size={14} /> Open
    </span>
  ) : (
    <span className={cx(styles.badge, styles.badgeClosed)} title={resolution ?? undefined}>
      <CheckCircleIcon size={14} /> Closed
    </span>
  );
}

export function BypassBadge() {
  return (
    <span className={cx(styles.badge, styles.badgeWarning)}>
      <ShieldLockIcon size={12} /> Bypassed
    </span>
  );
}

export function UserLink({ user }: { user: RestUser | null | undefined }) {
  if (!user) return <span>someone</span>;
  return (
    <Link to={`/${user.login}`} style={{ display: 'inline-flex', alignItems: 'center', gap: 4, fontWeight: 600, color: 'var(--fg)' }}>
      <Avatar user={{ login: user.login, avatarUrl: user.avatar_url }} size={16} />@{user.login}
    </Link>
  );
}

/** Code browser link to a location at its commit. */
export const blobHref = (owner: string, repo: string, l: { commit_sha: string; path: string; start_line: number }) =>
  `/${owner}/${repo}/blob/${l.commit_sha}/${l.path.split('/').map(encodeURIComponent).join('/')}#L${l.start_line}`;

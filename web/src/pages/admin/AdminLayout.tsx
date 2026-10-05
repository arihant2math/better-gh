import { observer } from 'mobx-react-lite';
import type { ReactNode } from 'react';
import { site } from '../../app/site';
import styles from '../../components/admin/admin.module.css';
import { Link, navigate, useLocation } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { EmptyState } from '../../ui/EmptyState';
import {
  DownloadIcon,
  GearIcon,
  GraphIcon,
  LogIcon,
  OrganizationIcon,
  PersonIcon,
  RepoIcon,
  ServerIcon,
  ShieldIcon,
  StackIcon,
  SyncIcon,
  ToolsIcon,
  WebhookIcon,
  type Icon,
} from '../../ui/icons';
import { settingsDirty } from './settingsState';

const NAV: { to: string; label: string; icon: Icon; keys: string; group?: string }[] = [
  { to: '/site-admin', label: 'Dashboard', icon: GraphIcon, keys: 'g d' },
  { to: '/site-admin/users', label: 'Users', icon: PersonIcon, keys: 'g u', group: 'Accounts' },
  { to: '/site-admin/orgs', label: 'Organizations', icon: OrganizationIcon, keys: 'g o' },
  { to: '/site-admin/repos', label: 'Repositories', icon: RepoIcon, keys: 'g r' },
  { to: '/site-admin/mirrors', label: 'Mirrors', icon: SyncIcon, keys: 'g y' },
  { to: '/site-admin/imports', label: 'Imports', icon: DownloadIcon, keys: 'g p' },
  { to: '/site-admin/settings', label: 'Site settings', icon: GearIcon, keys: 'g e', group: 'Instance' },
  { to: '/site-admin/audit-log', label: 'Audit log', icon: LogIcon, keys: 'g a' },
  { to: '/site-admin/jobs', label: 'Background jobs', icon: StackIcon, keys: 'g j' },
  { to: '/site-admin/maintenance', label: 'Git maintenance', icon: ToolsIcon, keys: 'g m' },
  { to: '/site-admin/hooks', label: 'Global webhooks', icon: WebhookIcon, keys: 'g w' },
];

function current(pathname: string): string {
  const best = NAV.filter((n) => pathname === n.to || pathname.startsWith(`${n.to}/`)).sort((a, b) => b.to.length - a.to.length)[0];
  return best?.to ?? '/site-admin';
}

/** Persistent layout of `/site-admin/*`: section navigation + access guard. */
export default observer(function AdminLayout({ children }: { children: ReactNode }) {
  const { pathname } = useLocation();
  const active = current(pathname);
  useShortcuts(
    'Site admin',
    Object.fromEntries(NAV.map((n) => [n.keys, { handler: () => navigate(n.to), description: `Site admin: ${n.label}`, group: 'Site admin' }])),
  );
  if (site.viewerSiteAdmin === false) {
    return (
      <EmptyState icon={ShieldIcon} title="Site administrators only">
        You need to be a site administrator of this instance to open site admin.
      </EmptyState>
    );
  }
  return (
    <div className={styles.layout}>
      <nav className={styles.subnav} aria-label="Site admin">
        <div className={styles.subnavTitle}>
          <ServerIcon size={16} /> Site admin
        </div>
        {NAV.map((n) => (
          <span key={n.to} style={{ display: 'contents' }}>
            {n.group && <div className={styles.subnavGroup}>{n.group}</div>}
            <Link to={n.to} className={styles.subnavItem} aria-current={active === n.to ? 'page' : undefined}>
              <n.icon size={16} />
              {n.label}
              {n.to === '/site-admin/settings' && settingsDirty.dirty ? (
                <span className={styles.dirtyDot} aria-label="Unsaved changes" />
              ) : (
                <span className={styles.subnavKey} aria-hidden>
                  {n.keys.replace(' ', '')}
                </span>
              )}
            </Link>
          </span>
        ))}
      </nav>
      <div className={styles.main}>{children}</div>
    </div>
  );
});

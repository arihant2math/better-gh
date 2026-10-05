import { observer } from 'mobx-react-lite';
import type { ReactNode } from 'react';
import { session } from '../../app/session';
import { Link, useLocation } from '../../router';
import { Avatar } from '../../ui/Badge';
import {
  AppsIcon,
  BellIcon,
  BlockedIcon,
  CodeIcon,
  DeviceDesktopIcon,
  GearIcon,
  KeyAsteriskIcon,
  KeyIcon,
  MailIcon,
  PaintbrushIcon,
  PersonIcon,
  PlugIcon,
  ShieldLockIcon,
  SyncIcon,
  type Icon,
} from '../../ui/icons';
import styles from './SettingsLayout.module.css';

export interface SettingsNavItem {
  id: string;
  label: string;
  icon: Icon;
}

export const SETTINGS_NAV: { group?: string; items: SettingsNavItem[] }[] = [
  {
    items: [
      { id: 'profile', label: 'Public profile', icon: PersonIcon },
      { id: 'account', label: 'Account', icon: GearIcon },
      { id: 'appearance', label: 'Appearance', icon: PaintbrushIcon },
      { id: 'notifications', label: 'Notifications', icon: BellIcon },
    ],
  },
  {
    group: 'Access',
    items: [
      { id: 'emails', label: 'Emails', icon: MailIcon },
      { id: 'security', label: 'Password and authentication', icon: ShieldLockIcon },
      { id: 'sessions', label: 'Sessions', icon: DeviceDesktopIcon },
      { id: 'keys', label: 'SSH and GPG keys', icon: KeyIcon },
      { id: 'blocked', label: 'Blocked users', icon: BlockedIcon },
    ],
  },
  {
    group: 'Integrations',
    items: [
      { id: 'applications', label: 'Applications', icon: AppsIcon },
      { id: 'installations', label: 'Installed GitHub Apps', icon: PlugIcon },
    ],
  },
  {
    group: 'Developer settings',
    items: [
      { id: 'apps', label: 'GitHub Apps', icon: AppsIcon },
      { id: 'developers', label: 'OAuth apps', icon: CodeIcon },
      { id: 'tokens', label: 'Personal access tokens', icon: KeyAsteriskIcon },
    ],
  },
  {
    group: 'This device',
    items: [{ id: 'local', label: 'Local data & sync', icon: SyncIcon }],
  },
];

/** Persistent layout for `/settings/*`: nav on the left, section page on the right. */
export default observer(function SettingsLayout({ children }: { children: ReactNode }) {
  const { pathname } = useLocation();
  const current = pathname.split('/')[2] || 'profile';
  const user = session.user;
  return (
    <div className={styles.page}>
      <nav className={styles.nav} aria-label="Settings">
        {user && (
          <Link to={`/${user.login}`} className={styles.who}>
            <Avatar user={user} size={32} />
            <span>
              <strong>{user.name ?? user.login}</strong>
              <span className={styles.whoSub}>Your personal account</span>
            </span>
          </Link>
        )}
        {SETTINGS_NAV.map((g, i) => (
          <div key={g.group ?? i} className={styles.navGroup}>
            {g.group && <div className={styles.navHeading}>{g.group}</div>}
            {g.items.map((s) => (
              <Link key={s.id} to={`/settings/${s.id}`} className={styles.navItem} aria-current={s.id === current ? 'page' : undefined}>
                <s.icon size={16} />
                {s.label}
              </Link>
            ))}
          </div>
        ))}
      </nav>
      <div className={styles.content}>{children}</div>
    </div>
  );
});

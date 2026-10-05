import { observer } from 'mobx-react-lite';
import type { ReactNode } from 'react';
import styles from '../../components/admin/admin.module.css';
import { Link, navigate, useLocation, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { orgByLogin } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { AppsIcon, CodeIcon, DownloadIcon, LogIcon, MailIcon, OrganizationIcon, PeopleIcon, PersonIcon, PersonAddIcon, WebhookIcon, type Icon } from '../../ui/icons';

export const ORG_SECTIONS: { id: string; label: string; icon: Icon; keys: string; group?: string }[] = [
  { id: 'profile', label: 'General', icon: OrganizationIcon, keys: 'g g' },
  { id: 'members', label: 'Members', icon: PersonIcon, keys: 'g m', group: 'Access' },
  { id: 'teams', label: 'Teams', icon: PeopleIcon, keys: 'g t' },
  { id: 'outside-collaborators', label: 'Outside collaborators', icon: PersonAddIcon, keys: 'g c' },
  { id: 'invitations', label: 'Invitations', icon: MailIcon, keys: 'g v' },
  { id: 'audit-log', label: 'Audit log', icon: LogIcon, keys: 'g a', group: 'Archive' },
  { id: 'hooks', label: 'Webhooks', icon: WebhookIcon, keys: 'g w', group: 'Code, planning, and automation' },
  { id: 'import', label: 'Import', icon: DownloadIcon, keys: 'g p' },
  { id: 'installations', label: 'GitHub Apps', icon: AppsIcon, keys: 'g i', group: 'Third-party Access' },
  { id: 'apps', label: 'Developer settings', icon: CodeIcon, keys: 'g d' },
];

export const orgSettingsPath = (org: string, section = 'profile') => `/organizations/${encodeURIComponent(org)}/settings/${section}`;

/** Persistent layout of `/organizations/:org/settings/*`. */
export default observer(function OrgSettingsLayout({ children }: { children: ReactNode }) {
  const { org = '' } = useParams<{ org: string }>();
  const { pathname } = useLocation();
  const section = pathname.split('/')[4] ?? 'profile';
  const local = orgByLogin(org);
  useShortcuts(
    'Organization settings',
    Object.fromEntries(ORG_SECTIONS.map((s) => [s.keys, { handler: () => navigate(orgSettingsPath(org, s.id)), description: `Org settings: ${s.label}`, group: 'Organization' }])),
  );
  return (
    <div className={styles.layout}>
      <nav className={styles.subnav} aria-label="Organization settings">
        <Link to={`/${org}`} className={styles.subnavTitle}>
          <Avatar user={local ?? { login: org, avatarUrl: '' }} size={20} square />
          {org}
        </Link>
        {ORG_SECTIONS.map((s) => (
          <span key={s.id} style={{ display: 'contents' }}>
            {s.group && <div className={styles.subnavGroup}>{s.group}</div>}
            <Link to={orgSettingsPath(org, s.id)} className={styles.subnavItem} aria-current={section === s.id ? 'page' : undefined}>
              <s.icon size={16} />
              {s.label}
              <span className={styles.subnavKey} aria-hidden>
                {s.keys.replace(' ', '')}
              </span>
            </Link>
          </span>
        ))}
      </nav>
      <div className={styles.main}>{children}</div>
    </div>
  );
});

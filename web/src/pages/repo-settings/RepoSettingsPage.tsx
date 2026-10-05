import { observer } from 'mobx-react-lite';
import { lazy, Suspense, type ComponentType, type LazyExoticComponent } from 'react';
import { Banner } from '../../components/settings/kit';
import { Link, useLocation, useParams } from '../../router';
import { store } from '../../sync';
import { repoByName } from '../../sync/selectors';
import { EmptyState } from '../../ui/EmptyState';
import { ArchiveIcon, GearIcon, GitBranchIcon, KeyIcon, LinkIcon, LockIcon, PeopleIcon, WebhookIcon, type Icon } from '../../ui/icons';
import styles from './RepoSettings.module.css';
import { ListSkeleton, type SectionProps } from './shared';

interface NavItem {
  id: string;
  label: string;
  icon: Icon;
  /** Extra path prefixes that select this item. */
  aliases?: string[];
  load: () => Promise<{ default: ComponentType<SectionProps> }>;
  component: LazyExoticComponent<ComponentType<SectionProps>>;
}

function section(id: string, label: string, icon: Icon, load: NavItem['load'], aliases?: string[]): NavItem {
  return { id, label, icon, load, aliases, component: lazy(load) };
}

const NAV: { group?: string; items: NavItem[] }[] = [
  { items: [section('', 'General', GearIcon, () => import('./sections/GeneralSettings'))] },
  { group: 'Access', items: [section('access', 'Collaborators and teams', PeopleIcon, () => import('./sections/AccessSettings'))] },
  {
    group: 'Code and automation',
    items: [
      section('branches', 'Branches', GitBranchIcon, () => import('./sections/BranchesSettings'), ['branch_protection_rules']),
      section('hooks', 'Webhooks', WebhookIcon, () => import('./sections/WebhooksSettings')),
      section('key_links', 'Autolink references', LinkIcon, () => import('./sections/AutolinksSettings')),
    ],
  },
  { group: 'Security', items: [section('keys', 'Deploy keys', KeyIcon, () => import('./sections/DeployKeysSettings'))] },
];

const ALL = NAV.flatMap((g) => g.items);

/**
 * `/:owner/:repo/settings[/*]`: own left nav; each section is a lazy chunk.
 * Renders synchronously from the synced `repo` row; sections fetch the rest.
 */
export default observer(function RepoSettingsPage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const { pathname } = useLocation();
  const repo = repoByName(owner, name);
  if (!repo) return null; // RepoLayout shows loading / not found
  const permission = store().get('viewerRepo', repo.id)?.permission;
  const base = `/${repo.owner}/${repo.name}/settings`;
  const segs = pathname.split('/').slice(4).filter(Boolean).map(decodeURIComponent);
  const first = segs[0] ?? '';
  const item = ALL.find((i) => i.id === first || i.aliases?.includes(first));

  if (permission !== 'admin') {
    return (
      <div className={styles.center}>
        <EmptyState icon={LockIcon} title="You need admin access">
          Only repository administrators can change the settings of {repo.owner}/{repo.name}.
        </EmptyState>
      </div>
    );
  }

  const Section = item?.component;
  const rest = item?.id === first ? segs.slice(1) : segs;
  return (
    <div className={styles.page}>
      <nav className={styles.nav} aria-label="Repository settings">
        {NAV.map((g, i) => (
          <div key={g.group ?? i} className={styles.navGroup}>
            {g.group && <div className={styles.navHeading}>{g.group}</div>}
            {g.items.map((s) => (
              <Link
                key={s.id}
                to={s.id ? `${base}/${s.id}` : base}
                className={styles.navItem}
                aria-current={s === item ? 'page' : undefined}
                onMouseEnter={() => void s.load()}
                onFocus={() => void s.load()}
              >
                <s.icon size={16} />
                {s.label}
              </Link>
            ))}
          </div>
        ))}
      </nav>
      <div className={styles.content}>
        {repo.archived && (
          <div className={styles.archivedBanner}>
            <Banner tone="warning" icon={ArchiveIcon}>
              This repository has been archived by the owner. It is now read-only. You can unarchive it from the Danger Zone on the{' '}
              <Link to={base}>General</Link> page.
            </Banner>
          </div>
        )}
        {Section ? (
          <Suspense fallback={<ListSkeleton rows={4} />}>
            <Section key={item.id} repo={repo} rest={rest} base={base} />
          </Suspense>
        ) : (
          <EmptyState title="Settings page not found">
            <Link to={base}>Back to general settings</Link>
          </EmptyState>
        )}
      </div>
    </div>
  );
});

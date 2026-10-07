import { Link } from '../../../router';
import { ArrowLeftIcon, CodeIcon, LockIcon, ServerIcon, WorkflowIcon, type Icon } from '../../../ui/icons';
import styles from './Settings.module.css';

export interface NavItem {
  id: string;
  label: string;
  icon: Icon;
  to: string;
}

/** Side navigation of the Actions settings pages (repository and organization). */
export function ActionsNav({ items, current, back }: { items: NavItem[]; current: string; back: { to: string; label: string } }) {
  return (
    <nav className={styles.nav} aria-label="Actions settings">
      <Link to={back.to} className={styles.backLink}>
        <ArrowLeftIcon size={16} />
        {back.label}
      </Link>
      <div className={styles.navHeading}>Actions</div>
      {items.map((it) => (
        <Link key={it.id} to={it.to} className={styles.navItem} aria-current={it.id === current ? 'page' : undefined}>
          <it.icon size={16} />
          {it.label}
        </Link>
      ))}
    </nav>
  );
}

export const orgSettingsBase = (org: string) => `/organizations/${encodeURIComponent(org)}/settings`;

export function orgNavItems(org: string): NavItem[] {
  const base = orgSettingsBase(org);
  return [
    { id: 'secrets', label: 'Secrets', icon: LockIcon, to: `${base}/secrets/actions` },
    { id: 'variables', label: 'Variables', icon: CodeIcon, to: `${base}/variables/actions` },
    { id: 'runners', label: 'Runners', icon: ServerIcon, to: `${base}/actions/runners` },
    { id: 'runner-groups', label: 'Runner groups', icon: WorkflowIcon, to: `${base}/actions/runner-groups` },
  ];
}

/** Shared pieces of the OAuth consent and device activation screens. */
import type { ReactNode } from 'react';
import { redirectHost, type PublicApp } from '../../api/auth';
import { describeScope } from '../../api/scopes';
import type { BootUser } from '../../boot';
import { Avatar } from '../../ui/Badge';
import {
  BellIcon,
  CodeSquareIcon,
  EyeIcon,
  GlobeIcon,
  KeyAsteriskIcon,
  KeyIcon,
  LinkExternalIcon,
  LinkIcon,
  OrganizationIcon,
  PackageIcon,
  PersonIcon,
  PlayIcon,
  RepoIcon,
  ShieldCheckIcon,
  ShieldIcon,
  TableIcon,
  TrashIcon,
  WebhookIcon,
  type Icon,
} from '../../ui/icons';
import { authStyles as styles } from './AuthPage';

/** Heading and icon for a scope (github.com's consent screen groups). */
export function scopeTitle(id: string): { title: string; icon: Icon } {
  if (id === 'delete_repo') return { title: 'Delete repositories', icon: TrashIcon };
  if (id === 'security_events') return { title: 'Security events', icon: ShieldCheckIcon };
  if (id === 'repo' || id.startsWith('repo') || id === 'public_repo') return { title: 'Repositories', icon: RepoIcon };
  if (id.endsWith(':org_hook')) return { title: 'Organization hooks', icon: WebhookIcon };
  if (id.endsWith(':repo_hook')) return { title: 'Repository hooks', icon: WebhookIcon };
  if (id.endsWith(':org')) return { title: 'Organizations and teams', icon: OrganizationIcon };
  if (id === 'user' || id.startsWith('user:') || id === 'read:user') return { title: 'Personal user data', icon: PersonIcon };
  if (id.endsWith(':public_key')) return { title: 'Public SSH keys', icon: KeyIcon };
  if (id.endsWith(':gpg_key')) return { title: 'GPG keys', icon: KeyAsteriskIcon };
  if (id.endsWith(':packages')) return { title: 'Packages', icon: PackageIcon };
  if (id === 'gist') return { title: 'Gists', icon: CodeSquareIcon };
  if (id === 'notifications') return { title: 'Notifications', icon: BellIcon };
  if (id === 'workflow') return { title: 'Workflow', icon: PlayIcon };
  if (id === 'project' || id === 'read:project') return { title: 'Projects', icon: TableIcon };
  if (id === 'site_admin') return { title: 'Site administration', icon: ShieldIcon };
  return { title: id, icon: KeyIcon };
}

export function ScopeList({ scopes }: { scopes: string[] }) {
  if (scopes.length === 0) {
    return (
      <ul className={styles.scopes} aria-label="Requested permissions">
        <li className={styles.scope}>
          <span className={styles.scopeIcon}>
            <EyeIcon size={16} />
          </span>
          <div className={styles.scopeBody}>
            <span className={styles.scopeTitle}>Public data only</span>
            <span className={styles.scopeDesc}>Limited read-only access to your public profile and public repositories.</span>
          </div>
        </li>
      </ul>
    );
  }
  return (
    <ul className={styles.scopes} aria-label="Requested permissions">
      {scopes.map((s) => {
        const { title, icon: I } = scopeTitle(s);
        return (
          <li key={s} className={styles.scope}>
            <span className={styles.scopeIcon}>
              <I size={16} />
            </span>
            <div className={styles.scopeBody}>
              <span className={styles.scopeTitle}>{title}</span>
              <span className={styles.scopeDesc}>{describeScope(s)}</span>
              <span className={styles.scopeId}>{s}</span>
            </div>
          </li>
        );
      })}
    </ul>
  );
}

function AppIcon({ app }: { app: PublicApp }) {
  if (app.owner) return <Avatar user={{ login: app.owner.login, avatarUrl: app.owner.avatar_url, name: app.name }} size={56} square />;
  return (
    <span className={styles.appIcon} aria-hidden>
      {app.name.trim().charAt(0).toUpperCase() || '?'}
    </span>
  );
}

/** App icon ··· viewer avatar. */
export function AppHero({ app, user }: { app: PublicApp; user: BootUser | null }) {
  return (
    <div className={styles.heroPair} aria-hidden>
      <AppIcon app={app} />
      <span className={styles.heroLink}>
        <span />
        <span />
        <span />
      </span>
      <Avatar user={user} size={56} />
    </div>
  );
}

/** Facts about the app under the consent card. */
export function AppMeta({ app, redirectUri, extra }: { app: PublicApp; redirectUri?: string; extra?: ReactNode }) {
  const homepage = /^https?:\/\/[^/]/i.test(app.homepage_url) ? app.homepage_url : null;
  return (
    <ul className={styles.meta} style={{ margin: 0 }}>
      {homepage && (
        <li>
          <LinkExternalIcon size={14} />
          <a href={homepage} target="_blank" rel="noreferrer noopener">
            {redirectHost(homepage)}
          </a>
        </li>
      )}
      {app.description && (
        <li>
          <GlobeIcon size={14} />
          <span>{app.description}</span>
        </li>
      )}
      {redirectUri && (
        <li>
          <LinkIcon size={14} />
          <span>
            Authorizing will redirect to <strong className={styles.mono}>{redirectHost(redirectUri)}</strong>
          </span>
        </li>
      )}
      {extra && (
        <li>
          <KeyIcon size={14} />
          <span>{extra}</span>
        </li>
      )}
      {!app.owner && (
        <li>
          <ShieldCheckIcon size={14} />
          <span>Built-in application of this instance</span>
        </li>
      )}
    </ul>
  );
}

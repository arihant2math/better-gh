import { observer } from 'mobx-react-lite';
import { NotFound } from '../../app/NotFound';
import { Link, setQuery, useQuery, useParams } from '../../router';
import { store } from '../../sync';
import type { Repo } from '../../sync/models';
import { orgByLogin, reposForOwner, userByLogin } from '../../sync/selectors';
import { Avatar, Tag } from '../../ui/Badge';
import { EmptyState } from '../../ui/EmptyState';
import { LockIcon, OrganizationIcon, PeopleIcon, RepoIcon, StarIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import styles from './ProfilePage.module.css';

const LANG_COLORS: Record<string, string> = { Rust: '#dea584', TypeScript: '#3178c6', Go: '#00add8', Python: '#3572a5', Shell: '#89e051' };

const RepoCard = observer(function RepoCard({ repo }: { repo: Repo }) {
  return (
    <Link to={`/${repo.owner}/${repo.name}`} className={styles.repoCard}>
      <div className={styles.repoTitle}>
        {repo.private ? <LockIcon size={14} /> : <RepoIcon size={14} />}
        <span>{repo.name}</span>
        <Tag>{repo.private ? 'Private' : 'Public'}</Tag>
      </div>
      <p className={styles.repoDesc}>{repo.description ?? 'No description'}</p>
      <div className={styles.repoMeta}>
        {repo.language && (
          <span className={styles.lang}>
            <span className={styles.langDot} style={{ background: LANG_COLORS[repo.language] ?? 'var(--fg-subtle)' }} />
            {repo.language}
          </span>
        )}
        <span>
          <StarIcon size={14} /> {repo.stars}
        </span>
        {repo.pushedAt && (
          <span>
            Updated <RelativeTime date={repo.pushedAt} />
          </span>
        )}
      </div>
    </Link>
  );
});

/** User or organization profile (both from the local store). */
export default observer(function ProfilePage() {
  const { owner } = useParams<{ owner: string }>();
  const tab = useQuery().get('tab') ?? 'repositories';
  const org = orgByLogin(owner);
  const user = org ? undefined : userByLogin(owner);
  const s = store();
  if (!org && !user) return <NotFound what="account" />;

  const ownerId = (org ?? user)!.id;
  const repos = reposForOwner(ownerId);
  const members = org ? s.byIndex('membership', 'orgId', org.id) : [];
  const teams = org ? s.byIndex('team', 'orgId', org.id) : [];
  const userOrgs = user ? s.byIndex('membership', 'userId', user.id).map((m) => s.get('org', m.orgId)).filter((o) => o !== undefined) : [];

  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <Avatar user={org ?? user!} size={72} square={!!org} />
        <div>
          <h1 className={styles.name}>{(org ?? user)!.name ?? owner}</h1>
          <div className={styles.login}>
            {org ? <OrganizationIcon size={14} /> : null} {(org ?? user)!.login}
          </div>
          {org?.description && <p className={styles.bio}>{org.description}</p>}
          {userOrgs.length > 0 && (
            <div className={styles.orgs}>
              {userOrgs.map((o) => (
                <Link key={o.id} to={`/${o.login}`} title={o.login}>
                  <Avatar user={o} size={24} square />
                </Link>
              ))}
            </div>
          )}
        </div>
      </header>
      {org && (
        <div className={styles.tabs}>
          <Tabs
            value={tab}
            onChange={(t) => setQuery({ tab: t === 'repositories' ? null : t })}
            items={[
              { id: 'repositories', label: 'Repositories', icon: RepoIcon, count: repos.length },
              { id: 'people', label: 'People', icon: PeopleIcon, count: members.length },
              { id: 'teams', label: 'Teams', icon: PeopleIcon, count: teams.length },
            ]}
          />
        </div>
      )}
      {tab === 'people' && org ? (
        <div className={styles.people}>
          {members.map((m) => {
            const u = s.get('user', m.userId);
            return (
              <Link key={m.id} to={`/${u?.login}`} className={styles.person}>
                <Avatar user={u} size={36} />
                <span>
                  <strong>{u?.name ?? u?.login}</strong>
                  <span className={styles.personLogin}>
                    {u?.login} · {m.role}
                  </span>
                </span>
              </Link>
            );
          })}
        </div>
      ) : tab === 'teams' && org ? (
        <div className={styles.people}>
          {teams.map((t) => (
            <div key={t.id} className={styles.person}>
              <PeopleIcon size={20} />
              <span>
                <strong>{t.name}</strong>
                <span className={styles.personLogin}>{t.memberIds.length} members</span>
              </span>
            </div>
          ))}
        </div>
      ) : repos.length === 0 ? (
        <EmptyState icon={RepoIcon} title="No repositories" />
      ) : (
        <div className={styles.grid}>
          {repos.map((r) => (
            <RepoCard key={r.id} repo={r} />
          ))}
        </div>
      )}
    </div>
  );
});

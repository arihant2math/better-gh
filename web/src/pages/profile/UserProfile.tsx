import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { getAccount, listEvents, listFollowers, listFollowing, listUserOrgs, profileKeys, type RestAccount } from '../../api/profile';
import { session } from '../../app/session';
import { Link, navigate, useQuery } from '../../router';
import { store } from '../../sync';
import type { User } from '../../sync/models';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { BookIcon, LinkIcon, LocationIcon, MailIcon, MentionIcon, OrganizationIcon, PeopleIcon, RepoIcon, StarIcon, TableIcon } from '../../ui/icons';
import { TabNav } from '../../ui/Tabs';
import { Activity } from './Activity';
import { FollowButton, useFollowCheck, useFollowDelta, useMyFollowingDelta } from './follow';
import { useOwnerRepos, useStarred } from './profileData';
import styles from './ProfilePage.module.css';
import { RepoCard, RepoList } from './RepoList';
import { popular, STAR_SORTS } from './repoList';
import { personFromRest, UserList } from './UserList';

export type UserTab = 'overview' | 'repositories' | 'stars' | 'followers' | 'following';

export function blogHref(blog: string): string {
  return /^https?:\/\//i.test(blog) ? blog : `https://${blog}`;
}

/** User profile: sidebar (avatar, bio, follow, details, orgs) + tabs. */
export const UserProfile = observer(function UserProfile({ login, synced }: { login: string; synced: User | undefined }) {
  const tabParam = (useQuery().get('tab') ?? 'overview') as UserTab;
  const tab: UserTab = ['overview', 'repositories', 'stars', 'followers', 'following'].includes(tabParam) ? tabParam : 'overview';
  const viewer = session.user;
  const isViewer = !!viewer && viewer.login.toLowerCase() === login.toLowerCase();
  const account = useResource(profileKeys.account(login), () => getAccount(login));
  const a = account.data;
  const displayLogin = a?.login ?? synced?.login ?? login;
  const repos = useOwnerRepos(displayLogin, synced?.id ?? a?.id, isViewer ? 'mine' : 'user');
  const base = `/${displayLogin}`;

  const repoCount = repos.complete ? repos.items.length : (a?.public_repos ?? (repos.items.length || undefined));
  const starredCount = isViewer ? store().all('viewerRepo').filter((v) => v.starred).length : undefined;

  return (
    <div className={styles.page}>
      <div className={styles.userLayout}>
        <Sidebar login={displayLogin} synced={synced} account={a} isViewer={isViewer} loading={account.loading} />
        <div className={styles.main}>
          <TabNav
            className={styles.tabs}
            aria-label="Profile"
            current={tab}
            items={[
              { id: 'overview', label: 'Overview', icon: BookIcon, href: base },
              { id: 'repositories', label: 'Repositories', icon: RepoIcon, count: repoCount, href: `${base}?tab=repositories` },
              { id: 'stars', label: 'Stars', icon: StarIcon, count: starredCount, href: `${base}?tab=stars` },
              { id: 'projects', label: 'Projects', icon: TableIcon, href: `/users${base}/projects` },
            ]}
          />
          {tab === 'overview' && <Overview login={displayLogin} repos={repos} isViewer={isViewer} />}
          {tab === 'repositories' && (
            <RepoList
              label="Repositories"
              items={repos.items}
              loading={repos.loading}
              error={repos.error ? 'Could not load repositories.' : undefined}
              emptyTitle={isViewer ? 'You don’t have any public repositories yet' : `${displayLogin} doesn’t have any public repositories yet`}
              emptyBody={isViewer ? <Link to="/new">Create a new repository</Link> : undefined}
            />
          )}
          {tab === 'stars' && <StarsTab login={displayLogin} isViewer={isViewer} />}
          {tab === 'followers' && <PeopleTab login={displayLogin} which="followers" isViewer={isViewer} />}
          {tab === 'following' && <PeopleTab login={displayLogin} which="following" isViewer={isViewer} />}
        </div>
      </div>
    </div>
  );
});

const Sidebar = observer(function Sidebar({
  login,
  synced,
  account: a,
  isViewer,
  loading,
}: {
  login: string;
  synced: User | undefined;
  account: RestAccount | undefined;
  isViewer: boolean;
  loading: boolean;
}) {
  const signedIn = !!session.user;
  const follow = useFollowCheck(login, signedIn && !isViewer);
  const followerDelta = useFollowDelta(login);
  const myDelta = useMyFollowingDelta();
  const orgs = useResource(profileKeys.orgs(login), () => listUserOrgs(login));
  const name = a?.name ?? synced?.name ?? null;
  const avatarUser = { login, avatarUrl: a?.avatar_url ?? synced?.avatarUrl ?? '', name };
  // Orgs from the store for the viewer (instant), REST for everyone.
  const s = store();
  const storeOrgs = isViewer && synced ? s.byIndex('membership', 'userId', synced.id).map((m) => s.get('org', m.orgId)).filter((o) => !!o) : [];
  const orgList = orgs.data?.map((o) => ({ login: o.login, avatarUrl: o.avatar_url, name: null as string | null })) ?? storeOrgs.map((o) => ({ login: o.login, avatarUrl: o.avatarUrl, name: o.name }));
  const followers = a ? a.followers + followerDelta : undefined;
  const following = a ? a.following + (isViewer ? myDelta : 0) : undefined;

  return (
    <aside className={styles.sidebar} aria-label="Profile details">
      <div className={styles.sidebarTop}>
        <div className={styles.bigAvatar}>
          <Avatar user={avatarUser} size={296} />
        </div>
        <div className={styles.names}>
          {name && <h1 className={styles.fullName}>{name}</h1>}
          {name ? <div className={styles.loginLarge}>{login}</div> : <h1 className={styles.fullName}>{login}</h1>}
        </div>
      </div>
      {isViewer ? (
        <Button block onClick={() => navigate('/settings/profile')}>
          Edit profile
        </Button>
      ) : (
        <FollowButton state={follow} size="md" />
      )}
      {a?.bio ? <p className={styles.bio}>{a.bio}</p> : loading && !a ? <Skeleton width="80%" /> : null}
      <div className={styles.counts}>
        <PeopleIcon size={16} />
        <Link to={`/${login}?tab=followers`}>
          <strong>{followers ?? '–'}</strong> {followers === 1 ? 'follower' : 'followers'}
        </Link>
        <span aria-hidden>·</span>
        <Link to={`/${login}?tab=following`}>
          <strong>{following ?? '–'}</strong> following
        </Link>
      </div>
      {a && (a.company || a.location || a.email || a.blog || a.twitter_username) && (
        <ul className={styles.details}>
          {a.company && (
            <li>
              <OrganizationIcon size={16} />
              <span>{a.company}</span>
            </li>
          )}
          {a.location && (
            <li>
              <LocationIcon size={16} />
              <span>{a.location}</span>
            </li>
          )}
          {a.email && (
            <li>
              <MailIcon size={16} />
              <a href={`mailto:${a.email}`}>{a.email}</a>
            </li>
          )}
          {a.blog && (
            <li>
              <LinkIcon size={16} />
              <a href={blogHref(a.blog)} rel="nofollow noopener noreferrer" target="_blank">
                {a.blog}
              </a>
            </li>
          )}
          {a.twitter_username && (
            <li>
              <MentionIcon size={16} />
              <a href={`https://x.com/${encodeURIComponent(a.twitter_username)}`} rel="nofollow noopener noreferrer" target="_blank">
                @{a.twitter_username}
              </a>
            </li>
          )}
        </ul>
      )}
      {orgList.length > 0 && (
        <div className={styles.sideSection}>
          <h2 className={styles.sideTitle}>Organizations</h2>
          <div className={styles.avatarRow}>
            {orgList.map((o) => (
              <Link key={o.login} to={`/${o.login}`} aria-label={o.login} title={o.login}>
                <Avatar user={o} size={32} square />
              </Link>
            ))}
          </div>
        </div>
      )}
    </aside>
  );
});

function Overview({ login, repos, isViewer }: { login: string; repos: ReturnType<typeof useOwnerRepos>; isViewer: boolean }) {
  const top = popular(repos.items);
  return (
    <>
      <section className={styles.section} aria-labelledby="popular-h">
        <div className={styles.sectionHead}>
          <h2 id="popular-h" className={styles.sectionTitle}>
            Popular repositories
          </h2>
          {repos.items.length > 6 && (
            <Link to={`/${login}?tab=repositories`} className={styles.sectionLink}>
              View all {repos.items.length}
            </Link>
          )}
        </div>
        {top.length > 0 ? (
          <div className={styles.grid}>
            {top.map((r) => (
              <RepoCard key={r.id} r={r} />
            ))}
          </div>
        ) : repos.loading ? (
          <div className={styles.grid}>
            {[0, 1].map((i) => (
              <div key={i} className={styles.repoCard}>
                <Skeleton width="50%" height={18} />
                <Skeleton width="80%" />
              </div>
            ))}
          </div>
        ) : (
          <p className={styles.muted}>
            {isViewer ? (
              <>
                You don’t have any public repositories yet. <Link to="/new">Create one</Link>.
              </>
            ) : (
              `${login} doesn’t have any public repositories yet.`
            )}
          </p>
        )}
      </section>
      <ActivitySection login={login} />
    </>
  );
}

/** "Contribution activity": only rendered when the events endpoint answers. */
function ActivitySection({ login }: { login: string }) {
  const { data } = useResource(profileKeys.events(login), () => listEvents(login));
  if (!data) return null;
  return (
    <section className={styles.section} aria-labelledby="activity-h">
      <div className={styles.sectionHead}>
        <h2 id="activity-h" className={styles.sectionTitle}>
          Contribution activity
        </h2>
      </div>
      <Activity events={data} />
    </section>
  );
}

const StarsTab = observer(function StarsTab({ login, isViewer }: { login: string; isViewer: boolean }) {
  const starred = useStarred(login, isViewer);
  return (
    <RepoList
      label="Starred repositories"
      items={starred.items}
      loading={starred.loading}
      error={starred.error ? 'Could not load starred repositories.' : undefined}
      types={false}
      showOwner
      sorts={STAR_SORTS}
      defaultSort="starred"
      emptyTitle={isViewer ? 'You haven’t starred any repositories yet' : `${login} hasn’t starred any repositories yet`}
      emptyBody="Starring a repository makes it easy to find again."
    />
  );
});

function PeopleTab({ login, which, isViewer }: { login: string; which: 'followers' | 'following'; isViewer: boolean }) {
  const key = which === 'followers' ? profileKeys.followers(login) : profileKeys.following(login);
  const res = useResource(key, () => (which === 'followers' ? listFollowers(login) : listFollowing(login)));
  const who = isViewer ? 'You' : login;
  return (
    <section aria-label={which === 'followers' ? 'Followers' : 'Following'}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{which === 'followers' ? 'Followers' : 'Following'}</h2>
      </div>
      <UserList
        label={which === 'followers' ? 'Followers' : 'Following'}
        people={res.data?.map(personFromRest)}
        loading={res.loading}
        error={res.error ? 'Could not load this list.' : undefined}
        emptyTitle={which === 'followers' ? (isViewer ? 'You don’t have any followers yet' : `${login} doesn’t have any followers yet`) : `${who} ${isViewer ? 'aren’t' : 'isn’t'} following anybody yet`}
      />
    </section>
  );
}

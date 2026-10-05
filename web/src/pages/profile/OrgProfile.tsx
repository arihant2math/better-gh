import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { getOrg, listOrgMembers, listOrgTeams, profileKeys, type RestOrg } from '../../api/profile';
import { session } from '../../app/session';
import { site } from '../../app/site';
import { Pill } from '../../components/settings/kit';
import { Link, navigate, useQuery } from '../../router';
import { store } from '../../sync';
import type { Membership, Org, Team } from '../../sync/models';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Box, EmptyState, Skeleton } from '../../ui/EmptyState';
import { BookIcon, GearIcon, LinkIcon, LocationIcon, MailIcon, PeopleIcon, PlusIcon, RepoIcon, VerifiedIcon, TableIcon } from '../../ui/icons';
import { TabNav } from '../../ui/Tabs';
import { useOwnerRepos, type ReposState } from './profileData';
import styles from './ProfilePage.module.css';
import { RepoCard, RepoList } from './RepoList';
import { languageColor, languagesOf, popular } from './repoList';
import { blogHref } from './UserProfile';
import { personFromRest, UserList, type PersonItem } from './UserList';

export type OrgTab = 'overview' | 'repositories' | 'people' | 'teams';

/** Organization profile: header + Overview / Repositories / People / Teams. */
export const OrgProfile = observer(function OrgProfile({ login, synced }: { login: string; synced: Org | undefined }) {
  const tabParam = (useQuery().get('tab') ?? 'overview') as OrgTab;
  const res = useResource(profileKeys.org(login), () => getOrg(login));
  const o = res.data;
  const orgLogin = o?.login ?? synced?.login ?? login;
  const orgId = synced?.id ?? o?.id;
  const s = store();
  const viewerId = session.user?.id;
  const memberships: Membership[] = orgId ? s.byIndex('membership', 'orgId', orgId) : [];
  const mine = memberships.find((m) => m.userId === viewerId);
  const isMember = !!mine;
  const isOwner = mine?.role === 'admin';
  const tab: OrgTab = (['overview', 'repositories', 'people', 'teams'] as const).includes(tabParam) && (tabParam !== 'teams' || isMember) ? tabParam : 'overview';
  const repos = useOwnerRepos(orgLogin, orgId, 'org');
  const members = useResource(profileKeys.members(orgLogin), () => listOrgMembers(orgLogin));
  const storeTeams: Team[] = orgId ? s.byIndex('team', 'orgId', orgId) : [];
  const teams = useResource(isMember ? profileKeys.teams(orgLogin) : null, () => listOrgTeams(orgLogin));

  const roleOf = new Map(memberships.map((m) => [m.userId, m.role]));
  const people: PersonItem[] | undefined = members.data
    ? members.data.map((u) => ({ ...personFromRest(u), badge: roleOf.get(u.id) === 'admin' ? 'Owner' : undefined }))
    : memberships.length
      ? memberships
          .map((m) => s.get('user', m.userId))
          .filter((u) => !!u)
          .map((u) => ({ login: u.login, id: u.id, avatarUrl: u.avatarUrl, name: u.name, badge: roleOf.get(u.id) === 'admin' ? 'Owner' : undefined }))
          .sort((a, b) => a.login.localeCompare(b.login))
      : undefined;
  const memberCount = (t: { id: number }) => s.get('team', t.id)?.memberIds.length;
  const teamRows = teams.data
    ? teams.data.map((t) => ({ id: t.id, name: t.name, description: t.description, privacy: t.privacy, members: memberCount(t), parent: t.parent?.name }))
    : storeTeams
        .map((t) => ({ id: t.id, name: t.name, description: t.description, privacy: t.privacy, members: t.memberIds.length, parent: t.parentId ? s.get('team', t.parentId)?.name : undefined }))
        .sort((a, b) => a.name.localeCompare(b.name));

  const base = `/${orgLogin}`;
  const name = o?.name ?? synced?.name ?? null;
  const description = o?.description ?? synced?.description ?? null;

  return (
    <div className={styles.page}>
      <header className={styles.orgHeader}>
        <Avatar user={{ login: orgLogin, avatarUrl: o?.avatar_url ?? synced?.avatarUrl ?? '', name }} size={100} square />
        <div className={styles.orgHeaderText}>
          <h1 className={styles.orgTitle}>
            {name || orgLogin}
            {o?.is_verified && (
              <Pill tone="success">
                <VerifiedIcon size={12} /> Verified
              </Pill>
            )}
          </h1>
          {description ? <p className={styles.orgDesc}>{description}</p> : res.loading && !synced ? <Skeleton width="50%" /> : null}
          <OrgMeta o={o} />
        </div>
        <div className={styles.headerActions}>
          {isMember && (
            <Button size="sm" leadingIcon={PlusIcon} onClick={() => navigate(`/new?owner=${encodeURIComponent(orgLogin)}`)}>
              New repository
            </Button>
          )}
          {(isOwner || site.viewerSiteAdmin) && (
            <Button size="sm" leadingIcon={GearIcon} onClick={() => navigate(`/organizations/${encodeURIComponent(orgLogin)}/settings`)}>
              Settings
            </Button>
          )}
        </div>
      </header>
      <TabNav
        className={styles.tabs}
        aria-label="Organization"
        current={tab}
        items={[
          { id: 'overview', label: 'Overview', icon: BookIcon, href: base },
          { id: 'repositories', label: 'Repositories', icon: RepoIcon, count: repos.items.length || (o ? o.public_repos + (o.total_private_repos ?? 0) : undefined), href: `${base}?tab=repositories` },
              { id: 'projects', label: 'Projects', icon: TableIcon, href: `/orgs${base}/projects` },
          { id: 'people', label: 'People', icon: PeopleIcon, count: people?.length, href: `${base}?tab=people` },
          ...(isMember ? [{ id: 'teams', label: 'Teams', icon: PeopleIcon, count: teamRows.length, href: `${base}?tab=teams` }] : []),
        ]}
      />
      {tab === 'overview' && <OrgOverview login={orgLogin} repos={repos} people={people} />}
      {tab === 'repositories' && (
        <RepoList
          label="Repositories"
          items={repos.items}
          loading={repos.loading}
          error={repos.error ? 'Could not load repositories.' : undefined}
          emptyTitle="This organization has no repositories yet"
          emptyBody={isMember ? <Link to={`/new?owner=${encodeURIComponent(orgLogin)}`}>Create a new repository</Link> : undefined}
        />
      )}
      {tab === 'people' && (
        <section aria-label="People">
          <div className={styles.sectionHead}>
            <h2 className={styles.sectionTitle}>People</h2>
            {!isMember && <span className={`${styles.muted} ${styles.small}`}>Public members</span>}
          </div>
          <UserList
            label="Members"
            people={people}
            loading={members.loading}
            error={members.error ? 'Could not load members.' : undefined}
            emptyTitle={isMember ? 'No members' : 'This organization has no public members'}
            emptyBody={isMember ? undefined : 'Members can choose to make their membership public.'}
          />
        </section>
      )}
      {tab === 'teams' && isMember && (
        <section aria-label="Teams">
          <div className={styles.sectionHead}>
            <h2 className={styles.sectionTitle}>Teams</h2>
          </div>
          {teamRows.length === 0 && teams.loading ? (
            <Skeleton height={60} />
          ) : teamRows.length === 0 && teams.error ? (
            <Box padded>Could not load teams.</Box>
          ) : teamRows.length === 0 ? (
            <EmptyState icon={PeopleIcon} title="No teams yet">
              Teams group members and give them access to repositories.
            </EmptyState>
          ) : (
            <div className={styles.userRows} role="list" aria-label="Teams">
              {teamRows.map((t) => (
                <div key={t.id} className={styles.userRow} role="listitem">
                  <span className={styles.teamIcon}>
                    <PeopleIcon size={20} />
                  </span>
                  <div className={styles.userRowText}>
                    <div className={styles.userRowName}>
                      <strong>{t.name}</strong>
                      {t.privacy === 'secret' && <Pill>Secret</Pill>}
                      {t.parent && <span className={`${styles.muted} ${styles.small}`}>in {t.parent}</span>}
                    </div>
                    {t.description && <span className={`${styles.muted} ${styles.small}`}>{t.description}</span>}
                  </div>
                  {t.members !== undefined && (
                    <span className={`${styles.muted} ${styles.small}`}>
                      {t.members} {t.members === 1 ? 'member' : 'members'}
                    </span>
                  )}
                </div>
              ))}
            </div>
          )}
        </section>
      )}
    </div>
  );
});

function OrgMeta({ o }: { o: RestOrg | undefined }) {
  if (!o) return null;
  const items = [
    o.location && (
      <span key="loc">
        <LocationIcon size={16} />
        {o.location}
      </span>
    ),
    o.blog && (
      <a key="blog" href={blogHref(o.blog)} rel="nofollow noopener noreferrer" target="_blank">
        <LinkIcon size={16} />
        {o.blog}
      </a>
    ),
    o.email && (
      <a key="email" href={`mailto:${o.email}`}>
        <MailIcon size={16} />
        {o.email}
      </a>
    ),
    o.followers > 0 && (
      <span key="followers">
        <PeopleIcon size={16} />
        {o.followers} {o.followers === 1 ? 'follower' : 'followers'}
      </span>
    ),
  ].filter(Boolean);
  if (!items.length) return null;
  return <div className={styles.metaRow}>{items}</div>;
}

function OrgOverview({ login, repos, people }: { login: string; repos: ReposState; people: PersonItem[] | undefined }) {
  const top = popular(repos.items);
  const langs = languagesOf(repos.items).slice(0, 8);
  return (
    <div className={styles.orgLayout}>
      <div className={styles.main}>
        <section className={styles.section} aria-labelledby="org-popular-h">
          <div className={styles.sectionHead}>
            <h2 id="org-popular-h" className={styles.sectionTitle}>
              Popular repositories
            </h2>
            {repos.items.length > 0 && (
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
            <Skeleton height={80} />
          ) : (
            <p className={styles.muted}>This organization has no public repositories yet.</p>
          )}
        </section>
      </div>
      <aside className={styles.sidebar} aria-label="Organization summary">
        <div>
          <h2 className={styles.sideTitle}>
            <Link to={`/${login}?tab=people`}>People</Link>
          </h2>
          {people && people.length > 0 ? (
            <div className={styles.avatarRow}>
              {people.slice(0, 24).map((p) => (
                <Link key={p.id} to={`/${p.login}`} aria-label={p.login} title={p.login}>
                  <Avatar user={{ login: p.login, avatarUrl: p.avatarUrl, name: p.name }} size={32} />
                </Link>
              ))}
            </div>
          ) : people ? (
            <p className={`${styles.muted} ${styles.small}`}>No public members.</p>
          ) : (
            <Skeleton width="60%" />
          )}
        </div>
        {langs.length > 0 && (
          <div className={styles.sideSection}>
            <h2 className={styles.sideTitle}>Top languages</h2>
            <div className={styles.langList}>
              {langs.map((l) => (
                <span key={l}>
                  <span className={styles.langDot} style={{ background: languageColor(l) }} />
                  {l}
                </span>
              ))}
            </div>
          </div>
        )}
      </aside>
    </div>
  );
}

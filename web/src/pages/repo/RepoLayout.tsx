import { observer } from 'mobx-react-lite';
import { lazy, Suspense, useEffect, useState, type ReactNode } from 'react';
import { useResource } from '../../api/cache';
import { getRepository } from '../../api/endpoints';
import type { RestRepository } from '../../api/types';
import { NotFound } from '../../app/NotFound';
import { session } from '../../app/session';
import { ui } from '../../app/uiState';
import { Link, loginHref, navigate, useLocation, useParams, useScrollContainer } from '../../router';
import { hasSync, store, sync } from '../../sync';
import type { Repo } from '../../sync/models';
import { setStarred } from '../../sync/mutations';
import { canPush, repoByName } from '../../sync/selectors';
import { Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Spinner } from '../../ui/Spinner';
import { loadWatchSettings, watchSettingsKey } from '../notifications/actions';
import { reportRepoView } from '../../app/traffic';
import { canonicalRepoUrl, currentRepoTab, visibleRepoTabs, watchLabel, type RepoTabId } from './nav';
import { SyncFork } from './SyncFork';
import {
  CodeIcon,
  EyeIcon,
  GearIcon,
  GitPullRequestIcon,
  GraphIcon,
  IssueOpenedIcon,
  LockIcon,
  OrganizationIcon,
  PlayIcon,
  RepoForkedIcon,
  RepoIcon,
  RepoTemplateIcon,
  ShieldIcon,
  StarFillIcon,
  StarIcon,
  TableIcon,
  BookIcon,
} from '../../ui/icons';
import { TabNav } from '../../ui/Tabs';
import styles from './RepoLayout.module.css';
import { RouteRepoContext } from './useRouteRepo';

/** Header badge: "Public", "Internal" or "Private". */
function visibilityLabel(repo: { private: boolean; visibility?: string }): string {
  if (repo.visibility === 'internal') return 'Internal';
  return repo.private ? 'Private' : 'Public';
}

function compact(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k` : String(n);
}

const ForkDialog = lazy(() => import('./ForkDialog'));

/** REST repository (parent, template, `is_template`; not in the sync model), cached per repo. */
export const restRepoKey = (owner: string, name: string) => `repo-rest:${owner}/${name}`.toLowerCase();

/**
 * Loads a repo that isn't in the local store yet (e.g. a public repo outside
 * your scopes). When the URL names an old owner/name (rename or transfer),
 * the API resolves the redirect and we replace the URL with the canonical one.
 */
function useEnsureRepo(owner: string, name: string, present: boolean): 'ok' | 'loading' | 'missing' {
  const [state, setState] = useState<{ key: string; status: 'loading' | 'missing' }>({ key: '', status: 'loading' });
  const key = `${owner}/${name}`.toLowerCase();
  useEffect(() => {
    if (present || !hasSync()) return;
    let cancelled = false;
    getRepository(owner, name)
      .then((r) => {
        if (cancelled) return true;
        const to = canonicalRepoUrl(window.location, owner, name, r.full_name);
        if (to) {
          navigate(to, { replace: true });
          return true;
        }
        return sync().ensureScope(`repo:${r.id}`);
      })
      .then(
        (ok) => !cancelled && !ok && setState({ key, status: 'missing' }),
        () => !cancelled && setState({ key, status: 'missing' }),
      );
    return () => {
      cancelled = true;
    };
  }, [owner, name, present, key]);
  if (present) return 'ok';
  return state.key === key ? state.status : 'loading';
}

const TAB_DEFS: Record<RepoTabId, { label: string; icon: typeof CodeIcon; path: string }> = {
  code: { label: 'Code', icon: CodeIcon, path: '' },
  issues: { label: 'Issues', icon: IssueOpenedIcon, path: '/issues' },
  pulls: { label: 'Pull requests', icon: GitPullRequestIcon, path: '/pulls' },
  actions: { label: 'Actions', icon: PlayIcon, path: '/actions' },
  projects: { label: 'Projects', icon: TableIcon, path: '/projects' },
  wiki: { label: 'Wiki', icon: BookIcon, path: '/wiki' },
  security: { label: 'Security', icon: ShieldIcon, path: '/security' },
  pulse: { label: 'Insights', icon: GraphIcon, path: '/pulse' },
  settings: { label: 'Settings', icon: GearIcon, path: '/settings' },
};

export default observer(function RepoLayout({ children }: { children: ReactNode }) {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const { pathname } = useLocation();
  const repo = repoByName(owner, name);
  const status = useEnsureRepo(owner, name, !!repo);
  const [body, setBody] = useState<HTMLDivElement | null>(null);
  useScrollContainer(body);
  const known = !!repo;
  // Traffic (P31): one page view per repository URL.
  useEffect(() => {
    if (known) reportRepoView(owner, name, pathname);
  }, [known, owner, name, pathname]);

  if (!repo) {
    return status === 'missing' ? (
      <NotFound what="repository" />
    ) : (
      <div className={styles.loading}>
        <Spinner />
      </div>
    );
  }

  const viewer = store().get('viewerRepo', repo.id);
  const base = `/${repo.owner}/${repo.name}`;
  const section = pathname.slice(base.length).split('/')[1] ?? '';
  const current = currentRepoTab(section);
  const canAdmin = viewer?.permission === 'admin';
  const tabs = visibleRepoTabs(repo, canAdmin).map((id) => {
    const t = TAB_DEFS[id];
    return {
      id,
      label: t.label,
      icon: t.icon,
      href: `${base}${t.path}`,
      ...(id === 'issues' ? { count: compact(repo.openIssues) } : id === 'pulls' ? { count: compact(repo.openPulls) } : {}),
    };
  });

  return (
    <div className={styles.layout}>
      <header className={styles.header}>
        <RepoHeader repo={repo} base={base} />
        <TabNav items={tabs} current={current} aria-label="Repository" className={styles.tabs} />
      </header>
      <div ref={setBody} className={styles.body}>
        <RouteRepoContext value={repo}>{children}</RouteRepoContext>
      </div>
    </div>
  );
});

const RepoHeader = observer(function RepoHeader({ repo, base }: { repo: Repo; base: string }) {
  const viewer = store().get('viewerRepo', repo.id);
  const signedIn = !!session.user;
  const [forking, setForking] = useState(false);
  const rest = useResource<RestRepository>(restRepoKey(repo.owner, repo.name), () => getRepository(repo.owner, repo.name), { ttlMs: 60_000 }).data;
  const watchCustom = useResource(signedIn && viewer?.watching === 'subscribed' ? watchSettingsKey(repo.id) : null, () => loadWatchSettings(repo), { ttlMs: 60_000 }).data?.state === 'custom';
  const label = watchLabel(viewer?.watching, watchCustom);
  const parent = repo.fork ? rest?.parent : null;
  const template = rest?.template_repository;
  const forkable = rest?.allow_forking !== false || !repo.private;
  const requireLogin = () => navigate(loginHref());

  return (
    <>
      <div className={styles.titleRow}>
        {repo.visibility === 'internal' ? (
          <OrganizationIcon size={16} className={styles.repoIcon} />
        ) : repo.private ? (
          <LockIcon size={16} className={styles.repoIcon} />
        ) : rest?.is_template ? <RepoTemplateIcon size={16} className={styles.repoIcon} /> : repo.fork ? <RepoForkedIcon size={16} className={styles.repoIcon} /> : <RepoIcon size={16} className={styles.repoIcon} />}
        <h1 className={styles.title}>
          <Link to={`/${repo.owner}`} className={styles.owner}>
            {repo.owner}
          </Link>
          <span className={styles.slash}>/</span>
          <Link to={base} className={styles.name}>
            {repo.name}
          </Link>
        </h1>
        <Tag>{rest?.is_template ? `${visibilityLabel(repo)} template` : visibilityLabel(repo)}</Tag>
        {repo.archived && <Tag>Archived</Tag>}
        {repo.mirrorUrl && <Tag>Mirror</Tag>}
        <div className={styles.actions}>
          {rest?.is_template && signedIn && (
            <Button size="sm" variant="success" leadingIcon={RepoTemplateIcon} onClick={() => navigate(`/new?template_owner=${encodeURIComponent(repo.owner)}&template_name=${encodeURIComponent(repo.name)}`)}>
              Use this template
            </Button>
          )}
          {parent && <SyncFork owner={repo.owner} name={repo.name} branch={repo.defaultBranch} upstream={{ owner: parent.owner.login, name: parent.name }} canPush={canPush(repo.id) && !repo.archived} />}
          <span className={styles.split}>
            <Button size="sm" leadingIcon={EyeIcon} onClick={() => (signedIn ? ui.openWatch(repo.id) : requireLogin())} aria-haspopup="dialog" title="Notification settings for this repository">
              {label}
            </Button>
            <Link to={`${base}/watchers`} className={styles.countLink} aria-label={`${repo.watchers} watching`}>
              {compact(repo.watchers)}
            </Link>
          </span>
          <span className={styles.split}>
            <Button size="sm" leadingIcon={RepoForkedIcon} disabled={!forkable} onClick={() => (signedIn ? setForking(true) : requireLogin())} aria-haspopup="dialog" title={forkable ? 'Fork your own copy of this repository' : 'Forking is disabled for this repository'}>
              Fork
            </Button>
            <Link to={`${base}/forks`} className={styles.countLink} aria-label={`${repo.forks} forks`}>
              {compact(repo.forks)}
            </Link>
          </span>
          <span className={styles.split}>
            <Button
              size="sm"
              leadingIcon={viewer?.starred ? StarFillIcon : StarIcon}
              className={viewer?.starred ? styles.starred : undefined}
              aria-pressed={!!viewer?.starred}
              onClick={() => (signedIn ? setStarred(repo, !viewer?.starred) : requireLogin())}
            >
              {viewer?.starred ? 'Starred' : 'Star'}
            </Button>
            <Link to={`${base}/stargazers`} className={styles.countLink} aria-label={`${repo.stars} stars`}>
              {compact(repo.stars)}
            </Link>
          </span>
        </div>
      </div>
      {(parent || template || repo.mirrorUrl) && (
        <div className={styles.subline}>
          {repo.mirrorUrl && (
            <span className={styles.mirrored}>
              mirrored from{' '}
              <a href={repo.mirrorUrl} rel="noreferrer noopener" target="_blank">
                {repo.mirrorUrl}
              </a>
            </span>
          )}
          {parent && (
            <span>
              forked from <Link to={`/${parent.full_name}`}>{parent.full_name}</Link>
            </span>
          )}
          {template && (
            <span>
              generated from <Link to={`/${template.full_name}`}>{template.full_name}</Link>
            </span>
          )}
        </div>
      )}
      {forking && (
        <Suspense fallback={null}>
          <ForkDialog repo={repo} onClose={() => setForking(false)} />
        </Suspense>
      )}
    </>
  );
});

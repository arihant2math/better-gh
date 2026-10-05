import { observer } from 'mobx-react-lite';
import { useEffect, useState, type ReactNode } from 'react';
import { getRepository } from '../../api/endpoints';
import { NotFound } from '../../app/NotFound';
import { Link, useLocation, useParams, useScrollContainer } from '../../router';
import { hasSync, store, sync } from '../../sync';
import { setStarred } from '../../sync/mutations';
import { repoByName } from '../../sync/selectors';
import { Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Spinner } from '../../ui/Spinner';
import {
  CodeIcon,
  EyeIcon,
  GearIcon,
  GitPullRequestIcon,
  GraphIcon,
  IssueOpenedIcon,
  LockIcon,
  PlayIcon,
  RepoForkedIcon,
  RepoIcon,
  ShieldIcon,
  StarFillIcon,
  StarIcon,
  TableIcon,
  BookIcon,
} from '../../ui/icons';
import { TabNav } from '../../ui/Tabs';
import styles from './RepoLayout.module.css';

function compact(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k` : String(n);
}

/** Loads a repo that isn't in the local store yet (e.g. a public repo outside your scopes). */
function useEnsureRepo(owner: string, name: string, present: boolean): 'ok' | 'loading' | 'missing' {
  const [state, setState] = useState<{ key: string; status: 'loading' | 'missing' }>({ key: '', status: 'loading' });
  const key = `${owner}/${name}`.toLowerCase();
  useEffect(() => {
    if (present || !hasSync()) return;
    let cancelled = false;
    getRepository(owner, name)
      .then((r) => sync().ensureScope(`repo:${r.id}`))
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

export default observer(function RepoLayout({ children }: { children: ReactNode }) {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const { pathname } = useLocation();
  const repo = repoByName(owner, name);
  const status = useEnsureRepo(owner, name, !!repo);
  const [body, setBody] = useState<HTMLDivElement | null>(null);
  useScrollContainer(body);

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
  const current =
    section === '' || section === 'tree' || section === 'blob'
      ? 'code'
      : section === 'pull'
        ? 'pulls'
        : section;
  const canAdmin = viewer?.permission === 'admin';

  const tabs = [
    { id: 'code', label: 'Code', icon: CodeIcon, href: base },
    { id: 'issues', label: 'Issues', icon: IssueOpenedIcon, href: `${base}/issues`, count: compact(repo.openIssues) },
    { id: 'pulls', label: 'Pull requests', icon: GitPullRequestIcon, href: `${base}/pulls`, count: compact(repo.openPulls) },
    { id: 'actions', label: 'Actions', icon: PlayIcon, href: `${base}/actions` },
    { id: 'projects', label: 'Projects', icon: TableIcon, href: `${base}/projects` },
    ...(repo.hasWiki ? [{ id: 'wiki', label: 'Wiki', icon: BookIcon, href: `${base}/wiki` }] : []),
    { id: 'security', label: 'Security', icon: ShieldIcon, href: `${base}/security` },
    { id: 'pulse', label: 'Insights', icon: GraphIcon, href: `${base}/pulse` },
    ...(canAdmin ? [{ id: 'settings', label: 'Settings', icon: GearIcon, href: `${base}/settings` }] : []),
  ];

  return (
    <div className={styles.layout}>
      <header className={styles.header}>
        <div className={styles.titleRow}>
          {repo.private ? <LockIcon size={16} className={styles.repoIcon} /> : <RepoIcon size={16} className={styles.repoIcon} />}
          <h1 className={styles.title}>
            <Link to={`/${repo.owner}`} className={styles.owner}>
              {repo.owner}
            </Link>
            <span className={styles.slash}>/</span>
            <Link to={base} className={styles.name}>
              {repo.name}
            </Link>
          </h1>
          <Tag>{repo.private ? 'Private' : 'Public'}</Tag>
          {repo.archived && <Tag>Archived</Tag>}
          <div className={styles.actions}>
            <Button size="sm" leadingIcon={EyeIcon}>
              {viewer?.watching === 'ignored' ? 'Ignoring' : 'Watch'} <span className={styles.count}>{compact(repo.watchers)}</span>
            </Button>
            <Button size="sm" leadingIcon={RepoForkedIcon}>
              Fork <span className={styles.count}>{compact(repo.forks)}</span>
            </Button>
            <Button
              size="sm"
              leadingIcon={viewer?.starred ? StarFillIcon : StarIcon}
              className={viewer?.starred ? styles.starred : undefined}
              aria-pressed={!!viewer?.starred}
              onClick={() => setStarred(repo, !viewer?.starred)}
            >
              {viewer?.starred ? 'Starred' : 'Star'} <span className={styles.count}>{compact(repo.stars)}</span>
            </Button>
          </div>
        </div>
        <TabNav items={tabs} current={current} aria-label="Repository" className={styles.tabs} />
      </header>
      <div ref={setBody} className={styles.body}>
        {children}
      </div>
    </div>
  );
});

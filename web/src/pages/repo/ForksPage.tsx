import { observer } from 'mobx-react-lite';
import { repoListPaths } from '../../api/endpoints';
import type { RestFork } from '../../api/types';
import { usePagedList } from '../../components/admin/usePagedList';
import { Link, setQuery, useQuery } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Box, EmptyState, Skeleton } from '../../ui/EmptyState';
import { IssueOpenedIcon, RepoForkedIcon, StarIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Select } from '../../ui/Input';
import styles from './RepoNav.module.css';
import { useRouteRepo } from './useRouteRepo';

const SORTS = [
  { id: 'newest', label: 'Newest' },
  { id: 'oldest', label: 'Oldest' },
  { id: 'stargazers', label: 'Most starred' },
  { id: 'watchers', label: 'Most watched' },
];

/** `/:owner/:repo/forks`: direct forks, paginated, sortable like GitHub's forks list. */
export default observer(function ForksPage() {
  const repo = useRouteRepo();
  const wanted = useQuery().get('sort');
  const sort = wanted && SORTS.some((s) => s.id === wanted) ? wanted : 'newest';
  const list = usePagedList<RestFork>(repoListPaths.forks(repo.owner, repo.name, sort));
  return (
    <div className={styles.page}>
      <h2 className={styles.pageTitle}>
        <RepoForkedIcon size={20} /> Forks
      </h2>
      <div className={styles.toolbar}>
        <label className={styles.muted} htmlFor="forks-sort">
          Sort
        </label>
        <Select id="forks-sort" value={sort} onChange={(e) => setQuery({ sort: e.target.value === 'newest' ? null : e.target.value })}>
          {SORTS.map((s) => (
            <option key={s.id} value={s.id}>
              {s.label}
            </option>
          ))}
        </Select>
      </div>
      {list.error && !list.items.length ? (
        <Box padded>{(list.error as Error).message}</Box>
      ) : !list.items.length && list.loading ? (
        <Box>
          {[0, 1, 2].map((i) => (
            <div key={i} className={styles.forkRow}>
              <Skeleton width={20} height={20} />
              <Skeleton width="40%" />
            </div>
          ))}
        </Box>
      ) : !list.items.length ? (
        <EmptyState icon={RepoForkedIcon} title="No one has forked this repository yet">
          Forks are a great way to contribute to a repository. After forking a repository, you can send the original author a pull request.
        </EmptyState>
      ) : (
        <Box>
          <div role="list" aria-label="Forks">
            {list.items.map((f) => (
              <div key={f.id} className={styles.forkRow} role="listitem">
                <Avatar user={{ login: f.owner.login, avatarUrl: f.owner.avatar_url }} size={20} square={f.owner.type === 'Organization'} />
                <div className={styles.forkMain}>
                  <Link to={`/${f.full_name}`} className={styles.forkName}>
                    {f.full_name}
                  </Link>
                  {f.description && <span className={styles.muted}>{f.description}</span>}
                  <span className={styles.forkMeta}>
                    <span>
                      <StarIcon size={14} /> {f.stargazers_count ?? 0}
                    </span>
                    <span>
                      <RepoForkedIcon size={14} /> {f.forks_count ?? 0}
                    </span>
                    <span>
                      <IssueOpenedIcon size={14} /> {f.open_issues_count ?? 0}
                    </span>
                    {(f.pushed_at ?? f.updated_at) && (
                      <span>
                        Updated <RelativeTime date={(f.pushed_at ?? f.updated_at)!} />
                      </span>
                    )}
                  </span>
                </div>
              </div>
            ))}
          </div>
        </Box>
      )}
      {list.next && (
        <div className={styles.more}>
          <Button onClick={() => void list.loadMore()} loading={list.loading}>
            Load more
          </Button>
        </div>
      )}
    </div>
  );
});

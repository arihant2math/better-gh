import { observer } from 'mobx-react-lite';
import { useMemo } from 'react';
import { repoListPaths } from '../../api/endpoints';
import { usePagedList } from '../../api/usePagedList';
import { useLocation } from '../../router';
import { Button } from '../../ui/Button';
import { EyeIcon, StarIcon } from '../../ui/icons';
import { personFromRest, UserList } from '../profile/UserList';
import styles from './RepoNav.module.css';
import type { SimpleUser } from '../../api/types';
import { useRouteRepo } from './useRouteRepo';

/** `/:owner/:repo/stargazers` and `/:owner/:repo/watchers` (paginated, `Link: rel="next"`). */
export default observer(function RepoPeoplePage() {
  const watchers = useLocation().pathname.endsWith('/watchers');
  const { owner: o, name: n } = useRouteRepo();
  const list = usePagedList<SimpleUser>(watchers ? repoListPaths.watchers(o, n) : repoListPaths.stargazers(o, n));
  const people = useMemo(() => (list.items.length || list.done ? list.items.map(personFromRest) : undefined), [list.items, list.done]);
  const Icon = watchers ? EyeIcon : StarIcon;
  return (
    <div className={styles.page}>
      <h2 className={styles.pageTitle}>
        <Icon size={20} /> {watchers ? 'Watchers' : 'Stargazers'}
      </h2>
      <UserList
        people={people}
        loading={list.loading}
        error={list.error ? (list.error as Error).message : undefined}
        emptyTitle={watchers ? 'No one’s watching this repository yet' : 'Be the first to star this repository'}
        emptyBody={watchers ? 'Watchers get notified about all activity in the repository.' : 'Stars show appreciation and help people find good projects.'}
        label={watchers ? 'Watchers' : 'Stargazers'}
      />
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

import { observer } from 'mobx-react-lite';
import { useCallback } from 'react';
import { navigate } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { issuesForRepo } from '../../sync/selectors';
import { Button } from '../../ui/Button';
import { GitPullRequestIcon } from '../../ui/icons';
import { IssueList } from '../issues/IssueList';
import { useRouteRepo } from '../repo/useRouteRepo';

export default observer(function PullListPage() {
  const repo = useRouteRepo();
  const repoId = repo.id;
  const source = useCallback(() => issuesForRepo(repoId).filter((i) => i.isPr), [repoId]);
  const compare = `/${repo.owner}/${repo.name}/compare`;
  useShortcuts('Pull requests', { c: { handler: () => navigate(compare), description: 'New pull request', group: 'Pull requests' } });
  return (
    <IssueList
      kind="pr"
      repo={repo}
      source={source}
      header={
        <div style={{ display: 'flex', justifyContent: 'flex-end', padding: '12px 16px 0' }}>
          <Button variant="success" size="sm" leadingIcon={GitPullRequestIcon} kbd="c" onClick={() => navigate(compare)}>
            New pull request
          </Button>
        </div>
      }
    />
  );
});

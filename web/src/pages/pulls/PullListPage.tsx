import { observer } from 'mobx-react-lite';
import { useCallback } from 'react';
import { navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { issuesForRepo, repoByName } from '../../sync/selectors';
import { Button } from '../../ui/Button';
import { GitPullRequestIcon } from '../../ui/icons';
import { IssueList } from '../issues/IssueList';

export default observer(function PullListPage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const repo = repoByName(owner, name);
  const repoId = repo?.id;
  const source = useCallback(() => (repoId ? issuesForRepo(repoId).filter((i) => i.isPr) : []), [repoId]);
  const compare = repo ? `/${repo.owner}/${repo.name}/compare` : '';
  useShortcuts('Pull requests', { c: { handler: () => navigate(compare), description: 'New pull request', group: 'Pull requests' } });
  if (!repo) return null;
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

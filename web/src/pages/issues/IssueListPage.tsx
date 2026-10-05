import { observer } from 'mobx-react-lite';
import { useCallback } from 'react';
import { useParams } from '../../router';
import { issuesForRepo, repoByName } from '../../sync/selectors';
import { IssueList } from './IssueList';

export default observer(function IssueListPage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const repo = repoByName(owner, name);
  const repoId = repo?.id;
  const source = useCallback(() => (repoId ? issuesForRepo(repoId).filter((i) => !i.isPr) : []), [repoId]);
  if (!repo) return null; // RepoLayout renders loading / not found
  return <IssueList kind="issue" repo={repo} source={source} />;
});

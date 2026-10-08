import { observer } from 'mobx-react-lite';
import { useEffect } from 'react';
import { NotFound } from '../../app/NotFound';
import { navigate, useParams } from '../../router';
import { issueByNumber, repoFullName } from '../../sync/selectors';
import { IssueActions } from './IssueActions';
import { IssueHeader } from './IssueHeader';
import { IssueSidebar } from './IssueSidebar';
import styles from './IssueView.module.css';
import { SubIssuesPanel } from './SubIssuesPanel';
import { Timeline } from './Timeline';
import { useRouteRepo } from '../repo/useRouteRepo';

/**
 * Issue detail. Renders instantly from the store (title, labels, state...);
 * lazy parts (body, comments, events) stream in via partial sync — usually
 * already prefetched when the link was hovered.
 */
export default observer(function IssueDetailPage() {
  const { number } = useParams<{ number: string }>();
  const repo = useRouteRepo();
  const issue = issueByNumber(repo.id, Number(number));

  useEffect(() => {
    // GitHub redirects /issues/N to /pull/N for pull requests.
    if (issue?.isPr) navigate(`/${repo.owner}/${repo.name}/pull/${issue.number}`, { replace: true });
  }, [issue?.isPr, issue?.number, repo]);

  if (!issue) return <NotFound what="issue" />;

  return (
    <div className={styles.page}>
      <IssueHeader issue={issue} />
      <div className={styles.columns}>
        <Timeline issue={issue} repoFullName={repoFullName(repo)} afterBody={<SubIssuesPanel issue={issue} repo={repo} />} />
        <IssueSidebar issue={issue} repo={repo} extra={<IssueActions issue={issue} repo={repo} />} />
      </div>
    </div>
  );
});

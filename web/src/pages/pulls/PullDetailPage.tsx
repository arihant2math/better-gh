import { observer } from 'mobx-react-lite';
import { Suspense, lazy } from 'react';
import { NotFound } from '../../app/NotFound';
import { navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import { usePullDetails } from '../../sync/hooks';
import type { Issue, Review } from '../../sync/models';
import { checkRunsFor, statusesFor, threadsForPull } from '../../sync/pullSelectors';
import { issueByNumber, repoByName, repoFullName } from '../../sync/selectors';
import { CheckCircleIcon, CommentDiscussionIcon, FileDiffIcon, GitCommitIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import { TabNav } from '../../ui/Tabs';
import { IssueHeader } from '../issues/IssueHeader';
import { IssueSidebar } from '../issues/IssueSidebar';
import styles from '../issues/IssueView.module.css';
import { Timeline } from '../issues/Timeline';
import { PullChecksIcon } from './ChecksIcon';
import { DeploymentsBanner } from './DeploymentsBanner';
import { MergeBox } from './MergeBox';
import pr from './PullDetail.module.css';
import { ReviewThreadView } from './ReviewThread';
import { Reviewers } from './Reviewers';

// Heavy tabs are separate chunks (prefetched by the route on link intent).
export const loadFilesTab = () => import('./FilesTab');
export const loadChecksTab = () => import('./ChecksTab');
export const loadCommitsTab = () => import('./CommitsTab');
const FilesTab = lazy(loadFilesTab);
const ChecksTab = lazy(loadChecksTab);
const CommitsTab = lazy(loadCommitsTab);

function TabLoading() {
  return (
    <div className={pr.loading}>
      <Spinner />
    </div>
  );
}

const FULL_HEIGHT = new Set(['files', 'checks', 'commit']);

export default observer(function PullDetailPage() {
  const params = useParams<{ owner: string; repo: string; number: string; tab?: string; sha?: string }>();
  const { owner, repo: name, number, sha } = params;
  const tab = params.tab ?? (sha ? 'commits' : 'conversation');
  const repo = repoByName(owner, name);
  const issue = repo ? issueByNumber(repo.id, Number(number)) : undefined;
  usePullDetails(issue?.isPr ? issue.id : undefined);
  const base = repo && issue ? `/${repo.owner}/${repo.name}/pull/${issue.number}` : '';
  useShortcuts('Pull request', {
    'g c': { handler: () => navigate(base), description: 'Conversation', group: 'Pull request' },
    'g m': { handler: () => navigate(`${base}/commits`), description: 'Commits', group: 'Pull request' },
    'g k': { handler: () => navigate(`${base}/checks`), description: 'Checks', group: 'Pull request' },
    'g f': { handler: () => navigate(`${base}/files`), description: 'Files changed', group: 'Pull request' },
  });
  if (!repo) return null;
  if (!issue || !issue.isPr) return <NotFound what="pull request" />;

  const author = store().get('user', issue.authorId);
  const view = tab === 'commits' && sha ? 'commit' : tab;
  const checksCount = checkRunsFor(issue.headSha).length + statusesFor(issue.headSha).length;
  const meta = (
    <span className={styles.metaText}>
      <strong>{author?.login}</strong> {issue.merged ? 'merged' : 'wants to merge'} {issue.commits ?? 0} commit{issue.commits === 1 ? '' : 's'} into{' '}
      <code className={styles.branch}>{issue.baseRef}</code> from <code className={styles.branch}>{issue.headRef}</code>
      {issue.merged && issue.mergedAt && (
        <>
          {' '}
          <RelativeTime date={issue.mergedAt} />
        </>
      )}
      {!issue.merged && issue.state === 'open' && (
        <>
          {' '}
          <PullChecksIcon checks={issue.checks} />
        </>
      )}
    </span>
  );

  return (
    <div className={FULL_HEIGHT.has(view) ? pr.filesPage : styles.page}>
      <div className={FULL_HEIGHT.has(view) ? pr.filesHeader : undefined}>
        <IssueHeader issue={issue} meta={meta} />
        <TabNav
          aria-label="Pull request"
          current={tab}
          className={pr.tabs}
          items={[
            { id: 'conversation', label: 'Conversation', icon: CommentDiscussionIcon, href: base, count: issue.comments },
            { id: 'commits', label: 'Commits', icon: GitCommitIcon, href: `${base}/commits`, count: issue.commits ?? 0 },
            { id: 'checks', label: 'Checks', icon: CheckCircleIcon, href: `${base}/checks`, count: checksCount },
            { id: 'files', label: 'Files changed', icon: FileDiffIcon, href: `${base}/files`, count: issue.changedFiles ?? 0 },
          ]}
        />
      </div>
      <Suspense fallback={<TabLoading />}>
        {tab === 'commits' ? (
          <CommitsTab repo={repo} pr={issue} sha={sha} base={base} />
        ) : tab === 'checks' ? (
          <ChecksTab repo={repo} pr={issue} />
        ) : tab === 'files' ? (
          <div className={pr.files}>
            <FilesTab repo={repo} pr={issue} />
          </div>
        ) : (
          <Conversation issue={issue} base={base} />
        )}
      </Suspense>
    </div>
  );
});

const Conversation = observer(function Conversation({ issue, base }: { issue: Issue; base: string }) {
  const repo = store().get('repo', issue.repoId)!;
  const full = repoFullName(repo);
  const threads = threadsForPull(issue.id);
  // Threads shown under the review that started them; replies stay in their thread.
  const renderReview = (review: Review) => {
    const mine = threads.filter((t) => t.root.reviewId === review.id && !t.pending);
    if (!mine.length) return null;
    return (
      <div className={pr.reviewThreads}>
        {mine.map((t) => (
          <ReviewThreadView key={t.id} thread={t} pr={issue} repo={full} showPath />
        ))}
      </div>
    );
  };
  return (
    <div className={styles.columns}>
      <Timeline issue={issue} repoFullName={full} footer={
          <>
            <DeploymentsBanner issue={issue} />
            <MergeBox issue={issue} base={base} />
          </>
        } renderReview={renderReview} />
      <IssueSidebar issue={issue} repo={repo} extra={<Reviewers issue={issue} />} />
    </div>
  );
});

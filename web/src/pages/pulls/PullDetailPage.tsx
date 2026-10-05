import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { ApiError } from '../../api/client';
import { useResource } from '../../api/cache';
import { getPullDiff, getPullRequirements, listPullCommits } from '../../api/endpoints';
import type { PullRequirements, RestCommit } from '../../api/types';
import { NotFound } from '../../app/NotFound';
import { DiffViewer } from '../../components/diff/DiffViewer';
import { useParams } from '../../router';
import { store } from '../../sync';
import type { Issue, Repo } from '../../sync/models';
import { mergePull, setDraft } from '../../sync/mutations';
import { canWrite, issueByNumber, repoByName, repoFullName, reviewsForIssue } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import {
  AlertIcon,
  CheckCircleIcon,
  CheckIcon,
  CommentDiscussionIcon,
  DotFillIcon,
  FileDiffIcon,
  GitCommitIcon,
  GitMergeIcon,
  GitPullRequestIcon,
  XCircleFillIcon,
} from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import { TabNav } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { IssueHeader } from '../issues/IssueHeader';
import { IssueSidebar } from '../issues/IssueSidebar';
import styles from '../issues/IssueView.module.css';
import { Timeline } from '../issues/Timeline';
import pr from './PullDetail.module.css';

export default observer(function PullDetailPage() {
  const { owner, repo: name, number, tab = 'conversation' } = useParams<{ owner: string; repo: string; number: string; tab?: string }>();
  const repo = repoByName(owner, name);
  const issue = repo ? issueByNumber(repo.id, Number(number)) : undefined;
  if (!repo) return null;
  if (!issue || !issue.isPr) return <NotFound what="pull request" />;

  const author = store().get('user', issue.authorId);
  const base = `/${repo.owner}/${repo.name}/pull/${issue.number}`;
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
    </span>
  );

  return (
    <div className={tab === 'files' ? pr.filesPage : styles.page}>
      <div className={tab === 'files' ? pr.filesHeader : undefined}>
        <IssueHeader issue={issue} meta={meta} />
        <TabNav
          aria-label="Pull request"
          current={tab}
          className={pr.tabs}
          items={[
            { id: 'conversation', label: 'Conversation', icon: CommentDiscussionIcon, href: base, count: issue.comments },
            { id: 'commits', label: 'Commits', icon: GitCommitIcon, href: `${base}/commits`, count: issue.commits ?? 0 },
            { id: 'files', label: 'Files changed', icon: FileDiffIcon, href: `${base}/files`, count: issue.changedFiles ?? 0 },
          ]}
        />
      </div>
      {tab === 'commits' ? (
        <Commits repo={repo} issue={issue} />
      ) : tab === 'files' ? (
        <Files repo={repo} issue={issue} />
      ) : (
        <div className={styles.columns}>
          <Timeline issue={issue} repoFullName={repoFullName(repo)} footer={<MergeBox issue={issue} />} />
          <IssueSidebar issue={issue} repo={repo} extra={<Reviewers issue={issue} />} />
        </div>
      )}
    </div>
  );
});

const Reviewers = observer(function Reviewers({ issue }: { issue: Issue }) {
  const s = store();
  const reviews = reviewsForIssue(issue.id);
  const latest = new Map<number, string>();
  for (const r of reviews) latest.set(r.authorId, r.state);
  const requested = (issue.requestedReviewerIds ?? []).filter((id) => !latest.has(id));
  return (
    <section className={styles.sideSection}>
      <div className={styles.sideHeaderStatic}>Reviewers</div>
      <div className={styles.sideBody}>
        {latest.size === 0 && requested.length === 0 && <span className={styles.subtle}>No reviews</span>}
        {[...latest.entries()].map(([id, state]) => (
          <span key={id} className={styles.person}>
            <Avatar user={s.get('user', id)} size={20} />
            {s.get('user', id)?.login}
            <span style={{ marginLeft: 'auto' }}>
              {state === 'APPROVED' ? (
                <CheckIcon size={16} className={pr.ok} />
              ) : state === 'CHANGES_REQUESTED' ? (
                <XCircleFillIcon size={16} className={pr.fail} />
              ) : (
                <CommentDiscussionIcon size={16} className={pr.muted} />
              )}
            </span>
          </span>
        ))}
        {requested.map((id) => (
          <span key={id} className={styles.person}>
            <Avatar user={s.get('user', id)} size={20} />
            {s.get('user', id)?.login}
            <span style={{ marginLeft: 'auto' }}>
              <DotFillIcon size={16} className={pr.pending} />
            </span>
          </span>
        ))}
      </div>
    </section>
  );
});

type MergeMethod = 'merge' | 'squash' | 'rebase';
const METHOD_LABEL: Record<MergeMethod, string> = {
  merge: 'Create a merge commit',
  squash: 'Squash and merge',
  rebase: 'Rebase and merge',
};

const MergeBox = observer(function MergeBox({ issue }: { issue: Issue }) {
  const writable = canWrite(issue.repoId);
  const repo = store().get('repo', issue.repoId);
  const open = issue.state === 'open' && !issue.merged;
  // Requirements depend on the head/base, reviews and checks: key the
  // resource on what the synced row tells us so it refetches on change.
  const key =
    repo && open
      ? `requirements:${repo.owner}/${repo.name}#${issue.number}@${issue.headSha}:${issue.baseSha}:${issue.mergeableState}:${issue.reviewDecision}:${issue.checks}:${issue.draft}`
      : null;
  const { data: req } = useResource<PullRequirements>(key, () => getPullRequirements(repo!.owner, repo!.name, issue.number), { ttlMs: 10_000 });
  const [method, setMethod] = useState<MergeMethod | null>(null);

  if (issue.merged) {
    return (
      <div className={styles.mergeBox}>
        <span className={styles.mergeIcon} style={{ background: 'var(--merged)' }}>
          <GitMergeIcon size={18} />
        </span>
        <div className={styles.mergeCard}>
          <div className={styles.mergeRow}>
            <span className={styles.mergeRowTitle}>Pull request successfully merged and closed</span>
          </div>
        </div>
      </div>
    );
  }
  if (!open) return null;
  const checksOk = issue.checks === 'success' || issue.checks === 'neutral' || !issue.checks;
  const conflict = issue.mergeableState === 'dirty' || req?.mergeable === false;
  const computing = issue.mergeable === null || issue.mergeableState === 'unknown';
  const blockers = req?.blockers ?? [];
  const blockedByRules = blockers.length > 0 && !req?.can_bypass;
  const blocked = conflict || !!issue.draft || blockedByRules;
  const approved = issue.reviewDecision === 'approved';
  const methods: MergeMethod[] = req?.allowed_merge_methods ?? ['merge', 'squash', 'rebase'];
  const chosen: MergeMethod = method && methods.includes(method) ? method : (methods[0] ?? 'merge');
  const reviewHint = req
    ? req.required_approvals > 0
      ? `${req.approvals} of ${req.required_approvals} required approving review${req.required_approvals === 1 ? '' : 's'}.`
      : 'Reviews are not required by branch protection.'
    : approved
      ? 'At least one approving review.'
      : 'Waiting for reviews.';
  return (
    <div className={styles.mergeBox}>
      <span className={styles.mergeIcon} style={{ background: blocked ? 'var(--draft)' : 'var(--open)' }}>
        <GitPullRequestIcon size={18} />
      </span>
      <div className={styles.mergeCard}>
        <div className={styles.mergeRow}>
          {approved ? <CheckCircleIcon size={20} className={pr.ok} /> : <AlertIcon size={20} className={pr.pending} />}
          <div>
            <div className={styles.mergeRowTitle}>{approved ? 'Changes approved' : issue.reviewDecision === 'changes_requested' ? 'Changes requested' : 'Review required'}</div>
            <div className={styles.subtle}>{reviewHint}</div>
          </div>
        </div>
        <div className={styles.mergeRow}>
          {checksOk ? <CheckCircleIcon size={20} className={pr.ok} /> : issue.checks === 'pending' ? <Spinner size={18} /> : <XCircleFillIcon size={20} className={pr.fail} />}
          <div>
            <div className={styles.mergeRowTitle}>{checksOk ? 'All checks have passed' : issue.checks === 'pending' ? 'Some checks haven’t completed yet' : 'Some checks were not successful'}</div>
            <div className={styles.subtle}>{req?.required_checks.length ? `Required: ${req.required_checks.join(', ')}` : 'No required checks'}</div>
          </div>
        </div>
        {blockers.length > 0 && (
          <div className={styles.mergeRow}>
            <XCircleFillIcon size={20} className={req?.can_bypass ? pr.pending : pr.fail} />
            <div>
              <div className={styles.mergeRowTitle}>Merging is blocked{req?.can_bypass ? ' (you can bypass as an administrator)' : ''}</div>
              {blockers.map((b) => (
                <div key={b} className={styles.subtle}>
                  {b}
                </div>
              ))}
            </div>
          </div>
        )}
        <div className={styles.mergeRow}>
          {conflict ? <XCircleFillIcon size={20} className={pr.fail} /> : computing ? <Spinner size={18} /> : <CheckCircleIcon size={20} className={pr.ok} />}
          <div style={{ flex: 1 }}>
            <div className={styles.mergeRowTitle}>
              {issue.draft
                ? 'This pull request is still a work in progress'
                : conflict
                  ? 'This branch has conflicts that must be resolved'
                  : computing
                    ? 'Checking for the ability to merge automatically…'
                    : req?.behind
                      ? 'This branch is out-of-date with the base branch'
                      : 'No conflicts with base branch'}
            </div>
            <div className={styles.subtle}>{issue.draft ? 'Draft pull requests cannot be merged.' : 'Merging can be performed automatically.'}</div>
          </div>
          {writable &&
            (issue.draft ? (
              <Button onClick={() => setDraft(issue, false)}>Ready for review</Button>
            ) : (
              <>
                {methods.length > 1 && (
                  <select aria-label="Merge method" value={chosen} onChange={(e) => setMethod(e.target.value as MergeMethod)}>
                    {methods.map((m) => (
                      <option key={m} value={m}>
                        {METHOD_LABEL[m]}
                      </option>
                    ))}
                  </select>
                )}
                <Button
                  variant="success"
                  leadingIcon={GitMergeIcon}
                  disabled={blocked || computing}
                  onClick={() => {
                    mergePull(issue, chosen).done.then(
                      () => toast({ kind: 'success', title: `Merged #${issue.number}` }),
                      () => undefined,
                    );
                  }}
                >
                  {chosen === 'merge' ? 'Merge pull request' : METHOD_LABEL[chosen]}
                </Button>
              </>
            ))}
        </div>
      </div>
    </div>
  );
});

function Commits({ repo, issue }: { repo: Repo; issue: Issue }) {
  const { data, loading, error } = useResource<RestCommit[]>(`commits:${repo.owner}/${repo.name}#${issue.number}`, () =>
    listPullCommits(repo.owner, repo.name, issue.number),
  );
  if (error) return <EmptyState icon={AlertIcon} title="Couldn’t load commits" />;
  if (loading || !data) {
    return (
      <div className={pr.commits}>
        {Array.from({ length: Math.min(issue.commits ?? 3, 6) }, (_, i) => (
          <div key={i} className={pr.commit}>
            <Skeleton width="50%" />
          </div>
        ))}
      </div>
    );
  }
  const byDay = new Map<string, RestCommit[]>();
  for (const c of data) {
    const day = new Date(c.commit.author.date).toLocaleDateString('en', { month: 'short', day: 'numeric', year: 'numeric' });
    byDay.set(day, [...(byDay.get(day) ?? []), c]);
  }
  return (
    <div className={pr.commits}>
      {[...byDay.entries()].map(([day, list]) => (
        <section key={day} className={pr.commitDay}>
          <h3 className={pr.commitDayTitle}>
            <GitCommitIcon size={16} /> Commits on {day}
          </h3>
          <div className={pr.commitList}>
            {list.map((c) => (
              <div key={c.sha} className={pr.commit}>
                <div className={pr.commitMain}>
                  <div className={pr.commitMsg}>{c.commit.message.split('\n')[0]}</div>
                  <div className={styles.subtle}>
                    <Avatar user={c.author ? { login: c.author.login, avatarUrl: c.author.avatar_url } : null} size={16} /> <strong>{c.author?.login ?? c.commit.author.name}</strong> committed{' '}
                    <RelativeTime date={c.commit.author.date} />
                  </div>
                </div>
                <code className={pr.sha}>{c.sha.slice(0, 7)}</code>
              </div>
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

function Files({ repo, issue }: { repo: Repo; issue: Issue }) {
  // Keyed by the SHAs: a diff between two commits never changes.
  const key = `diff:${repo.owner}/${repo.name}#${issue.number}@${issue.baseSha}...${issue.headSha}`;
  const { data, loading, error } = useResource(key, () => getPullDiff(repo.owner, repo.name, issue.number), { immutable: true });
  if (error) {
    const tooLarge = error instanceof ApiError && error.status === 406;
    return <EmptyState icon={AlertIcon} title={tooLarge ? 'This diff is too large to display' : 'Couldn’t load the diff'} />;
  }
  if (loading || data === undefined) {
    return (
      <div className={pr.loading}>
        <Spinner /> Loading diff…
      </div>
    );
  }
  return (
    <div className={pr.files}>
      <DiffViewer diff={data} />
    </div>
  );
}

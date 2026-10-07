import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { mutate, refresh, usePollWhileVisible, useResource } from '../../api/cache';
import { dequeuePull, enqueuePull, getPullRequirements } from '../../api/endpoints';
import type { MergeQueueEntry, PullRequirements } from '../../api/types';
import { Link } from '../../router';
import { store } from '../../sync';
import type { Issue } from '../../sync/models';
import { setDraft } from '../../sync/mutations';
import { deleteHeadBranch, disableAutoMerge, enableAutoMerge, mergePullWith, updateBranch, type MergeMethod } from '../../sync/pullMutations';
import { checkRunsFor, checksSummary, latestReviews, runRollup, statusesFor, statusRollup } from '../../sync/pullSelectors';
import { canWrite, eventsForIssue } from '../../sync/selectors';
import { Button, cx } from '../../ui/Button';
import { AlertIcon, CheckCircleIcon, CheckIcon, ChevronDownIcon, DotFillIcon, GitBranchIcon, GitMergeIcon, GitMergeQueueIcon, GitPullRequestClosedIcon, GitPullRequestIcon, TrashIcon, XCircleFillIcon } from '../../ui/icons';
import { Input, Textarea } from '../../ui/Input';
import { Menu } from '../../ui/Menu';
import { Spinner } from '../../ui/Spinner';
import { toast } from '../../ui/Toast';
import styles from '../issues/IssueView.module.css';
import { RollupIcon } from './ChecksIcon';
import { entryState, etaLabel, lastQueueEventId, positionLabel, QUEUE_POLL_MS, queuePath } from './mergeQueue';
import pr from './PullDetail.module.css';

const METHOD_LABEL: Record<MergeMethod, string> = {
  merge: 'Create a merge commit',
  squash: 'Squash and merge',
  rebase: 'Rebase and merge',
};
const METHOD_HINT: Record<MergeMethod, string> = {
  merge: 'All commits from this branch will be added to the base branch via a merge commit.',
  squash: 'The commits from this branch will be combined into one commit in the base branch.',
  rebase: 'The commits from this branch will be rebased and added to the base branch.',
};
const BUTTON_LABEL: Record<MergeMethod, string> = { merge: 'Merge pull request', squash: 'Squash and merge', rebase: 'Rebase and merge' };

function savedMethod(repoId: number): MergeMethod | null {
  try {
    return localStorage.getItem(`bgh:merge-method:${repoId}`) as MergeMethod | null;
  } catch {
    return null;
  }
}

/** The merge box under the conversation: reviews, checks, blockers, conflicts, merge / auto-merge / update / delete branch. */
export const MergeBox = observer(function MergeBox({ issue, base }: { issue: Issue; base: string }) {
  const writable = canWrite(issue.repoId);
  const repo = store().get('repo', issue.repoId);
  const open = issue.state === 'open' && !issue.merged;
  const viewer = store().viewerId;
  const isAuthor = issue.authorId === viewer;
  // Enqueue / dequeue / ejection (by anyone) land as synced timeline events: fold the newest into the key.
  const queueEvent = open ? lastQueueEventId(eventsForIssue(issue.id)) : 0;
  const key =
    repo && open
      ? `requirements:${repo.owner}/${repo.name}#${issue.number}@${issue.headSha}:${issue.baseSha}:${issue.mergeableState}:${issue.reviewDecision}:${issue.checks}:${issue.draft}:q${queueEvent}`
      : null;
  const loadReq = () => getPullRequirements(repo!.owner, repo!.name, issue.number);
  const { data: req } = useResource<PullRequirements>(key, loadReq, { ttlMs: 10_000 });
  // A queued entry moves on its own (checks finish, PRs ahead merge): poll like the queue page.
  usePollWhileVisible(req?.merge_queue?.entry ? key : null, loadReq, QUEUE_POLL_MS, { ttlMs: 10_000 });
  const [queueBusy, setQueueBusy] = useState(false);
  const [method, setMethod] = useState<MergeMethod | null>(() => savedMethod(issue.repoId));
  const [menu, setMenu] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [title, setTitle] = useState('');
  const [message, setMessage] = useState('');
  const [showChecks, setShowChecks] = useState(false);
  const [branchDeleted, setBranchDeleted] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);

  if (issue.merged) {
    const sameRepo = (issue.headRepoId ?? issue.repoId) === issue.repoId;
    return (
      <div className={styles.mergeBox}>
        <span className={styles.mergeIcon} style={{ background: 'var(--merged)' }}>
          <GitMergeIcon size={18} />
        </span>
        <div className={styles.mergeCard}>
          <div className={styles.mergeRow}>
            <div style={{ flex: 1 }}>
              <div className={styles.mergeRowTitle}>Pull request successfully merged and closed</div>
              <div className={styles.subtle}>
                {branchDeleted ? (
                  <>
                    The <code className={styles.branch}>{issue.headRef}</code> branch has been deleted.
                  </>
                ) : (
                  <>
                    You’re all set — the <code className={styles.branch}>{issue.headRef}</code> branch can be safely deleted.
                  </>
                )}
              </div>
            </div>
            {writable && sameRepo && !branchDeleted && (
              <Button
                leadingIcon={TrashIcon}
                onClick={() => {
                  setBranchDeleted(true);
                  deleteHeadBranch(issue).done.catch(() => setBranchDeleted(false));
                }}
              >
                Delete branch
              </Button>
            )}
          </div>
        </div>
      </div>
    );
  }
  if (!open) {
    return (
      <div className={styles.mergeBox}>
        <span className={styles.mergeIcon} style={{ background: 'var(--closed)' }}>
          <GitPullRequestClosedIcon size={18} />
        </span>
        <div className={styles.mergeCard}>
          <div className={styles.mergeRow}>
            <div>
              <div className={styles.mergeRowTitle}>Closed with unmerged commits</div>
              <div className={styles.subtle}>This pull request is closed.</div>
            </div>
          </div>
        </div>
      </div>
    );
  }

  const summary = checksSummary(issue.headSha);
  const checksState = summary.state ?? issue.checks ?? null;
  const checksOk = checksState === 'success' || checksState === 'neutral' || checksState === 'skipped' || !checksState;
  const conflict = issue.mergeableState === 'dirty' || req?.mergeable === false;
  const computing = issue.mergeable == null && issue.mergeableState === 'unknown' && !req;
  const behind = !!req?.behind || issue.mergeableState === 'behind';
  const blockers = req?.blockers ?? [];
  const blockedByRules = blockers.length > 0 && !req?.can_bypass;
  const blocked = conflict || !!issue.draft || blockedByRules;
  const approved = issue.reviewDecision === 'approved';
  const methods: MergeMethod[] = req?.allowed_merge_methods?.length ? req.allowed_merge_methods : ['merge', 'squash', 'rebase'];
  const chosen: MergeMethod = method && methods.includes(method) ? method : methods[0]!;
  const reviews = latestReviews(issue.id);
  const approvals = [...reviews.values()].filter((r) => r.state === 'APPROVED').length;
  const reviewsOptional = !!req && req.required_approvals === 0 && issue.reviewDecision !== 'changes_requested';
  const reviewHint = req
    ? req.required_approvals > 0
      ? `${req.approvals} of ${req.required_approvals} required approving review${req.required_approvals === 1 ? '' : 's'}.`
      : approvals
        ? `${approvals} approving review${approvals === 1 ? '' : 's'}.`
        : 'Reviews are not required by branch protection.'
    : approved
      ? 'At least one approving review.'
      : 'Waiting for reviews.';
  const runs = checkRunsFor(issue.headSha);
  const statuses = statusesFor(issue.headSha);
  const auto = issue.autoMerge;
  const queue = req?.merge_queue?.required ? req.merge_queue : null;
  const queued = queue?.entry ?? null;
  const canAutoMerge = !queue && writable && !conflict && !issue.draft && !auto && (blocked || !checksOk);

  /** Put the queue entry into the cached requirements now, then revalidate. */
  const settleQueue = (entry: MergeQueueEntry | null) => {
    if (!key) return;
    mutate<PullRequirements>(key, (prev) => (prev?.merge_queue ? { ...prev, merge_queue: { ...prev.merge_queue, entry } } : prev!));
    void refresh(key, loadReq, { ttlMs: 10_000 }).catch(() => undefined);
  };
  const addToQueue = () => {
    setQueueBusy(true);
    enqueuePull(repo!.owner, repo!.name, issue.number)
      .then(
        (entry) => {
          settleQueue(entry);
          toast({ kind: 'success', title: `Added #${issue.number} to the merge queue` });
        },
        (e: unknown) => {
          if (key) void refresh(key, loadReq, { ttlMs: 10_000 }).catch(() => undefined);
          toast({ kind: 'error', title: 'Couldn’t add to the merge queue', description: e instanceof Error ? e.message : undefined });
        },
      )
      .finally(() => setQueueBusy(false));
  };
  const removeFromQueue = () => {
    setQueueBusy(true);
    dequeuePull(repo!.owner, repo!.name, issue.number)
      .then(
        () => {
          settleQueue(null);
          toast({ kind: 'success', title: `Removed #${issue.number} from the merge queue` });
        },
        (e: unknown) => {
          if (key) void refresh(key, loadReq, { ttlMs: 10_000 }).catch(() => undefined);
          toast({ kind: 'error', title: 'Couldn’t remove from the merge queue', description: e instanceof Error ? e.message : undefined });
        },
      )
      .finally(() => setQueueBusy(false));
  };
  const queueState = queued ? entryState(queued) : null;
  const queueEta = queued ? etaLabel(queued.estimated_time_to_merge) : null;

  const startConfirm = () => {
    setTitle(chosen === 'squash' ? `${issue.title} (#${issue.number})` : chosen === 'merge' ? `Merge pull request #${issue.number} from ${issue.headRef}` : '');
    setMessage(chosen === 'merge' ? issue.title : '');
    setConfirming(true);
  };
  const doMerge = () => {
    setConfirming(false);
    mergePullWith(issue, chosen, chosen === 'rebase' ? undefined : title, chosen === 'rebase' ? undefined : message).done.then(
      () => toast({ kind: 'success', title: `Merged #${issue.number}` }),
      () => undefined,
    );
  };

  return (
    <div className={styles.mergeBox}>
      <span className={styles.mergeIcon} style={{ background: blocked || conflict ? 'var(--draft)' : 'var(--open)' }}>
        <GitPullRequestIcon size={18} />
      </span>
      <div className={styles.mergeCard}>
        <div className={styles.mergeRow}>
          {approved || (reviewsOptional && approvals > 0) ? <CheckCircleIcon size={20} className={pr.ok} /> : reviewsOptional ? <DotFillIcon size={20} className={pr.muted} /> : issue.reviewDecision === 'changes_requested' ? <XCircleFillIcon size={20} className={pr.fail} /> : <AlertIcon size={20} className={pr.pending} />}
          <div>
            <div className={styles.mergeRowTitle}>
              {approved ? 'Changes approved' : issue.reviewDecision === 'changes_requested' ? 'Changes requested' : reviewsOptional ? (approvals ? 'Approved' : 'No reviews yet') : 'Review required'}
            </div>
            <div className={styles.subtle}>{reviewHint}</div>
          </div>
        </div>
        <div className={styles.mergeRow}>
          {checksOk ? <CheckCircleIcon size={20} className={pr.ok} /> : checksState === 'pending' ? <Spinner size={18} /> : <XCircleFillIcon size={20} className={pr.fail} />}
          <div style={{ flex: 1 }}>
            <div className={styles.mergeRowTitle}>
              {!checksState ? 'No checks reported' : checksOk ? 'All checks have passed' : checksState === 'pending' ? 'Some checks haven’t completed yet' : 'Some checks were not successful'}
            </div>
            <div className={styles.subtle}>
              {summary.total > 0 && `${[summary.success && `${summary.success} successful`, summary.failure && `${summary.failure} failing`, summary.pending && `${summary.pending} in progress`, (summary.neutral || summary.skipped) && `${summary.neutral + summary.skipped} skipped or neutral`].filter(Boolean).join(', ')} checks. `}
              {req?.required_checks.length ? `Required: ${req.required_checks.join(', ')}` : summary.total === 0 ? 'No required checks' : ''}
            </div>
          </div>
          {summary.total > 0 && (
            <Button size="sm" variant="ghost" onClick={() => setShowChecks((s) => !s)} aria-expanded={showChecks}>
              {showChecks ? 'Hide all checks' : 'Show all checks'}
            </Button>
          )}
        </div>
        {showChecks && (
          <div className={pr.checkList}>
            {runs.map((r) => (
              <Link key={`r${r.id}`} to={`${base}/checks?run=${r.id}`} className={pr.checkItem}>
                <RollupIcon state={runRollup(r.status, r.conclusion)} />
                <strong>{r.name}</strong>
                <span className={styles.subtle}>{r.title ?? r.conclusion ?? r.status.replace('_', ' ')}</span>
              </Link>
            ))}
            {statuses.map((st) => (
              <div key={`s${st.id}`} className={pr.checkItem}>
                <RollupIcon state={statusRollup(st.state)} />
                <strong>{st.context}</strong>
                <span className={styles.subtle}>{st.description}</span>
              </div>
            ))}
          </div>
        )}
        {blockers.length > 0 && (
          <div className={styles.mergeRow}>
            <XCircleFillIcon size={20} className={req?.can_bypass ? pr.pending : pr.fail} />
            <div>
              <div className={styles.mergeRowTitle}>Merging is blocked{req?.can_bypass ? ' (you can bypass as an administrator)' : ''}</div>
              {(req?.requirements ?? blockers.map((message) => ({ message, source: '' }))).map((b, i) => (
                <div key={`${i}-${b.message}`} className={styles.subtle}>
                  {b.message}
                  {b.source ? ` (${b.source})` : ''}
                </div>
              ))}
            </div>
          </div>
        )}
        {behind && !conflict && (
          <div className={styles.mergeRow}>
            <AlertIcon size={20} className={pr.pending} />
            <div style={{ flex: 1 }}>
              <div className={styles.mergeRowTitle}>This branch is out-of-date with the base branch</div>
              <div className={styles.subtle}>Merge the latest changes from {issue.baseRef} into this branch.</div>
            </div>
            {(writable || isAuthor) && (
              <Button
                leadingIcon={GitBranchIcon}
                onClick={() =>
                  void updateBranch(issue).done.then(
                    () => toast({ kind: 'success', title: 'Updating branch…' }),
                    () => undefined,
                  )
                }
              >
                Update branch
              </Button>
            )}
          </div>
        )}
        {auto && (
          <div className={styles.mergeRow}>
            <GitMergeIcon size={20} className={pr.ok} />
            <div style={{ flex: 1 }}>
              <div className={styles.mergeRowTitle}>Auto-merge enabled</div>
              <div className={styles.subtle}>
                This pull request will be merged automatically ({METHOD_LABEL[auto.mergeMethod].toLowerCase()}) when all requirements are met.
              </div>
            </div>
            {(writable || isAuthor) && <Button onClick={() => disableAutoMerge(issue)}>Disable auto-merge</Button>}
          </div>
        )}
        {queued && queue && queueState && (
          <div className={cx(styles.mergeRow, pr.queueRow)} data-testid="merge-queue-status">
            <GitMergeQueueIcon size={20} className={queueState.tone === 'fail' ? pr.fail : queueState.tone === 'ok' ? pr.ok : pr.pending} />
            <div className={pr.queueText} role="status" aria-live="polite">
              <div className={styles.mergeRowTitle}>
                Queued to merge · {positionLabel(queued)}
              </div>
              <div className={styles.subtle}>
                {queueState.label}
                {queueEta && ` · ${queueEta} to merge`}
                {' · '}
                <Link to={queuePath(repo!.owner, repo!.name, queue.branch)}>View merge queue</Link>
              </div>
            </div>
            {(writable || isAuthor) && (
              <Button onClick={removeFromQueue} loading={queueBusy} className={pr.touchButton}>
                Remove from queue
              </Button>
            )}
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
                    : 'No conflicts with base branch'}
            </div>
            <div className={styles.subtle}>
              {issue.draft
                ? 'Draft pull requests cannot be merged.'
                : conflict
                  ? 'Use the command line to resolve conflicts before continuing.'
                  : blocked
                    ? 'Merging is blocked until the requirements above are met.'
                    : queue
                      ? queued
                        ? 'This pull request will be merged by the merge queue.'
                        : `Changes to ${queue.branch} must go through the merge queue.`
                      : 'Merging can be performed automatically.'}
            </div>
          </div>
          {(writable || isAuthor) &&
            (issue.draft ? (
              <Button onClick={() => setDraft(issue, false)}>Ready for review</Button>
            ) : queue ? (
              writable &&
              !queued && (
                <Button variant="success" leadingIcon={GitMergeQueueIcon} disabled={blocked || computing} loading={queueBusy} onClick={addToQueue} className={pr.touchButton}>
                  Add to merge queue
                </Button>
              )
            ) : writable && !confirming ? (
              <span className={pr.splitButton}>
                <Button variant="success" leadingIcon={GitMergeIcon} disabled={blocked || computing} onClick={startConfirm}>
                  {BUTTON_LABEL[chosen]}
                </Button>
                {methods.length > 1 && (
                  <Button ref={menuRef} variant="success" aria-label="Select merge method" onClick={() => setMenu((m) => !m)} className={pr.splitToggle}>
                    <ChevronDownIcon size={16} />
                  </Button>
                )}
                <Menu
                  open={menu}
                  onClose={() => setMenu(false)}
                  anchor={menuRef}
                  placement="bottom-end"
                  items={methods.map((m) => ({
                    id: m,
                    label: METHOD_LABEL[m],
                    description: METHOD_HINT[m],
                    leading: <span className={pr.menuCheck}>{m === chosen && <CheckIcon size={16} />}</span>,
                    onSelect: () => {
                      setMethod(m);
                      try {
                        localStorage.setItem(`bgh:merge-method:${issue.repoId}`, m);
                      } catch {
                        /* ignore */
                      }
                    },
                  }))}
                />
              </span>
            ) : null)}
        </div>
        {confirming && (
          <div className={cx(styles.mergeRow, pr.confirm)}>
            {chosen !== 'rebase' && (
              <>
                <Input value={title} onChange={(e) => setTitle(e.target.value)} aria-label="Commit title" autoFocus />
                <Textarea value={message} onChange={(e) => setMessage(e.target.value)} rows={3} aria-label="Commit message" />
              </>
            )}
            <div className={pr.confirmActions}>
              <Button variant="success" onClick={doMerge}>
                Confirm {chosen === 'merge' ? 'merge' : chosen === 'squash' ? 'squash and merge' : 'rebase and merge'}
              </Button>
              <Button variant="ghost" onClick={() => setConfirming(false)}>
                Cancel
              </Button>
            </div>
          </div>
        )}
        {(canAutoMerge || (!issue.draft && (writable || isAuthor))) && !confirming && (
          <div className={cx(styles.mergeRow, pr.mergeFooter)}>
            {canAutoMerge && (
              <Button
                size="sm"
                onClick={() =>
                  void enableAutoMerge(issue, chosen).done.then(
                    () => toast({ kind: 'success', title: 'Auto-merge enabled' }),
                    () => undefined,
                  )
                }
              >
                Enable auto-merge ({METHOD_LABEL[chosen].toLowerCase()})
              </Button>
            )}
            <span style={{ flex: 1 }} />
            {!issue.draft && (writable || isAuthor) && (
              <Button size="sm" variant="ghost" onClick={() => setDraft(issue, true)}>
                Convert to draft
              </Button>
            )}
          </div>
        )}
      </div>
    </div>
  );
});

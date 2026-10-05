import { observer } from 'mobx-react-lite';
import type { MouseEvent } from 'react';
import { Link, navigate, prefetch } from '../../router';
import { store } from '../../sync';
import type { Issue } from '../../sync/models';
import { AvatarStack, LabelPill, StateIcon } from '../../ui/Badge';
import { cx } from '../../ui/Button';
import { CheckIcon, CommentIcon, DotFillIcon, MilestoneIcon, XIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tooltip } from '../../ui/Tooltip';
import styles from './IssueList.module.css';

export function issueHref(issue: Pick<Issue, 'repoId' | 'number' | 'isPr'>): string {
  const r = store().get('repo', issue.repoId);
  return r ? `/${r.owner}/${r.name}/${issue.isPr ? 'pull' : 'issues'}/${issue.number}` : '/';
}

const ChecksIcon = observer(function ChecksIcon({ issue }: { issue: Issue }) {
  if (!issue.isPr || !issue.checks || issue.state !== 'open') return null;
  const map = {
    success: { icon: <CheckIcon size={14} />, cls: styles.checkOk, label: 'All checks have passed' },
    failure: { icon: <XIcon size={14} />, cls: styles.checkFail, label: 'Some checks were not successful' },
    pending: { icon: <DotFillIcon size={14} />, cls: styles.checkPending, label: 'Checks are running' },
    neutral: { icon: <DotFillIcon size={14} />, cls: styles.checkNeutral, label: 'Checks neutral' },
  } as const;
  const c = map[issue.checks];
  return (
    <Tooltip label={c.label}>
      <span className={cx(styles.checks, c.cls)} aria-label={c.label}>
        {c.icon}
      </span>
    </Tooltip>
  );
});

const REVIEW_TEXT = { approved: 'Approved', changes_requested: 'Changes requested', review_required: 'Review required' } as const;

export const IssueRow = observer(function IssueRow({
  issue,
  showRepo,
  selected,
  active,
  selectable = true,
  onToggleSelect,
  onActivate,
}: {
  issue: Issue;
  showRepo?: boolean;
  selected?: boolean;
  active?: boolean;
  selectable?: boolean;
  onToggleSelect?: (e: MouseEvent) => void;
  onActivate?: () => void;
}) {
  const s = store();
  const href = issueHref(issue);
  const repo = s.get('repo', issue.repoId);
  const author = s.get('user', issue.authorId);
  const milestone = s.get('milestone', issue.milestoneId);
  const labels = issue.labelIds.map((id) => s.get('label', id)).filter((l) => !!l);
  const pending = issue.id < 0;
  return (
    <div
      className={cx(styles.row, active && styles.rowActive, selected && styles.rowSelected, pending && styles.rowPending)}
      role="listitem"
      aria-selected={selected}
      onMouseEnter={() => {
        onActivate?.();
        if (!pending) prefetch(href);
      }}
      onClick={(e) => {
        if ((e.target as HTMLElement).closest('a,button,input')) return;
        if (e.metaKey || e.ctrlKey) window.open(href, '_blank');
        else if (!pending) navigate(href);
      }}
    >
      {selectable && (
        <input
          type="checkbox"
          className={styles.checkbox}
          checked={!!selected}
          aria-label={`Select #${issue.number}`}
          onChange={() => undefined}
          onClick={(e) => {
            e.stopPropagation();
            onToggleSelect?.(e);
          }}
        />
      )}
      <span className={styles.stateIcon}>
        <StateIcon issue={issue} />
      </span>
      <div className={styles.main}>
        <div className={styles.titleLine}>
          {showRepo && repo && (
            <span className={styles.repoName}>
              {repo.owner}/{repo.name}
            </span>
          )}
          <Link to={href} className={styles.title} prefetch={!pending}>
            {issue.title}
          </Link>
          {issue.draft && <span className={styles.draftTag}>Draft</span>}
          {labels.map((l) => (
            <LabelPill key={l.id} label={l} size="sm" />
          ))}
        </div>
        <div className={styles.meta}>
          {pending ? (
            <span>Creating…</span>
          ) : (
            <>
              <span>#{issue.number}</span>
              <span>
                {issue.state === 'open' ? 'opened ' : issue.merged ? 'merged ' : 'closed '}
                <RelativeTime date={issue.state === 'open' ? issue.createdAt : (issue.closedAt ?? issue.updatedAt)} />
                {issue.state === 'open' && author && <> by {author.login}</>}
              </span>
              {issue.isPr && issue.state === 'open' && issue.reviewDecision && (
                <span className={cx(styles.review, styles[issue.reviewDecision])}>{REVIEW_TEXT[issue.reviewDecision]}</span>
              )}
              {milestone && (
                <span className={styles.milestone}>
                  <MilestoneIcon size={12} />
                  {milestone.title}
                </span>
              )}
            </>
          )}
        </div>
      </div>
      <div className={styles.trailing}>
        <ChecksIcon issue={issue} />
        <AvatarStack users={issue.assigneeIds.map((id) => s.get('user', id))} size={20} />
        <span className={styles.comments} aria-label={`${issue.comments} comments`} data-zero={issue.comments === 0}>
          <CommentIcon size={14} />
          {issue.comments}
        </span>
        <span className={styles.updated}>
          <RelativeTime date={issue.updatedAt} short />
        </span>
      </div>
    </div>
  );
});

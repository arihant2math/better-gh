import { observer } from 'mobx-react-lite';
import { useRef, useState, type ReactNode, type Ref } from 'react';
import { store } from '../../sync';
import { useIssueDetails } from '../../sync/hooks';
import type { Comment, Issue, IssueEvent, Review } from '../../sync/models';
import { closeIssue, createComment, deleteComment, editComment, reopenIssue, updateIssue } from '../../sync/mutations';
import { canWrite, commentsForIssue, eventsForIssue, reviewsForIssue } from '../../sync/selectors';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { formatKeys } from '../../shortcuts/manager';
import { Avatar, LabelPill } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import {
  CheckCircleIcon,
  CircleSlashIcon,
  CommentIcon,
  CopyIcon,
  EyeIcon,
  FileDiffIcon,
  GitMergeIcon,
  GitPullRequestDraftIcon,
  IssueClosedIcon,
  IssueReopenedIcon,
  KebabHorizontalIcon,
  LockIcon,
  MilestoneIcon,
  PencilIcon,
  PersonIcon,
  SkipIcon,
  TagIcon,
  TrashIcon,
  XCircleFillIcon,
  type Icon,
} from '../../ui/icons';
import { Textarea } from '../../ui/Input';
import { Markdown } from '../../ui/Markdown';
import { Menu } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import styles from './IssueView.module.css';

type Item =
  | { kind: 'comment'; at: string; c: Comment }
  | { kind: 'event'; at: string; e: IssueEvent }
  | { kind: 'review'; at: string; r: Review };

/** Issue / PR conversation: body, merged timeline (comments, events, reviews), composer. */
export const Timeline = observer(function Timeline({
  issue,
  repoFullName,
  footer,
  renderReview,
}: {
  issue: Issue;
  repoFullName: string;
  footer?: ReactNode;
  /** Extra content under a review (PRs: its inline review threads). */
  renderReview?: (review: Review) => ReactNode;
}) {
  const loaded = useIssueDetails(issue.id);
  const items: Item[] = [
    ...commentsForIssue(issue.id).map((c) => ({ kind: 'comment' as const, at: c.createdAt, c })),
    ...eventsForIssue(issue.id).map((e) => ({ kind: 'event' as const, at: e.createdAt, e })),
    ...reviewsForIssue(issue.id)
      .filter((r) => r.submittedAt && r.state !== 'PENDING')
      .map((r) => ({ kind: 'review' as const, at: r.submittedAt!, r })),
  ].sort((a, b) => (a.at < b.at ? -1 : a.at > b.at ? 1 : 0));
  const writable = canWrite(issue.repoId) || issue.authorId === store().viewerId;

  return (
    <div className={styles.timeline}>
      <CommentCard
        authorId={issue.authorId}
        createdAt={issue.createdAt}
        association="OWNER"
        body={issue.body}
        repo={repoFullName}
        isAuthor
        onEdit={writable ? (body) => updateIssue(issue, { body }) : undefined}
      />
      {items.map((it) =>
        it.kind === 'comment' ? (
          <CommentItem key={`c${it.c.id}`} comment={it.c} repo={repoFullName} issueAuthor={issue.authorId} />
        ) : it.kind === 'event' ? (
          <EventItem key={`e${it.e.id}`} event={it.e} />
        ) : (
          <ReviewItem key={`r${it.r.id}`} review={it.r} repo={repoFullName} extra={renderReview?.(it.r)} />
        ),
      )}
      {!loaded && issue.comments > 0 && (
        <div className={styles.card}>
          <div className={styles.cardHeader}>
            <Skeleton width={160} />
          </div>
          <div className={styles.cardBody}>
            <Skeleton width="80%" />
            <Skeleton width="60%" style={{ marginTop: 8 }} />
          </div>
        </div>
      )}
      {footer}
      <Composer issue={issue} repoFullName={repoFullName} />
    </div>
  );
});

const CommentItem = observer(function CommentItem({ comment, repo, issueAuthor }: { comment: Comment; repo: string; issueAuthor: number }) {
  const mine = comment.authorId === store().viewerId;
  return (
    <CommentCard
      authorId={comment.authorId}
      createdAt={comment.createdAt}
      edited={comment.updatedAt !== comment.createdAt}
      association={comment.authorAssociation}
      body={comment.body}
      repo={repo}
      pending={comment.id < 0}
      isAuthor={comment.authorId === issueAuthor}
      reactions={comment.reactions}
      onEdit={mine && comment.id > 0 ? (body) => editComment(comment, body) : undefined}
      onDelete={(mine || canWrite(comment.repoId)) && comment.id > 0 ? () => deleteComment(comment) : undefined}
    />
  );
});

const REACTIONS: Record<string, string> = { '+1': '👍', '-1': '👎', laugh: '😄', hooray: '🎉', confused: '😕', heart: '❤️', rocket: '🚀', eyes: '👀' };

const CommentCard = observer(function CommentCard({
  authorId,
  createdAt,
  edited,
  association,
  body,
  repo,
  pending,
  isAuthor,
  reactions,
  onEdit,
  onDelete,
}: {
  authorId: number;
  createdAt: string;
  edited?: boolean;
  association: string;
  body: string | null | undefined;
  repo: string;
  pending?: boolean;
  isAuthor?: boolean;
  reactions?: Record<string, number | undefined>;
  onEdit?: (body: string) => void;
  onDelete?: () => void;
}) {
  const author = store().get('user', authorId);
  const [editing, setEditing] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  return (
    <div className={styles.item}>
      <Avatar user={author} size={32} />
      <div className={cx(styles.card, isAuthor && styles.cardAuthor, pending && styles.cardPending)}>
        <div className={styles.cardHeader}>
          <strong>{author?.login ?? 'ghost'}</strong>
          <span className={styles.subtle}>
            commented {pending ? 'just now' : <RelativeTime date={createdAt} />}
            {edited && ' · edited'}
          </span>
          <span className={styles.spacer} />
          {association !== 'NONE' && <span className={styles.assoc}>{association === 'OWNER' && isAuthor ? 'Author' : association.toLowerCase()}</span>}
          {(onEdit || onDelete) && (
            <>
              <IconButton ref={menuRef} icon={KebabHorizontalIcon} label="Comment actions" size="sm" onClick={() => setMenuOpen((o) => !o)} />
              <Menu
                open={menuOpen}
                onClose={() => setMenuOpen(false)}
                anchor={menuRef}
                placement="bottom-end"
                items={[
                  { id: 'copy', label: 'Copy text', icon: CopyIcon, onSelect: () => void navigator.clipboard?.writeText(body ?? '') },
                  ...(onEdit ? [{ id: 'edit', label: 'Edit', icon: PencilIcon, onSelect: () => setEditing(body ?? '') }] : []),
                  ...(onDelete ? [{ separator: true as const, id: 's' }, { id: 'delete', label: 'Delete', icon: TrashIcon, danger: true, onSelect: onDelete }] : []),
                ]}
              />
            </>
          )}
        </div>
        <div className={styles.cardBody}>
          {editing !== null ? (
            <MarkdownEditor
              value={editing}
              onChange={setEditing}
              repo={repo}
              autoFocus
              onSubmit={() => {
                onEdit?.(editing);
                setEditing(null);
              }}
              onCancel={() => setEditing(null)}
              submitLabel="Update comment"
            />
          ) : body === undefined ? (
            <>
              <Skeleton width="90%" />
              <Skeleton width="70%" style={{ marginTop: 8 }} />
              <Skeleton width="40%" style={{ marginTop: 8 }} />
            </>
          ) : (
            <Markdown source={body ?? ''} repo={repo} />
          )}
          {reactions && Object.keys(reactions).length > 0 && (
            <div className={styles.reactions}>
              {Object.entries(reactions).map(([k, n]) =>
                n ? (
                  <span key={k} className={styles.reaction}>
                    {REACTIONS[k] ?? k} {n}
                  </span>
                ) : null,
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
});

const EVENT_ICONS: Partial<Record<IssueEvent['event'], Icon>> = {
  labeled: TagIcon,
  unlabeled: TagIcon,
  assigned: PersonIcon,
  unassigned: PersonIcon,
  milestoned: MilestoneIcon,
  demilestoned: MilestoneIcon,
  renamed: PencilIcon,
  closed: IssueClosedIcon,
  reopened: IssueReopenedIcon,
  merged: GitMergeIcon,
  locked: LockIcon,
  unlocked: LockIcon,
  review_requested: EyeIcon,
  review_request_removed: EyeIcon,
  ready_for_review: EyeIcon,
  convert_to_draft: GitPullRequestDraftIcon,
  head_ref_force_pushed: FileDiffIcon,
};

const EventItem = observer(function EventItem({ event }: { event: IssueEvent }) {
  const s = store();
  const actor = s.get('user', event.actorId);
  const d = event.data;
  const who = (id?: number) => <strong>{s.get('user', id)?.login ?? 'someone'}</strong>;
  let text: ReactNode;
  let cls = '';
  switch (event.event) {
    case 'labeled':
    case 'unlabeled':
      text = (
        <>
          {event.event === 'labeled' ? 'added' : 'removed'} <LabelPill label={{ name: d.labelName ?? '?', color: d.labelColor ?? 'cccccc', description: null }} size="sm" />
        </>
      );
      break;
    case 'assigned':
      text = event.actorId === d.assigneeId ? <>self-assigned this</> : <>assigned {who(d.assigneeId)}</>;
      break;
    case 'unassigned':
      text = <>unassigned {who(d.assigneeId)}</>;
      break;
    case 'milestoned':
      text = (
        <>
          added this to the <strong>{d.milestoneTitle}</strong> milestone
        </>
      );
      break;
    case 'demilestoned':
      text = (
        <>
          removed this from the <strong>{d.milestoneTitle}</strong> milestone
        </>
      );
      break;
    case 'renamed':
      text = (
        <>
          changed the title <del>{d.from}</del> {d.to}
        </>
      );
      break;
    case 'closed':
      text = d.stateReason === 'not_planned' ? <>closed this as not planned</> : <>closed this as completed</>;
      cls = d.stateReason === 'not_planned' ? styles.evNeutral! : styles.evClosed!;
      break;
    case 'reopened':
      text = <>reopened this</>;
      cls = styles.evOpen!;
      break;
    case 'merged':
      text = (
        <>
          merged commit <code>{d.commitId?.slice(0, 7)}</code>
        </>
      );
      cls = styles.evClosed!;
      break;
    case 'review_requested':
      text = <>requested a review from {who(d.reviewerId)}</>;
      break;
    case 'ready_for_review':
      text = <>marked this pull request as ready for review</>;
      break;
    case 'convert_to_draft':
      text = <>marked this pull request as draft</>;
      break;
    default:
      text = <>{event.event.replace(/_/g, ' ')}</>;
  }
  const I = event.event === 'closed' && d.stateReason === 'not_planned' ? SkipIcon : (EVENT_ICONS[event.event] ?? CommentIcon);
  return (
    <div className={styles.event}>
      <span className={cx(styles.eventIcon, cls)}>
        <I size={14} />
      </span>
      <Avatar user={actor} size={18} />
      <span className={styles.eventText}>
        <strong>{actor?.login ?? 'ghost'}</strong> {text} <span className={styles.subtle}><RelativeTime date={event.createdAt} /></span>
      </span>
    </div>
  );
});

const ReviewItem = observer(function ReviewItem({ review, repo, extra }: { review: Review; repo: string; extra?: ReactNode }) {
  const author = store().get('user', review.authorId);
  const map = {
    APPROVED: { icon: CheckCircleIcon, text: 'approved these changes', cls: styles.evOpen },
    CHANGES_REQUESTED: { icon: XCircleFillIcon, text: 'requested changes', cls: styles.evDanger },
    COMMENTED: { icon: EyeIcon, text: 'reviewed', cls: '' },
    DISMISSED: { icon: CircleSlashIcon, text: 'had a review dismissed', cls: styles.evNeutral },
    PENDING: { icon: EyeIcon, text: 'started a review', cls: '' },
  } as const;
  const m = map[review.state];
  return (
    <>
      <div className={styles.event}>
        <span className={cx(styles.eventIcon, m.cls)}>
          <m.icon size={14} />
        </span>
        <Avatar user={author} size={18} />
        <span className={styles.eventText}>
          <strong>{author?.login ?? 'ghost'}</strong> {m.text} <span className={styles.subtle}>{review.submittedAt && <RelativeTime date={review.submittedAt} />}</span>
        </span>
      </div>
      {review.body && (
        <div className={styles.reviewBody}>
          <Markdown source={review.body} repo={repo} />
        </div>
      )}
      {extra}
    </>
  );
});

// ------------------------------------------------------------------ composer

export function MarkdownEditor({
  value,
  onChange,
  repo,
  onSubmit,
  onCancel,
  submitLabel,
  placeholder = 'Leave a comment',
  autoFocus,
  textareaRef,
  extraActions,
}: {
  value: string;
  onChange: (v: string) => void;
  repo: string;
  onSubmit: () => void;
  onCancel?: () => void;
  submitLabel: string;
  placeholder?: string;
  autoFocus?: boolean;
  textareaRef?: Ref<HTMLTextAreaElement>;
  extraActions?: ReactNode;
}) {
  const [tab, setTab] = useState('write');
  return (
    <div className={styles.editor}>
      <div className={styles.editorTabs}>
        <Tabs
          size="sm"
          value={tab}
          onChange={setTab}
          items={[
            { id: 'write', label: 'Write' },
            { id: 'preview', label: 'Preview' },
          ]}
        />
        <span className={styles.subtle}>Markdown supported</span>
      </div>
      {tab === 'write' ? (
        <Textarea
          ref={textareaRef}
          value={value}
          autoFocus={autoFocus}
          placeholder={placeholder}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              if (value.trim()) onSubmit();
            } else if (e.key === 'Escape' && onCancel) {
              e.preventDefault();
              onCancel();
            }
          }}
          rows={5}
          aria-label="Comment body"
        />
      ) : (
        <div className={styles.preview}>
          <Markdown source={value || 'Nothing to preview'} repo={repo} />
        </div>
      )}
      <div className={styles.editorActions}>
        {extraActions}
        <span className={styles.spacer} />
        {onCancel && (
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
        )}
        <Button variant="primary" disabled={!value.trim()} onClick={onSubmit} kbd={formatKeys('mod+enter')[0]}>
          {submitLabel}
        </Button>
      </div>
    </div>
  );
}

const Composer = observer(function Composer({ issue, repoFullName }: { issue: Issue; repoFullName: string }) {
  const [body, setBody] = useState('');
  const ref = useRef<HTMLTextAreaElement>(null);
  const me = store().get('user', store().viewerId);
  const writable = canWrite(issue.repoId) || issue.authorId === store().viewerId;
  useShortcuts('Issue', {
    r: {
      handler: () => {
        ref.current?.focus();
        ref.current?.scrollIntoView({ block: 'center', behavior: 'smooth' });
      },
      description: 'Reply',
      group: 'Issue',
    },
  });
  const submit = () => {
    if (!body.trim()) return;
    createComment(issue, body.trim());
    setBody('');
  };
  const canClose = writable && !(issue.isPr && issue.merged);
  return (
    <div className={styles.item}>
      <Avatar user={me} size={32} />
      <div className={styles.composer}>
        <MarkdownEditor
          value={body}
          onChange={setBody}
          repo={repoFullName}
          onSubmit={submit}
          submitLabel="Comment"
          textareaRef={ref}
          extraActions={
            canClose && (
              <Button
                leadingIcon={issue.state === 'open' ? (issue.isPr ? CircleSlashIcon : IssueClosedIcon) : IssueReopenedIcon}
                onClick={() => {
                  if (body.trim()) createComment(issue, body.trim());
                  setBody('');
                  if (issue.state === 'open') closeIssue(issue);
                  else reopenIssue(issue);
                  toast({ kind: 'success', title: issue.state === 'open' ? `Closed #${issue.number}` : `Reopened #${issue.number}` });
                }}
              >
                {issue.state === 'open' ? (body.trim() ? 'Close with comment' : issue.isPr ? 'Close pull request' : 'Close issue') : 'Reopen'}
              </Button>
            )
          }
        />
      </div>
    </div>
  );
});


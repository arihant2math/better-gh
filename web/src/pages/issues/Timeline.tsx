import { observer } from 'mobx-react-lite';
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { ConfirmDialog } from '../../components/ConfirmDialog';
import { MarkdownEditor } from '../../components/editor/MarkdownEditor';
import { Link } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { hasSync, store } from '../../sync';
import { useIssueDetails } from '../../sync/hooks';
import type { Comment, Issue, IssueEvent, Review } from '../../sync/models';
import { closeIssue, createComment, deleteComment, editComment, reopenIssue, updateIssue } from '../../sync/mutations';
import { canWrite, commentsForIssue, eventsForIssue, reviewsForIssue } from '../../sync/selectors';
import { loadViewerReactions } from '../../sync/viewerReactions';
import { Avatar, LabelPill, StateIcon } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import {
  ArrowSwitchIcon,
  BellIcon,
  CheckCircleIcon,
  CircleSlashIcon,
  CommentIcon,
  CopyIcon,
  CrossReferenceIcon,
  DuplicateIcon,
  EyeIcon,
  FileDiffIcon,
  GitCommitIcon,
  GitMergeIcon,
  GitPullRequestDraftIcon,
  IssueClosedIcon,
  IssueReopenedIcon,
  IssueTrackedByIcon,
  IssueTracksIcon,
  KebabHorizontalIcon,
  LinkIcon,
  LockIcon,
  MentionIcon,
  MilestoneIcon,
  PencilIcon,
  PersonIcon,
  PinIcon,
  SkipIcon,
  TagIcon,
  TrashIcon,
  TriangleDownIcon,
  UnlockIcon,
  XCircleFillIcon,
  type Icon,
} from '../../ui/icons';
import { Markdown } from '../../ui/Markdown';
import { Menu } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import styles from './IssueView.module.css';
import { ReactionBar } from './Reactions';

export { MarkdownEditor };

type Item =
  | { kind: 'comment'; at: string; c: Comment }
  | { kind: 'events'; at: string; e: IssueEvent[] }
  | { kind: 'review'; at: string; r: Review };

/** Events GitHub doesn't render in the conversation. */
const HIDDEN_EVENTS = new Set<IssueEvent['event']>(['subscribed']);
const GROUPABLE = new Set<IssueEvent['event']>(['labeled', 'unlabeled', 'assigned', 'unassigned']);
const GROUP_WINDOW_MS = 2 * 60_000;

/** Merge consecutive label/assignee changes by the same actor (GitHub: "added a b and removed c"). */
export function groupEvents(events: IssueEvent[]): IssueEvent[][] {
  const out: IssueEvent[][] = [];
  for (const e of events) {
    const last = out[out.length - 1];
    const prev = last?.[last.length - 1];
    const sameKind = (a: IssueEvent['event'], b: IssueEvent['event']) =>
      (a === 'labeled' || a === 'unlabeled') === (b === 'labeled' || b === 'unlabeled');
    if (
      prev &&
      GROUPABLE.has(e.event) &&
      GROUPABLE.has(prev.event) &&
      sameKind(e.event, prev.event) &&
      prev.actorId === e.actorId &&
      Math.abs(Date.parse(e.createdAt) - Date.parse(prev.createdAt)) <= GROUP_WINDOW_MS
    ) {
      last!.push(e);
    } else {
      out.push([e]);
    }
  }
  return out;
}

function buildItems(issue: Issue): Item[] {
  const raw = [
    ...commentsForIssue(issue.id).map((c) => ({ kind: 'comment' as const, at: c.createdAt, id: c.id, c })),
    ...eventsForIssue(issue.id)
      .filter((e) => !HIDDEN_EVENTS.has(e.event))
      .map((e) => ({ kind: 'event' as const, at: e.createdAt, id: e.id, e })),
    ...reviewsForIssue(issue.id)
      .filter((r) => r.submittedAt && r.state !== 'PENDING')
      .map((r) => ({ kind: 'review' as const, at: r.submittedAt!, id: r.id, r })),
  ].sort((a, b) => (a.at < b.at ? -1 : a.at > b.at ? 1 : Math.abs(a.id) - Math.abs(b.id)));
  const items: Item[] = [];
  let run: IssueEvent[] = [];
  const flush = () => {
    for (const g of groupEvents(run)) items.push({ kind: 'events', at: g[0]!.createdAt, e: g });
    run = [];
  };
  for (const r of raw) {
    if (r.kind === 'event') {
      run.push(r.e);
      continue;
    }
    flush();
    items.push(r.kind === 'comment' ? { kind: 'comment', at: r.at, c: r.c } : { kind: 'review', at: r.at, r: r.r });
  }
  flush();
  return items;
}

/** Issue / PR conversation: body, merged timeline (comments, events, reviews), composer. */
export const Timeline = observer(function Timeline({
  issue,
  repoFullName,
  footer,
  afterBody,
}: {
  issue: Issue;
  repoFullName: string;
  footer?: ReactNode;
  /** Rendered right below the description (sub-issues panel). */
  afterBody?: ReactNode;
}) {
  const loaded = useIssueDetails(issue.id);
  const [owner = '', name = ''] = repoFullName.split('/');
  const { id, number } = issue;
  useEffect(() => {
    if (hasSync() && id > 0) void loadViewerReactions(owner, name, { id, number });
  }, [owner, name, id, number]);
  const items = buildItems(issue);
  const writable = canWrite(issue.repoId) || issue.authorId === store().viewerId;
  const reactDisabled = issue.locked && !canWrite(issue.repoId);

  return (
    <div className={styles.timeline}>
      <CommentCard
        anchorId="issue-body"
        authorId={issue.authorId}
        createdAt={issue.createdAt}
        association="NONE"
        body={issue.body}
        repo={repoFullName}
        repoId={issue.repoId}
        isAuthor
        pending={issue.id < 0}
        reactions={<ReactionBar target={{ issue }} disabled={reactDisabled} />}
        onEdit={writable && issue.id > 0 ? (body) => updateIssue(issue, { body }) : undefined}
      />
      {afterBody}
      {items.map((it) =>
        it.kind === 'comment' ? (
          <CommentItem key={`c${it.c.id}`} comment={it.c} repo={repoFullName} issue={issue} />
        ) : it.kind === 'events' ? (
          <EventItem key={`e${it.e[0]!.id}`} events={it.e} repo={repoFullName} />
        ) : (
          <ReviewItem key={`r${it.r.id}`} review={it.r} repo={repoFullName} />
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

const CommentItem = observer(function CommentItem({ comment, repo, issue }: { comment: Comment; repo: string; issue: Issue }) {
  const mine = comment.authorId === store().viewerId;
  const [confirm, setConfirm] = useState(false);
  const confirmed = comment.id > 0;
  return (
    <>
      <CommentCard
        anchorId={confirmed ? `issuecomment-${comment.id}` : undefined}
        authorId={comment.authorId}
        createdAt={comment.createdAt}
        edited={comment.updatedAt !== comment.createdAt}
        association={comment.authorAssociation}
        body={comment.body}
        repo={repo}
        repoId={comment.repoId}
        pending={!confirmed}
        isAuthor={comment.authorId === issue.authorId}
        reactions={<ReactionBar target={{ comment }} disabled={issue.locked && !canWrite(issue.repoId)} />}
        onEdit={(mine || canWrite(comment.repoId)) && confirmed ? (body) => editComment(comment, body) : undefined}
        onDelete={(mine || canWrite(comment.repoId)) && confirmed ? () => setConfirm(true) : undefined}
      />
      <ConfirmDialog open={confirm} onClose={() => setConfirm(false)} onConfirm={() => deleteComment(comment)} title="Delete comment?">
        Are you sure you want to delete this comment? This can’t be undone.
      </ConfirmDialog>
    </>
  );
});

const CommentCard = observer(function CommentCard({
  anchorId,
  authorId,
  createdAt,
  edited,
  association,
  body,
  repo,
  repoId,
  pending,
  isAuthor,
  reactions,
  onEdit,
  onDelete,
}: {
  anchorId?: string;
  authorId: number;
  createdAt: string;
  edited?: boolean;
  association: string;
  body: string | null | undefined;
  repo: string;
  repoId: number;
  pending?: boolean;
  isAuthor?: boolean;
  reactions?: ReactNode;
  onEdit?: (body: string) => void;
  onDelete?: () => void;
}) {
  const author = store().get('user', authorId);
  const [editing, setEditing] = useState<string | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const copyLink = () => {
    if (!anchorId) return;
    const url = `${location.origin}${location.pathname}#${anchorId}`;
    void navigator.clipboard?.writeText(url);
    toast({ kind: 'success', title: 'Link copied' });
  };
  return (
    <div className={styles.item} id={anchorId}>
      <Avatar user={author} size={32} />
      <div className={cx(styles.card, isAuthor && styles.cardAuthor, pending && styles.cardPending)}>
        <div className={styles.cardHeader}>
          <Link to={`/${author?.login ?? 'ghost'}`} className={styles.authorLink}>
            {author?.login ?? 'ghost'}
          </Link>
          <span className={styles.subtle}>
            commented {pending ? 'just now' : <RelativeTime date={createdAt} />}
            {edited && ' · edited'}
          </span>
          <span className={styles.spacer} />
          {isAuthor && <span className={styles.assoc}>Author</span>}
          {association !== 'NONE' && <span className={styles.assoc}>{association.toLowerCase().replace(/_/g, ' ')}</span>}
          <IconButton ref={menuRef} icon={KebabHorizontalIcon} label="Comment actions" size="sm" onClick={() => setMenuOpen((o) => !o)} disabled={pending} />
          <Menu
            open={menuOpen}
            onClose={() => setMenuOpen(false)}
            anchor={menuRef}
            placement="bottom-end"
            items={[
              ...(anchorId ? [{ id: 'link', label: 'Copy link', icon: LinkIcon, onSelect: copyLink }] : []),
              { id: 'copy', label: 'Copy text', icon: CopyIcon, onSelect: () => void navigator.clipboard?.writeText(body ?? '') },
              ...(onEdit ? [{ id: 'edit', label: 'Edit', icon: PencilIcon, onSelect: () => setEditing(body ?? '') }] : []),
              ...(onDelete ? [{ separator: true as const, id: 's' }, { id: 'delete', label: 'Delete', icon: TrashIcon, danger: true, onSelect: onDelete }] : []),
            ]}
          />
        </div>
        <div className={styles.cardBody}>
          {editing !== null ? (
            <MarkdownEditor
              value={editing}
              onChange={setEditing}
              repo={repo}
              repoId={repoId}
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
          ) : body ? (
            <Markdown source={body} repo={repo} />
          ) : (
            <span className={styles.subtle}>No description provided.</span>
          )}
          {editing === null && reactions}
        </div>
      </div>
    </div>
  );
});

// ------------------------------------------------------------------ events

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
  referenced: GitCommitIcon,
  'cross-referenced': CrossReferenceIcon,
  locked: LockIcon,
  unlocked: UnlockIcon,
  mentioned: MentionIcon,
  subscribed: BellIcon,
  pinned: PinIcon,
  unpinned: PinIcon,
  transferred: ArrowSwitchIcon,
  sub_issue_added: IssueTracksIcon,
  sub_issue_removed: IssueTracksIcon,
  parent_issue_added: IssueTrackedByIcon,
  parent_issue_removed: IssueTrackedByIcon,
  review_requested: EyeIcon,
  review_request_removed: EyeIcon,
  ready_for_review: EyeIcon,
  convert_to_draft: GitPullRequestDraftIcon,
  head_ref_force_pushed: FileDiffIcon,
};

const LOCK_REASONS: Record<string, string> = { 'off-topic': 'off-topic', 'too heated': 'too heated', resolved: 'resolved', spam: 'spam' };

/** Link + title of an issue referenced by id/number (local store first, else `owner/repo#n`). */
const IssueRef = observer(function IssueRef({ id, number, repository, isPr, current }: { id?: number; number?: number; repository?: string; isPr?: boolean; current: string }) {
  const s = store();
  const local = id != null ? s.get('issue', id) : undefined;
  const localRepo = local ? s.get('repo', local.repoId) : undefined;
  const full = localRepo ? `${localRepo.owner}/${localRepo.name}` : repository;
  const n = local?.number ?? number;
  if (!full || n == null) return <span className={styles.subtle}>an issue you can’t see</span>;
  const pr = local?.isPr ?? isPr ?? false;
  const href = `/${full}/${pr ? 'pull' : 'issues'}/${n}`;
  const sameRepo = full.toLowerCase() === current.toLowerCase();
  return (
    <Link to={href} className={styles.ref}>
      {local && <StateIcon issue={local} size={14} />}
      {local && <span className={styles.refTitle}>{local.title}</span>}
      <span className={styles.refNumber}>{sameRepo ? `#${n}` : `${full}#${n}`}</span>
    </Link>
  );
});

function closedVisual(reason: string | undefined): { icon: Icon; cls: string | undefined; text: string } {
  switch (reason) {
    case 'not_planned':
      return { icon: SkipIcon, cls: styles.evNeutral, text: 'closed this as not planned' };
    case 'duplicate':
      return { icon: DuplicateIcon, cls: styles.evNeutral, text: 'closed this as a duplicate' };
    default:
      return { icon: IssueClosedIcon, cls: styles.evClosed, text: 'closed this as completed' };
  }
}

/** One timeline row; `events` is a group of compatible events by one actor. */
export const EventItem = observer(function EventItem({ events, repo }: { events: IssueEvent[]; repo: string }) {
  const s = store();
  const event = events[0]!;
  const actor = s.get('user', event.actorId);
  const d = event.data;
  const who = (id?: number | null) => {
    const u = s.get('user', id);
    return u ? (
      <Link to={`/${u.login}`} className={styles.eventUser}>
        {u.login}
      </Link>
    ) : (
      <strong>someone</strong>
    );
  };
  const sha = (c?: string) => c && <code className={styles.sha}>{c.slice(0, 7)}</code>;
  let text: ReactNode;
  let extra: ReactNode = null;
  let cls: string | undefined;
  let icon: Icon = EVENT_ICONS[event.event] ?? CommentIcon;

  switch (event.event) {
    case 'labeled':
    case 'unlabeled': {
      const pills = (kind: 'labeled' | 'unlabeled') =>
        events
          .filter((e) => e.event === kind)
          .map((e) => <LabelPill key={e.id} label={{ name: e.data.labelName ?? '?', color: e.data.labelColor ?? 'cccccc', description: null }} size="sm" />);
      const added = pills('labeled');
      const removed = pills('unlabeled');
      text = (
        <>
          {added.length > 0 && <>added {added}</>}
          {added.length > 0 && removed.length > 0 && ' and '}
          {removed.length > 0 && <>removed {removed}</>}
          {added.length + removed.length > 1 ? ' labels' : added.length + removed.length === 1 ? ' label' : null}
        </>
      );
      break;
    }
    case 'assigned':
    case 'unassigned': {
      const assigned = events.filter((e) => e.event === 'assigned');
      const unassigned = events.filter((e) => e.event === 'unassigned');
      const selfOnly = (list: IssueEvent[]) => list.length === 1 && list[0]!.data.assigneeId === event.actorId;
      const names = (list: IssueEvent[]) =>
        list.map((e, i) => (
          <span key={e.id}>
            {i > 0 && (i === list.length - 1 ? ' and ' : ', ')}
            {who(e.data.assigneeId)}
          </span>
        ));
      text = (
        <>
          {assigned.length > 0 && (selfOnly(assigned) && !unassigned.length ? 'self-assigned this' : <>assigned {names(assigned)}</>)}
          {assigned.length > 0 && unassigned.length > 0 && ' and '}
          {unassigned.length > 0 && (selfOnly(unassigned) && !assigned.length ? 'removed their assignment' : <>unassigned {names(unassigned)}</>)}
        </>
      );
      break;
    }
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
          changed the title <del>{d.from}</del> <strong className={styles.renamedTo}>{d.to}</strong>
        </>
      );
      break;
    case 'closed': {
      const v = closedVisual(d.stateReason);
      icon = v.icon;
      cls = v.cls;
      text = (
        <>
          {v.text}
          {d.commitId && <> in {sha(d.commitId)}</>}
        </>
      );
      break;
    }
    case 'reopened':
      text = <>reopened this</>;
      cls = styles.evOpen;
      break;
    case 'merged':
      text = <>merged commit {sha(d.commitId)}</>;
      cls = styles.evClosed;
      break;
    case 'referenced':
      text = <>added a commit that referenced this issue {sha(d.commitId)}</>;
      break;
    case 'cross-referenced':
      text = <>mentioned this {d.sourceIsPr ? 'from a pull request' : 'issue'}</>;
      extra = <IssueRef id={d.sourceIssueId} number={d.sourceNumber} repository={d.sourceRepository} isPr={d.sourceIsPr} current={repo} />;
      break;
    case 'locked':
      text = d.lockReason ? (
        <>
          locked as <strong>{LOCK_REASONS[d.lockReason] ?? d.lockReason}</strong> and limited conversation to collaborators
        </>
      ) : (
        <>locked and limited conversation to collaborators</>
      );
      break;
    case 'unlocked':
      text = <>unlocked this conversation</>;
      break;
    case 'mentioned':
      text = <>was mentioned</>;
      break;
    case 'subscribed':
      text = <>subscribed to this issue</>;
      break;
    case 'pinned':
      text = <>pinned this issue</>;
      break;
    case 'unpinned':
      text = <>unpinned this issue</>;
      break;
    case 'transferred':
      text = (
        <>
          transferred this issue from <strong>{d.fromRepository ?? 'another repository'}</strong>
        </>
      );
      break;
    case 'sub_issue_added':
    case 'sub_issue_removed':
      text = <>{event.event === 'sub_issue_added' ? 'added' : 'removed'} a sub-issue</>;
      extra = <IssueRef id={d.subIssueId} number={d.subIssueNumber} repository={d.subIssueRepository} current={repo} />;
      break;
    case 'parent_issue_added':
    case 'parent_issue_removed':
      text = <>{event.event === 'parent_issue_added' ? 'added a parent issue' : 'removed a parent issue'}</>;
      extra = <IssueRef id={d.parentIssueId} number={d.parentIssueNumber} repository={d.parentIssueRepository} current={repo} />;
      break;
    case 'review_requested':
      text = event.actorId === d.reviewerId ? <>self-requested a review</> : <>requested a review from {who(d.reviewerId)}</>;
      break;
    case 'review_request_removed':
      text = <>removed the request for review from {who(d.reviewerId)}</>;
      break;
    case 'ready_for_review':
      text = <>marked this pull request as ready for review</>;
      break;
    case 'convert_to_draft':
      text = <>marked this pull request as draft</>;
      break;
    case 'head_ref_force_pushed':
      text = <>force-pushed the branch {sha(d.commitId)}</>;
      break;
    default:
      text = <>{String(event.event).replace(/_/g, ' ')}</>;
  }
  const I = icon;
  return (
    <div className={styles.event} data-event={event.event}>
      <span className={cx(styles.eventIcon, cls)}>
        <I size={14} />
      </span>
      <Avatar user={actor} size={18} />
      <span className={styles.eventText}>
        {actor ? (
          <Link to={`/${actor.login}`} className={styles.eventUser}>
            {actor.login}
          </Link>
        ) : (
          <strong>ghost</strong>
        )}{' '}
        {text} {extra}{' '}
        <span className={styles.subtle}>
          <RelativeTime date={event.createdAt} />
        </span>
      </span>
    </div>
  );
});

const ReviewItem = observer(function ReviewItem({ review, repo }: { review: Review; repo: string }) {
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
    </>
  );
});

// ------------------------------------------------------------------ composer

const Composer = observer(function Composer({ issue, repoFullName }: { issue: Issue; repoFullName: string }) {
  const [body, setBody] = useState('');
  const ref = useRef<HTMLTextAreaElement | null>(null);
  const me = store().get('user', store().viewerId);
  const collaborator = canWrite(issue.repoId);
  const writable = collaborator || issue.authorId === store().viewerId;
  const blocked = issue.locked && !collaborator;
  useShortcuts('Issue', {
    r: {
      handler: () => {
        if (blocked) return false;
        ref.current?.focus();
        ref.current?.scrollIntoView({ block: 'center', behavior: 'smooth' });
      },
      description: 'Reply',
      group: 'Issue',
    },
  });
  const submit = () => {
    if (!body.trim() || issue.id < 0) return;
    createComment(issue, body.trim());
    setBody('');
  };
  const lockNote = issue.locked ? (
    <div className={styles.lockNote} role="note">
      <LockIcon size={16} />
      <span>
        This conversation has been locked{issue.activeLockReason ? <> as <strong>{issue.activeLockReason}</strong></> : null} and limited to collaborators.
      </span>
    </div>
  ) : null;
  if (blocked) return lockNote;
  const canClose = writable && !(issue.isPr && issue.merged) && issue.id > 0;
  return (
    <>
      {lockNote}
      <div className={styles.item}>
        <Avatar user={me} size={32} />
        <div className={styles.composer}>
          <MarkdownEditor
            value={body}
            onChange={setBody}
            repo={repoFullName}
            repoId={issue.repoId}
            onSubmit={submit}
            submitLabel="Comment"
            submitDisabled={issue.id < 0}
            textareaRef={ref}
            extraActions={canClose && <CloseButton issue={issue} body={body} onDone={() => setBody('')} />}
          />
        </div>
      </div>
    </>
  );
});

/** "Close issue" with a reason menu (completed / not planned), or "Reopen". */
const CloseButton = observer(function CloseButton({ issue, body, onDone }: { issue: Issue; body: string; onDone: () => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  const withComment = () => {
    if (body.trim()) createComment(issue, body.trim());
    onDone();
  };
  if (issue.state === 'closed') {
    return (
      <Button
        leadingIcon={IssueReopenedIcon}
        onClick={() => {
          withComment();
          reopenIssue(issue);
          toast({ kind: 'success', title: `Reopened #${issue.number}` });
        }}
      >
        {body.trim() ? 'Reopen with comment' : `Reopen ${issue.isPr ? 'pull request' : 'issue'}`}
      </Button>
    );
  }
  const close = (reason: 'completed' | 'not_planned') => {
    withComment();
    closeIssue(issue, reason);
    toast({ kind: 'success', title: `Closed #${issue.number}${reason === 'not_planned' ? ' as not planned' : ''}` });
  };
  if (issue.isPr) {
    return (
      <Button leadingIcon={CircleSlashIcon} onClick={() => close('completed')}>
        {body.trim() ? 'Close with comment' : 'Close pull request'}
      </Button>
    );
  }
  return (
    <span className={styles.splitButton}>
      <Button leadingIcon={IssueClosedIcon} onClick={() => close('completed')}>
        {body.trim() ? 'Close with comment' : 'Close issue'}
      </Button>
      <IconButton ref={ref} icon={TriangleDownIcon} variant="secondary" label="Close with reason" aria-expanded={open} onClick={() => setOpen((o) => !o)} />
      <Menu
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        placement="top-end"
        aria-label="Close reasons"
        items={[
          { id: 'completed', label: 'Close as completed', description: 'Done, closed, fixed, resolved', icon: IssueClosedIcon, onSelect: () => close('completed') },
          { id: 'not_planned', label: 'Close as not planned', description: 'Won’t fix, can’t repro, stale', icon: SkipIcon, onSelect: () => close('not_planned') },
        ]}
      />
    </span>
  );
});

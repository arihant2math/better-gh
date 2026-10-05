import { observer } from 'mobx-react-lite';
import { createContext, useContext, useRef, useState } from 'react';
import { store } from '../../sync';
import type { Issue, ReactionContent, ReviewComment } from '../../sync/models';
import { commitSuggestion, deleteReviewComment, editReviewComment, replyToThread, setThreadResolved, toggleReviewCommentReaction } from '../../sync/pullMutations';
import { isPendingComment, type ReviewThread as Thread } from '../../sync/pullSelectors';
import { viewerReactions } from '../../sync/viewerReactions';
import { canWrite } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { CheckIcon, CopyIcon, FileIcon, KebabHorizontalIcon, PencilIcon, SmileyIcon, TrashIcon, UnfoldIcon } from '../../ui/icons';
import { Markdown } from '../../ui/Markdown';
import { Menu } from '../../ui/Menu';
import { Popover } from '../../ui/Popover';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import { MarkdownEditor } from '../issues/Timeline';
import styles from './Review.module.css';

/**
 * Lookup of the current text of `path` lines `start..end` (RIGHT side) for
 * rendering ```suggestion blocks as diffs. Provided by the Files tab from
 * the loaded patches; falls back to the comment's diff hunk.
 */
export type LineSource = (path: string, start: number, end: number) => string[] | null;
export const LineSourceContext = createContext<LineSource | null>(null);

const SUGGESTION_RE = /```suggestion[^\n]*\n([\s\S]*?)```/g;

export function splitSuggestions(body: string): ({ kind: 'md'; text: string } | { kind: 'suggestion'; text: string })[] {
  const out: ({ kind: 'md'; text: string } | { kind: 'suggestion'; text: string })[] = [];
  let last = 0;
  for (const m of body.matchAll(SUGGESTION_RE)) {
    if (m.index! > last) out.push({ kind: 'md', text: body.slice(last, m.index) });
    out.push({ kind: 'suggestion', text: m[1]! });
    last = m.index! + m[0].length;
  }
  if (last < body.length) out.push({ kind: 'md', text: body.slice(last) });
  return out;
}

/** The last `n` added/context lines of a diff hunk (what the comment points at). */
function hunkTail(hunk: string | undefined, n: number): string[] | null {
  if (!hunk) return null;
  const lines = hunk
    .split('\n')
    .slice(1)
    .filter((l) => !l.startsWith('-') && !l.startsWith('\\'))
    .map((l) => l.slice(1));
  return lines.length >= n ? lines.slice(lines.length - n) : null;
}

const REACTIONS: [ReactionContent, string][] = [
  ['+1', '👍'],
  ['-1', '👎'],
  ['laugh', '😄'],
  ['hooray', '🎉'],
  ['confused', '😕'],
  ['heart', '❤️'],
  ['rocket', '🚀'],
  ['eyes', '👀'],
];

/** An inline review thread: comments, reply box, resolve / unresolve, suggestions. */
export const ReviewThreadView = observer(function ReviewThreadView({
  thread,
  pr,
  repo,
  showPath = false,
  defaultCollapsed,
}: {
  thread: Thread;
  pr: Issue;
  repo: string;
  /** Show the file path + diff excerpt (Conversation tab). */
  showPath?: boolean;
  defaultCollapsed?: boolean;
}) {
  const [collapsed, setCollapsed] = useState(defaultCollapsed ?? thread.resolved);
  const [reply, setReply] = useState<string | null>(null);
  const viewer = store().viewerId;
  const canResolve = !thread.pending && thread.root.id > 0 && (canWrite(pr.repoId) || pr.authorId === viewer);
  const resolver = store().get('user', thread.root.resolvedById);
  const lines = thread.root.line ?? thread.root.originalLine;
  const range = thread.root.startLine ?? thread.root.originalStartLine;

  return (
    <div className={cx(styles.thread, thread.resolved && styles.resolved)} data-thread={thread.id}>
      {(showPath || thread.resolved || thread.outdated) && (
        <button type="button" className={styles.threadHeader} onClick={() => setCollapsed((c) => !c)} aria-expanded={!collapsed}>
          {showPath && (
            <>
              <FileIcon size={14} />
              <span className={styles.threadPath}>{thread.path}</span>
              {lines != null && (
                <span className={styles.subtle}>
                  {range != null && range !== lines ? `lines ${range}–${lines}` : `line ${lines}`}
                </span>
              )}
            </>
          )}
          {thread.outdated && <span className={styles.chip}>Outdated</span>}
          {thread.pending && <span className={cx(styles.chip, styles.chipPending)}>Pending</span>}
          {thread.resolved && (
            <span className={styles.subtle}>
              <CheckIcon size={14} /> {resolver ? `${resolver.login} marked this conversation as resolved.` : 'Resolved'}
            </span>
          )}
          <span className={styles.spacer} />
          <UnfoldIcon size={14} />
          <span className={styles.subtle}>{collapsed ? `Show ${thread.comments.length} comment${thread.comments.length === 1 ? '' : 's'}` : 'Hide'}</span>
        </button>
      )}
      {!collapsed && (
        <>
          {showPath && thread.root.diffHunk && <HunkExcerpt hunk={thread.root.diffHunk} />}
          <div className={styles.comments}>
            {thread.comments.map((c) => (
              <ReviewCommentView key={c.id} comment={c} pr={pr} repo={repo} />
            ))}
          </div>
          <div className={styles.threadFooter}>
            {reply === null ? (
              <>
                <button type="button" className={styles.replyStub} onClick={() => setReply('')}>
                  <Avatar user={store().get('user', viewer)} size={20} />
                  Reply…
                </button>
                {canResolve && (
                  <Button size="sm" onClick={() => setThreadResolved(pr, thread.root, !thread.resolved)}>
                    {thread.resolved ? 'Unresolve conversation' : 'Resolve conversation'}
                  </Button>
                )}
              </>
            ) : (
              <div className={styles.replyBox}>
                <MarkdownEditor
                  value={reply}
                  onChange={setReply}
                  repo={repo}
                  autoFocus
                  placeholder="Reply…"
                  submitLabel={store().byIndex('review', 'issueId', pr.id).some((r) => r.state === 'PENDING' && r.authorId === viewer) ? 'Add review comment' : 'Reply'}
                  onCancel={() => setReply(null)}
                  onSubmit={() => {
                    replyToThread(pr, thread.root, reply.trim());
                    setReply(null);
                  }}
                />
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
});

function HunkExcerpt({ hunk }: { hunk: string }) {
  const lines = hunk.split('\n');
  const shown = lines.slice(Math.max(1, lines.length - 4));
  return (
    <div className={styles.excerpt}>
      {shown.map((l, i) => (
        <div key={i} className={cx(styles.excerptLine, l.startsWith('+') && styles.exAdd, l.startsWith('-') && styles.exDel)}>
          {l}
        </div>
      ))}
    </div>
  );
}

export const ReviewCommentView = observer(function ReviewCommentView({ comment: c, pr, repo }: { comment: ReviewComment; pr: Issue; repo: string }) {
  const author = store().get('user', c.authorId);
  const viewer = store().viewerId;
  const mine = c.authorId === viewer;
  const pending = isPendingComment(c);
  const [editing, setEditing] = useState<string | null>(null);
  const [menu, setMenu] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const writable = canWrite(c.repoId);
  return (
    <div className={cx(styles.comment, c.id < 0 && styles.optimistic)} id={`discussion_r${c.id}`}>
      <Avatar user={author} size={24} />
      <div className={styles.commentMain}>
        <div className={styles.commentHeader}>
          <strong>{author?.login ?? 'ghost'}</strong>
          <span className={styles.subtle}>
            <RelativeTime date={c.createdAt} />
            {c.updatedAt !== c.createdAt && ' · edited'}
          </span>
          {pending && <span className={cx(styles.chip, styles.chipPending)}>Pending</span>}
          {c.authorId === pr.authorId && <span className={styles.chip}>Author</span>}
          <span className={styles.spacer} />
          {c.id > 0 && (
            <>
              <IconButton ref={menuRef} icon={KebabHorizontalIcon} label="Comment actions" size="sm" onClick={() => setMenu((o) => !o)} />
              <Menu
                open={menu}
                onClose={() => setMenu(false)}
                anchor={menuRef}
                placement="bottom-end"
                items={[
                  { id: 'copy', label: 'Copy link', icon: CopyIcon, onSelect: () => void navigator.clipboard?.writeText(`${location.origin}${location.pathname}#discussion_r${c.id}`) },
                  ...(mine ? [{ id: 'edit', label: 'Edit', icon: PencilIcon, onSelect: () => setEditing(c.body) }] : []),
                  ...(mine || writable ? [{ separator: true as const, id: 's' }, { id: 'delete', label: 'Delete', icon: TrashIcon, danger: true, onSelect: () => deleteReviewComment(c) }] : []),
                ]}
              />
            </>
          )}
        </div>
        {editing !== null ? (
          <MarkdownEditor
            value={editing}
            onChange={setEditing}
            repo={repo}
            autoFocus
            submitLabel="Update comment"
            onCancel={() => setEditing(null)}
            onSubmit={() => {
              editReviewComment(c, editing);
              setEditing(null);
            }}
          />
        ) : (
          <CommentBody comment={c} pr={pr} repo={repo} pending={pending} />
        )}
        {c.id > 0 && !pending && <Reactions comment={c} />}
      </div>
    </div>
  );
});

const CommentBody = observer(function CommentBody({ comment: c, pr, repo, pending }: { comment: ReviewComment; pr: Issue; repo: string; pending: boolean }) {
  const parts = splitSuggestions(c.body);
  const source = useContext(LineSourceContext);
  if (parts.length === 1 && parts[0]!.kind === 'md') return <Markdown source={c.body} repo={repo} />;
  const end = c.line ?? c.originalLine ?? 0;
  const start = c.startLine ?? c.originalStartLine ?? end;
  const original = (c.side !== 'LEFT' && source?.(c.path, start, end)) || hunkTail(c.diffHunk, end - start + 1);
  return (
    <div>
      {parts.map((p, i) =>
        p.kind === 'md' ? (
          p.text.trim() && <Markdown key={i} source={p.text} repo={repo} />
        ) : (
          <Suggestion key={i} text={p.text} original={original} comment={c} pr={pr} pending={pending} />
        ),
      )}
    </div>
  );
});

const Suggestion = observer(function Suggestion({ text, original, comment, pr, pending }: { text: string; original: string[] | null; comment: ReviewComment; pr: Issue; pending: boolean }) {
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const added = text.replace(/\n$/, '').split('\n');
  const writable = canWrite(pr.repoId) || pr.authorId === store().viewerId;
  const applicable = writable && pr.state === 'open' && !pr.merged && !comment.outdated && !pending && comment.line != null && comment.side !== 'LEFT';
  return (
    <div className={styles.suggestion}>
      <div className={styles.suggestionHeader}>
        <span>Suggested change</span>
        <span className={styles.spacer} />
        {applicable && (
          <Button
            size="sm"
            loading={busy}
            disabled={done}
            onClick={() => {
              setBusy(true);
              commitSuggestion(pr, comment, text).then(
                () => {
                  setDone(true);
                  toast({ kind: 'success', title: 'Suggestion committed' });
                },
                (e: unknown) => toast({ kind: 'error', title: 'Couldn’t commit the suggestion', description: e instanceof Error ? e.message : undefined }),
              ).finally(() => setBusy(false));
            }}
          >
            {done ? 'Committed' : 'Commit suggestion'}
          </Button>
        )}
      </div>
      <div className={styles.suggestionBody}>
        {(original ?? []).map((l, i) => (
          <div key={`d${i}`} className={cx(styles.excerptLine, styles.exDel)}>
            -{l}
          </div>
        ))}
        {added.map((l, i) => (
          <div key={`a${i}`} className={cx(styles.excerptLine, styles.exAdd)}>
            +{l}
          </div>
        ))}
      </div>
    </div>
  );
});

const Reactions = observer(function Reactions({ comment }: { comment: ReviewComment }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  const mine = viewerReactions({ kind: 'reviewComment', id: comment.id });
  const groups = REACTIONS.map(([k, e]) => ({ k, e, n: comment.reactions?.[k] ?? 0, mine: mine.includes(k) })).filter((g) => g.n > 0);
  return (
    <div className={styles.reactions}>
      {groups.map((g) => (
        <button key={g.k} type="button" className={cx(styles.reaction, g.mine && styles.reactionMine)} onClick={() => toggleReviewCommentReaction(comment, g.k)} aria-pressed={g.mine}>
          {g.e} {g.n}
        </button>
      ))}
      <IconButton ref={ref} icon={SmileyIcon} label="Add reaction" size="sm" onClick={() => setOpen((o) => !o)} />
      <Popover open={open} onClose={() => setOpen(false)} anchor={ref} placement="top-start" className={styles.reactionPicker}>
        {REACTIONS.map(([k, e]) => (
          <button
            key={k}
            type="button"
            className={styles.reactionPick}
            aria-label={k}
            onClick={() => {
              toggleReviewCommentReaction(comment, k);
              setOpen(false);
            }}
          >
            {e}
          </button>
        ))}
      </Popover>
    </div>
  );
});

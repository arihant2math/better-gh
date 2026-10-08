/**
 * Commit comments (P32): the general thread under a commit and inline
 * threads on new-side diff lines. Not synced: the list is a cached REST
 * resource (`api/commitComments`) patched in place after each write.
 * Reuses the PR review-thread look (`pulls/Review.module.css`).
 */
import { observable, runInAction } from 'mobx';
import { observer } from 'mobx-react-lite';
import { Suspense, lazy, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { mutate, useResource } from '../../api/cache';
import {
  COMMIT_REACTIONS,
  commitCommentKeys,
  createCommitComment,
  deleteCommitComment,
  listCommitComments,
  toggleCommitCommentReaction,
  updateCommitComment,
  type CommitComment,
  type CommitReactionContent,
} from '../../api/commitComments';
import { errorMessage } from '../../api/errors';
import { minimizedStates, setMinimizedRest } from '../../api/moderation';
import { session } from '../../app/session';
import { ConfirmDialog } from '../../components/ConfirmDialog';
import type { DiffAnnotations, LineSelection } from '../../components/diff/DiffView';
import { MarkdownEditor } from '../../components/editor/MarkdownEditor';
import type { MinimizedReason, Repo } from '../../sync/models';
import { canAdmin, reasonLabel } from '../../sync/moderation';
import { canTriage, canWrite } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { IconButton, cx } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { CopyIcon, EyeClosedIcon, EyeIcon, KebabHorizontalIcon, PencilIcon, SmileyIcon, TrashIcon, TriangleDownIcon } from '../../ui/icons';
import { Markdown } from '../../ui/Markdown';
import { Menu } from '../../ui/Menu';
import { Popover } from '../../ui/Popover';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import review from '../pulls/Review.module.css';
import styles from './CommitComments.module.css';
import { onReset, resettableSet, sameSession } from '../../api/reset';

const EMOJI: Record<CommitReactionContent, string> = {
  '+1': '👍',
  '-1': '👎',
  laugh: '😄',
  hooray: '🎉',
  confused: '😕',
  heart: '❤️',
  rocket: '🚀',
  eyes: '👀',
};

// Moderation (P42): lazily loaded edit history / hide dialog.
const EditHistory = lazy(() => import('../issues/Moderation').then((m) => ({ default: m.EditHistory })));
const HideDialog = lazy(() => import('../issues/Moderation').then((m) => ({ default: m.HideDialog })));

/** Hidden states of loaded commit comments (not part of GitHub's REST shape). */
const hiddenReasons = observable.map<number, string>();
onReset(() => runInAction(() => hiddenReasons.clear()));

function useHiddenStates(repo: Repo, comments: CommitComment[] | undefined) {
  const ids = (comments ?? []).map((c) => c.id).join(',');
  useEffect(() => {
    if (!ids) return;
    const list = ids.split(',').map(Number);
    const live = sameSession();
    minimizedStates(repo.owner, repo.name, 'commit_comment', list).then(
      (states) =>
        live() &&
        runInAction(() => {
          for (const id of list) hiddenReasons.delete(id);
          for (const st of states) if (st.minimizedReason) hiddenReasons.set(st.id, st.minimizedReason);
        }),
      () => undefined,
    );
  }, [repo.owner, repo.name, ids]);
}

async function setHidden(repo: Repo, c: CommitComment, reason: MinimizedReason | null) {
  try {
    await setMinimizedRest(repo.owner, repo.name, 'commit_comment', c.id, reason);
    runInAction(() => {
      if (reason) hiddenReasons.set(c.id, reason);
      else hiddenReasons.delete(c.id);
    });
  } catch (e) {
    toast({ kind: 'error', title: reason ? 'Couldn’t hide the comment' : 'Couldn’t unhide the comment', description: errorMessage(e) });
  }
}

/** Reactions the viewer toggled this session (`{id}:{content}`); the REST list has no "viewer reacted" flag. */
const myReactions = resettableSet<string>();

function useComments(repo: Repo, sha: string | undefined) {
  const key = sha ? commitCommentKeys.forCommit(repo.owner, repo.name, sha) : null;
  const res = useResource<CommitComment[]>(key, () => listCommitComments(repo.owner, repo.name, sha!));
  useHiddenStates(repo, res.data);
  return { key, ...res };
}

function patchList(key: string, fn: (list: CommitComment[]) => CommitComment[]) {
  mutate<CommitComment[]>(key, (prev) => fn(prev ?? []));
}

/** Writes against one commit's cached comment list. */
function actions(repo: Repo, sha: string, key: string) {
  const { owner: o, name: r } = repo;
  return {
    create: async (input: { body: string; path?: string; line?: number }) => {
      try {
        const c = await createCommitComment(o, r, sha, input);
        patchList(key, (l) => (l.some((x) => x.id === c.id) ? l : [...l, c]));
        return true;
      } catch (e) {
        toast({ kind: 'error', title: 'Couldn’t add the comment', description: errorMessage(e) });
        return false;
      }
    },
    edit: async (c: CommitComment, body: string) => {
      try {
        const next = await updateCommitComment(o, r, c.id, body);
        patchList(key, (l) => l.map((x) => (x.id === c.id ? next : x)));
        return true;
      } catch (e) {
        toast({ kind: 'error', title: 'Couldn’t update the comment', description: errorMessage(e) });
        return false;
      }
    },
    remove: async (c: CommitComment) => {
      const before = c;
      patchList(key, (l) => l.filter((x) => x.id !== c.id));
      try {
        await deleteCommitComment(o, r, c.id);
      } catch (e) {
        patchList(key, (l) => [...l, before].sort((a, b) => a.created_at.localeCompare(b.created_at) || a.id - b.id));
        toast({ kind: 'error', title: 'Couldn’t delete the comment', description: errorMessage(e) });
      }
    },
    react: async (c: CommitComment, content: CommitReactionContent) => {
      const k = `${c.id}:${content}`;
      const bump = (d: number) =>
        patchList(key, (l) =>
          l.map((x) => {
            if (x.id !== c.id || !x.reactions) return x;
            return { ...x, reactions: { ...x.reactions, [content]: Math.max(0, x.reactions[content] + d), total_count: Math.max(0, x.reactions.total_count + d) } };
          }),
        );
      const guess = !myReactions.has(k);
      // Optimistic guess from what we know; corrected by the server's answer.
      if (guess) myReactions.add(k);
      else myReactions.delete(k);
      bump(guess ? 1 : -1);
      try {
        const on = await toggleCommitCommentReaction(o, r, c.id, content);
        if (on !== guess) {
          // We reacted before this session (or the guess was wrong): undo the
          // optimistic bump and apply the real change (the toggle removed it).
          if (on) myReactions.add(k);
          else myReactions.delete(k);
          bump(on ? 2 : -2);
        }
      } catch (e) {
        if (guess) myReactions.delete(k);
        else myReactions.add(k);
        bump(guess ? -1 : 1);
        toast({ kind: 'error', title: 'Couldn’t update the reaction', description: errorMessage(e) });
      }
    },
  };
}
type Actions = ReturnType<typeof actions>;

// ------------------------------------------------------------------ single comment

const CommentItem = observer(function CommentItem({ comment: c, repo, act, compact }: { comment: CommitComment; repo: Repo; act: Actions; compact?: boolean }) {
  const viewer = session.user;
  const mine = !!viewer && c.user?.id === viewer.id;
  const writable = !!viewer && canWrite(repo.id);
  const [editing, setEditing] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [menu, setMenu] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const full = `${repo.owner}/${repo.name}`;
  const anchor = `commitcomment-${c.id}`;
  const user = c.user ? { login: c.user.login, avatarUrl: c.user.avatar_url } : null;
  const hiddenReason = hiddenReasons.get(c.id);
  const [showHidden, setShowHidden] = useState(false);
  const [hiding, setHiding] = useState(false);
  const [historyOpen, setHistoryOpen] = useState(false);
  const historyRef = useRef<HTMLButtonElement>(null);
  const collapsed = !!hiddenReason && !showHidden && editing === null;
  const triage = !!viewer && canTriage(repo.id);
  return (
    <div className={cx(review.comment, !compact && styles.timelineComment)} id={anchor} data-comment-id={c.id}>
      <Avatar user={user} size={compact ? 24 : 32} />
      <div className={review.commentMain}>
        <div className={review.commentHeader}>
          <strong>{c.user?.login ?? 'ghost'}</strong>
          <span className={review.subtle}>
            <a href={`#${anchor}`} className={styles.timeLink}>
              <RelativeTime date={c.created_at} />
            </a>
          </span>
          {c.updated_at !== c.created_at && (
            <button ref={historyRef} type="button" className={review.editedButton} aria-expanded={historyOpen} aria-haspopup="dialog" onClick={() => setHistoryOpen((o) => !o)}>
              edited <TriangleDownIcon size={12} />
            </button>
          )}
          {hiddenReason && (
            <span className={review.minimizedNote}>
              · Marked as {reasonLabel(hiddenReason)}.{' '}
              <button type="button" className={review.linkButton} onClick={() => setShowHidden((v) => !v)} aria-expanded={!collapsed}>
                {collapsed ? 'Show comment' : 'Hide comment'}
              </button>
            </span>
          )}
          {c.author_association && c.author_association !== 'NONE' && <span className={review.chip}>{c.author_association.toLowerCase()}</span>}
          <span className={review.spacer} />
          <IconButton ref={menuRef} icon={KebabHorizontalIcon} label="Comment actions" size="sm" onClick={() => setMenu((o) => !o)} />
          <Menu
            open={menu}
            onClose={() => setMenu(false)}
            anchor={menuRef}
            placement="bottom-end"
            items={[
              { id: 'copy', label: 'Copy link', icon: CopyIcon, onSelect: () => void navigator.clipboard?.writeText(`${location.origin}${location.pathname}#${anchor}`) },
              ...(mine ? [{ id: 'edit', label: 'Edit', icon: PencilIcon, onSelect: () => setEditing(c.body) }] : []),
              ...(triage
                ? [
                    hiddenReason
                      ? { id: 'unhide', label: 'Unhide', icon: EyeIcon, onSelect: () => void setHidden(repo, c, null) }
                      : { id: 'hide', label: 'Hide', icon: EyeClosedIcon, onSelect: () => setHiding(true) },
                  ]
                : []),
              ...(mine || writable ? [{ separator: true as const, id: 's' }, { id: 'delete', label: 'Delete', icon: TrashIcon, danger: true, onSelect: () => setConfirm(true) }] : []),
            ]}
          />
        </div>
        {historyOpen && (
          <Suspense fallback={null}>
            <EditHistory
              target={{ owner: repo.owner, repo: repo.name, kind: 'commit_comment', id: c.id, canDelete: mine || canAdmin(repo.id) }}
              anchor={historyRef}
              onClose={() => setHistoryOpen(false)}
              authorLogin={c.user?.login ?? 'ghost'}
              createdAt={c.created_at}
            />
          </Suspense>
        )}
        {hiding && (
          <Suspense fallback={null}>
            <HideDialog onClose={() => setHiding(false)} onHide={(reason) => void setHidden(repo, c, reason)} />
          </Suspense>
        )}
        {collapsed ? null : editing !== null ? (
          <MarkdownEditor
            value={editing}
            onChange={setEditing}
            repo={full}
            repoId={repo.id}
            autoFocus
            submitLabel="Update comment"
            submitDisabled={saving || !editing.trim()}
            onCancel={() => setEditing(null)}
            onSubmit={() => {
              setSaving(true);
              void act.edit(c, editing.trim()).then((ok) => {
                setSaving(false);
                if (ok) setEditing(null);
              });
            }}
          />
        ) : (
          <Markdown source={c.body} repo={full} />
        )}
        {!collapsed && <Reactions comment={c} act={act} signedIn={!!viewer} />}
      </div>
      <ConfirmDialog open={confirm} onClose={() => setConfirm(false)} onConfirm={() => void act.remove(c)} title="Delete comment?">
        Are you sure you want to delete this comment?
      </ConfirmDialog>
    </div>
  );
});

function Reactions({ comment: c, act, signedIn }: { comment: CommitComment; act: Actions; signedIn: boolean }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  const groups = COMMIT_REACTIONS.map((k) => ({ k, n: c.reactions?.[k] ?? 0, mine: myReactions.has(`${c.id}:${k}`) })).filter((g) => g.n > 0);
  if (!signedIn && groups.length === 0) return null;
  return (
    <div className={review.reactions}>
      {groups.map((g) => (
        <button
          key={g.k}
          type="button"
          className={cx(review.reaction, g.mine && review.reactionMine)}
          disabled={!signedIn}
          onClick={() => void act.react(c, g.k)}
          aria-pressed={g.mine}
          aria-label={`${g.k} (${g.n})`}
        >
          {EMOJI[g.k]} {g.n}
        </button>
      ))}
      {signedIn && (
        <>
          <IconButton ref={ref} icon={SmileyIcon} label="Add reaction" size="sm" onClick={() => setOpen((o) => !o)} />
          <Popover open={open} onClose={() => setOpen(false)} anchor={ref} placement="top-start" className={review.reactionPicker}>
            {COMMIT_REACTIONS.map((k) => (
              <button
                key={k}
                type="button"
                className={review.reactionPick}
                aria-label={k}
                onClick={() => {
                  void act.react(c, k);
                  setOpen(false);
                }}
              >
                {EMOJI[k]}
              </button>
            ))}
          </Popover>
        </>
      )}
    </div>
  );
}

/** Textarea + submit; clears on success. */
function Composer({
  repo,
  label,
  placeholder = 'Leave a comment',
  autoFocus,
  onSubmit,
  onCancel,
}: {
  repo: Repo;
  label: string;
  placeholder?: string;
  autoFocus?: boolean;
  onSubmit: (body: string) => Promise<boolean>;
  onCancel?: () => void;
}) {
  const [draft, setDraft] = useState('');
  const [busy, setBusy] = useState(false);
  return (
    <MarkdownEditor
      value={draft}
      onChange={setDraft}
      repo={`${repo.owner}/${repo.name}`}
      repoId={repo.id}
      autoFocus={autoFocus}
      placeholder={placeholder}
      ariaLabel={placeholder}
      submitLabel={label}
      submitDisabled={busy || !draft.trim()}
      onCancel={onCancel}
      onSubmit={() => {
        if (!draft.trim() || busy) return;
        setBusy(true);
        void onSubmit(draft.trim()).then((ok) => {
          setBusy(false);
          if (ok) setDraft('');
        });
      }}
    />
  );
}

// ------------------------------------------------------------------ inline (diff) threads

function InlineThread({ comments, repo, act, onReply }: { comments: CommitComment[]; repo: Repo; act: Actions; onReply?: () => void }) {
  return (
    <div className={review.thread}>
      <div className={review.comments}>
        {comments.map((c) => (
          <CommentItem key={c.id} comment={c} repo={repo} act={act} compact />
        ))}
      </div>
      {onReply && (
        <div className={review.threadFooter}>
          <button type="button" className={review.replyStub} onClick={onReply}>
            <Avatar user={session.user} size={20} />
            Reply…
          </button>
        </div>
      )}
    </div>
  );
}

const anchorOf = (c: CommitComment) => (c.line != null ? `R${c.line}` : 'file');

/**
 * Diff annotations for a commit: inline comment threads under new-side
 * lines (`path` + `line`), and a composer on gutter clicks (signed in).
 */
export function useCommitCommentAnnotations(repo: Repo, sha: string | undefined): DiffAnnotations | undefined {
  const { key, data } = useComments(repo, sha);
  const [selection, setSelection] = useState<LineSelection | null>(null);
  const signedIn = !!session.user;
  const byFile = useMemo(() => {
    const m = new Map<string, Map<string, CommitComment[]>>();
    for (const c of data ?? []) {
      if (c.path == null) continue;
      let f = m.get(c.path);
      if (!f) m.set(c.path, (f = new Map()));
      const a = anchorOf(c);
      f.set(a, [...(f.get(a) ?? []), c]);
    }
    return m;
  }, [data]);

  return useMemo(() => {
    if (!sha || !key) return undefined;
    const act = actions(repo, sha, key);
    const selAnchor = selection ? `R${selection.end}` : null;
    const composer = (sel: LineSelection) => (
      <div className={review.composer}>
        <div className={review.composerLabel}>Comment on line R{sel.end}</div>
        <Composer
          repo={repo}
          label="Comment"
          autoFocus
          onCancel={() => setSelection(null)}
          onSubmit={async (body) => {
            const ok = await act.create({ body, path: sel.path, line: sel.end });
            if (ok) setSelection(null);
            return ok;
          }}
        />
      </div>
    );
    const out: DiffAnnotations = {
      anchors: (path) => {
        const keys = [...(byFile.get(path)?.keys() ?? [])];
        if (selection && selection.path === path && selAnchor && !keys.includes(selAnchor)) keys.push(selAnchor);
        return keys;
      },
      render: (path, anchor) => {
        const list = byFile.get(path)?.get(anchor) ?? [];
        const line = anchor.startsWith('R') ? Number(anchor.slice(1)) : null;
        const composing = !!selection && selection.path === path && anchor === selAnchor;
        return (
          <>
            {list.length > 0 && (
              <InlineThread
                comments={list}
                repo={repo}
                act={act}
                onReply={signedIn && line != null && !composing ? () => setSelection({ path, side: 'RIGHT', start: line, end: line }) : undefined}
              />
            )}
            {composing && composer(selection)}
          </>
        );
      },
      selection,
      sides: ['RIGHT'],
      commentCount: (path) => {
        let n = 0;
        for (const l of byFile.get(path)?.values() ?? []) n += l.length;
        return n;
      },
    };
    if (signedIn) {
      out.onSelect = (sel) => {
        if (sel.side !== 'RIGHT') return;
        // Commit comments anchor to one line: use the last line of a drag.
        setSelection({ path: sel.path, side: 'RIGHT', start: sel.end, end: sel.end });
      };
    }
    return out;
  }, [repo, sha, key, byFile, selection, signedIn]);
}

// ------------------------------------------------------------------ general thread

/** "N comments on commit abc1234", the general (non-line) comments and the composer. */
export const CommitCommentThread = observer(function CommitCommentThread({ repo, sha }: { repo: Repo; sha: string | undefined }) {
  const { key, data, error } = useComments(repo, sha);
  const signedIn = !!session.user;
  if (!sha || !key) return null;
  const act = actions(repo, sha, key);
  const general = (data ?? []).filter((c) => c.path == null);
  const total = data?.length ?? 0;
  let body: ReactNode;
  if (data === undefined && error) body = <div className={styles.muted}>Couldn’t load comments.</div>;
  else if (data === undefined)
    body = (
      <div aria-busy="true">
        <Skeleton width="40%" />
      </div>
    );
  else
    body = general.map((c) => (
      <div key={c.id} className={styles.box}>
        <CommentItem comment={c} repo={repo} act={act} />
      </div>
    ));
  const inline = total - general.length;
  return (
    <section className={styles.section} aria-label="Commit comments">
      <h2 className={styles.heading}>
        {general.length} comment{general.length === 1 ? '' : 's'} on commit <code>{sha.slice(0, 7)}</code>
        {inline > 0 && (
          <span className={styles.muted}>
            {' '}
            · {inline} inline comment{inline === 1 ? '' : 's'}
          </span>
        )}
      </h2>
      {body}
      {signedIn ? (
        <div className={cx(styles.box, styles.newComment)}>
          <Avatar user={session.user} size={32} />
          <div className={styles.composer}>
            <Composer repo={repo} label="Comment" onSubmit={(b) => act.create({ body: b })} />
          </div>
        </div>
      ) : (
        <div className={styles.muted}>Sign in to comment on this commit.</div>
      )}
    </section>
  );
});

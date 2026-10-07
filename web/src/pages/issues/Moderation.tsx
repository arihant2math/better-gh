/**
 * Comment moderation UI (P42), loaded lazily from the timeline, review
 * threads and commit comments: the "edited ▾" revision list (+ a revision
 * viewer that can delete a revision's text) and the "Hide comment" dialog.
 */
import { useEffect, useState, type RefObject } from 'react';
import { deleteEdit, listEdits, type ContentEdit, type ContentKind } from '../../api/moderation';
import type { MinimizedReason } from '../../sync/models';
import { MINIMIZE_REASONS } from '../../sync/moderation';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { Skeleton } from '../../ui/EmptyState';
import { EyeClosedIcon, TrashIcon } from '../../ui/icons';
import { Markdown } from '../../ui/Markdown';
import { Popover } from '../../ui/Popover';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import styles from './Moderation.module.css';

export interface HistoryTarget {
  owner: string;
  repo: string;
  kind: ContentKind;
  id: number;
  /** Author or repo admin: may delete revisions. */
  canDelete: boolean;
}

/** One entry of the dropdown: an edit, or the original text (`edit` null). */
interface Revision {
  key: string;
  who: string;
  at: string;
  text: string | null;
  deleted: boolean;
  /** `user_content_edits.id`; 0 = the original text. */
  editId: number;
  current: boolean;
}

function revisions(edits: ContentEdit[], authorLogin: string, createdAt: string): Revision[] {
  const out: Revision[] = edits.map((e, i) => ({
    key: `e${e.id}`,
    who: e.editor?.login ?? 'ghost',
    at: e.edited_at,
    text: e.body,
    deleted: e.deleted_at != null && e.body == null,
    editId: e.id,
    current: i === 0,
  }));
  const first = edits[edits.length - 1];
  if (first) {
    out.push({ key: 'created', who: authorLogin, at: createdAt, text: first.previous_body, deleted: first.previous_body == null, editId: 0, current: false });
  }
  return out;
}

/** "edited ▾" dropdown: revisions newest first, then the original. */
export function EditHistory({
  target,
  anchor,
  onClose,
  authorLogin,
  createdAt,
}: {
  target: HistoryTarget;
  anchor: RefObject<HTMLElement | null>;
  onClose: () => void;
  authorLogin: string;
  createdAt: string;
}) {
  const [edits, setEdits] = useState<ContentEdit[] | null>(null);
  const [failed, setFailed] = useState(false);
  const [viewing, setViewing] = useState<Revision | null>(null);
  const { owner, repo, kind, id } = target;
  const load = () =>
    listEdits(owner, repo, kind, id).then(setEdits, () => {
      setFailed(true);
    });
  useEffect(() => {
    void listEdits(owner, repo, kind, id).then(setEdits, () => setFailed(true));
  }, [owner, repo, kind, id]);
  const list = edits ? revisions(edits, authorLogin, createdAt) : [];
  return (
    <>
      <Popover open={!viewing} onClose={onClose} anchor={anchor} placement="bottom-start" className={styles.history}>
        <div className={styles.historyHeader}>{edits ? `Edited ${edits.length} time${edits.length === 1 ? '' : 's'}` : 'Edit history'}</div>
        {failed ? (
          <div className={styles.historyEmpty}>Couldn’t load the edit history.</div>
        ) : !edits ? (
          <div className={styles.historyEmpty}>
            <Skeleton width="80%" />
          </div>
        ) : list.length === 0 ? (
          <div className={styles.historyEmpty}>No edit history recorded.</div>
        ) : (
          <ul className={styles.historyList} aria-label="Revisions">
            {list.map((r) => (
              <li key={r.key}>
                <button type="button" className={styles.historyItem} onClick={() => setViewing(r)}>
                  <strong>{r.who}</strong> {r.editId === 0 ? 'created' : 'edited'} <RelativeTime date={r.at} />
                  {r.deleted && <span className={styles.deletedTag}>deleted</span>}
                </button>
              </li>
            ))}
          </ul>
        )}
      </Popover>
      {viewing && (
        <RevisionDialog
          revision={viewing}
          canDelete={target.canDelete && !viewing.current && !viewing.deleted}
          onClose={() => {
            setViewing(null);
            onClose();
          }}
          onDelete={() =>
            void deleteEdit(owner, repo, kind, id, viewing.editId).then(
              () => {
                toast({ kind: 'success', title: 'Revision deleted' });
                setViewing(null);
                void load();
              },
              (e: unknown) => toast({ kind: 'error', title: 'Couldn’t delete the revision', description: e instanceof Error ? e.message : undefined }),
            )
          }
          repo={`${owner}/${repo}`}
        />
      )}
    </>
  );
}

function RevisionDialog({ revision, canDelete, onClose, onDelete, repo }: { revision: Revision; canDelete: boolean; onClose: () => void; onDelete: () => void; repo: string }) {
  return (
    <Dialog
      open
      onClose={onClose}
      title={
        <span>
          {revision.who} {revision.editId === 0 ? 'created' : 'edited'} <RelativeTime date={revision.at} />
        </span>
      }
      aria-label="Revision"
      footer={
        canDelete ? (
          <Button variant="danger" leadingIcon={TrashIcon} onClick={onDelete}>
            Delete revision from history
          </Button>
        ) : undefined
      }
    >
      <div className={styles.revisionBody}>
        {revision.deleted || revision.text == null ? (
          <p className={styles.deletedNote}>This revision was deleted.</p>
        ) : revision.text.trim() ? (
          <Markdown source={revision.text} repo={repo} />
        ) : (
          <p className={styles.deletedNote}>No description provided.</p>
        )}
      </div>
    </Dialog>
  );
}

/** Reason picker for hiding a comment. */
export function HideDialog({ onClose, onHide }: { onClose: () => void; onHide: (reason: MinimizedReason) => void }) {
  const [reason, setReason] = useState<MinimizedReason>('spam');
  return (
    <Dialog
      open
      onClose={onClose}
      title="Hide comment"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            leadingIcon={EyeClosedIcon}
            onClick={() => {
              onHide(reason);
              onClose();
            }}
          >
            Hide
          </Button>
        </>
      }
    >
      <p className={styles.hint}>Hidden comments are collapsed for everyone; they can still be shown or unhidden.</p>
      <div className={styles.reasons} role="radiogroup" aria-label="Reason">
        {MINIMIZE_REASONS.map((r) => (
          <label key={r.id} className={styles.reason}>
            <input type="radio" name="hide-reason" value={r.id} checked={reason === r.id} onChange={() => setReason(r.id)} data-autofocus={reason === r.id ? '' : undefined} />
            {r.label}
          </label>
        ))}
      </div>
    </Dialog>
  );
}

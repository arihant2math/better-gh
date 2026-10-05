import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { useCommands } from '../../app/commands';
import { ConfirmDialog } from '../../components/ConfirmDialog';
import { Link, navigate } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import type { Issue, Repo } from '../../sync/models';
import { canAdmin, deleteIssue } from '../../sync/moderation';
import { lockIssue, removeSubIssue, setPinned, transferIssue, unlockIssue, type LockReason } from '../../sync/mutations';
import { canPush, canTriage, pinnedIssues, reposForOwner } from '../../sync/selectors';
import { StateIcon } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { ArrowSwitchIcon, IssueTrackedByIcon, LockIcon, PinIcon, PinSlashIcon, RepoIcon, TrashIcon, UnlockIcon, XIcon } from '../../ui/icons';
import { Field, Select } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import styles from './IssueView.module.css';

const LOCK_REASONS: { value: LockReason | ''; label: string }[] = [
  { value: '', label: 'Choose a reason (optional)' },
  { value: 'off-topic', label: 'Off-topic' },
  { value: 'too heated', label: 'Too heated' },
  { value: 'resolved', label: 'Resolved' },
  { value: 'spam', label: 'Spam' },
];

/** Parent issue + lock / pin / transfer actions in the issue sidebar. */
export const IssueActions = observer(function IssueActions({ issue, repo }: { issue: Issue; repo: Repo }) {
  const [dialog, setDialog] = useState<null | 'lock' | 'transfer' | 'delete'>(null);
  const triage = canTriage(repo.id);
  const push = canPush(repo.id);
  const admin = canAdmin(repo.id) && !issue.isPr;
  const removeIssue = () => {
    const { done } = deleteIssue(issue);
    navigate(`/${repo.owner}/${repo.name}/issues`, { replace: true });
    done.then(
      () => toast({ kind: 'success', title: `Deleted #${issue.number}` }),
      () => undefined,
    );
  };
  const confirmed = issue.id > 0;
  const parent = issue.parentId != null ? store().get('issue', issue.parentId) : undefined;
  const parentRepo = parent ? store().get('repo', parent.repoId) : undefined;
  const pinFull = !issue.pinned && pinnedIssues(repo.id).length >= 3;

  const togglePin = () => {
    if (pinFull) {
      toast({ kind: 'error', title: 'You can pin up to 3 issues', description: 'Unpin another issue first.' });
      return;
    }
    setPinned(issue, !issue.pinned);
  };
  const toggleLock = () => {
    if (issue.locked) unlockIssue(issue);
    else setDialog('lock');
  };

  useCommands(
    confirmed
      ? [
          ...(triage ? [{ id: 'issue.lock', title: issue.locked ? 'Unlock conversation' : 'Lock conversation…', group: 'Issue', icon: issue.locked ? UnlockIcon : LockIcon, run: toggleLock }] : []),
          ...(push ? [{ id: 'issue.pin', title: issue.pinned ? 'Unpin issue' : 'Pin issue', group: 'Issue', icon: issue.pinned ? PinSlashIcon : PinIcon, run: togglePin }] : []),
          ...(push ? [{ id: 'issue.transfer', title: 'Transfer issue…', group: 'Issue', icon: ArrowSwitchIcon, run: () => setDialog('transfer') }] : []),
          ...(admin ? [{ id: 'issue.delete', title: 'Delete issue…', group: 'Issue', icon: TrashIcon, run: () => setDialog('delete') }] : []),
        ]
      : [],
    [confirmed, triage, push, admin, issue.locked, issue.pinned, pinFull],
  );
  useShortcuts('Issue', {
    'shift+l': { handler: () => (triage && confirmed ? toggleLock() : false), description: 'Lock / unlock conversation', group: 'Issue' },
    'shift+p': { handler: () => (push && confirmed ? togglePin() : false), description: 'Pin / unpin issue', group: 'Issue' },
  });

  return (
    <>
      {parent && (
        <section className={styles.sideSection}>
          <div className={styles.sideHeaderStatic}>Parent issue</div>
          <div className={styles.sideBody}>
            <div className={styles.parentRow}>
              <IssueTrackedByIcon size={14} />
              <StateIcon issue={parent} size={14} />
              <Link to={`/${parentRepo?.owner}/${parentRepo?.name}/issues/${parent.number}`} className={styles.parentLink}>
                {parent.title} <span className={styles.subtle}>#{parent.number}</span>
              </Link>
              {triage && confirmed && (
                <button type="button" className={styles.inlineIcon} aria-label="Remove parent" title="Remove parent" onClick={() => removeSubIssue(parent, issue.id)}>
                  <XIcon size={14} />
                </button>
              )}
            </div>
          </div>
        </section>
      )}
      {confirmed && (triage || push) && (
        <section className={styles.sideSection}>
          <div className={styles.sideActions}>
            {triage && (
              <button type="button" className={styles.sideAction} onClick={toggleLock}>
                {issue.locked ? <UnlockIcon size={16} /> : <LockIcon size={16} />}
                {issue.locked ? 'Unlock conversation' : 'Lock conversation'}
              </button>
            )}
            {push && (
              <button type="button" className={styles.sideAction} onClick={togglePin} aria-disabled={pinFull}>
                {issue.pinned ? <PinSlashIcon size={16} /> : <PinIcon size={16} />}
                {issue.pinned ? 'Unpin issue' : 'Pin issue'}
              </button>
            )}
            {push && (
              <button type="button" className={styles.sideAction} onClick={() => setDialog('transfer')}>
                <ArrowSwitchIcon size={16} />
                Transfer issue
              </button>
            )}
            {admin && (
              <button type="button" className={`${styles.sideAction} ${styles.sideActionDanger}`} onClick={() => setDialog('delete')}>
                <TrashIcon size={16} />
                Delete issue
              </button>
            )}
          </div>
        </section>
      )}
      <LockDialog open={dialog === 'lock'} onClose={() => setDialog(null)} issue={issue} />
      <TransferDialog open={dialog === 'transfer'} onClose={() => setDialog(null)} issue={issue} repo={repo} />
      <ConfirmDialog open={dialog === 'delete'} onClose={() => setDialog(null)} onConfirm={removeIssue} title="Delete issue?" confirmLabel="Delete this issue">
        <p className={styles.dialogText}>
          <strong>#{issue.number}</strong> and its comments will be permanently deleted. This can’t be undone; the number won’t be reused.
        </p>
      </ConfirmDialog>
    </>
  );
});

function LockDialog({ open, onClose, issue }: { open: boolean; onClose: () => void; issue: Issue }) {
  const [reason, setReason] = useState<LockReason | ''>('');
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Lock conversation on this issue"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            leadingIcon={LockIcon}
            onClick={() => {
              lockIssue(issue, reason || null);
              onClose();
            }}
          >
            Lock conversation
          </Button>
        </>
      }
    >
      <ul className={styles.dialogList}>
        <li>Other users <strong>can’t add new comments</strong> to this issue.</li>
        <li>You and other collaborators with access to this repository can still leave comments that others can see.</li>
        <li>You can always unlock this issue again in the future.</li>
      </ul>
      <Field label="Reason for locking" htmlFor="lock-reason" hint="The reason is shown in the timeline.">
        <Select id="lock-reason" value={reason} onChange={(e) => setReason(e.target.value as LockReason | '')}>
          {LOCK_REASONS.map((r) => (
            <option key={r.value} value={r.value}>
              {r.label}
            </option>
          ))}
        </Select>
      </Field>
    </Dialog>
  );
}

const TransferDialog = observer(function TransferDialog({ open, onClose, issue, repo }: { open: boolean; onClose: () => void; issue: Issue; repo: Repo }) {
  const [targetId, setTargetId] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const targets = reposForOwner(repo.ownerId).filter((r) => r.id !== repo.id && r.hasIssues && !r.archived && canPush(r.id) && !(repo.private === false && r.private));
  const target = targets.find((r) => r.id === targetId);
  const submit = () => {
    if (!target) return;
    setBusy(true);
    transferIssue(issue, target).done.then(
      (res) => {
        setBusy(false);
        onClose();
        const moved = res.data as { number?: number; html_url?: string } | undefined;
        const path = moved?.html_url ? new URL(moved.html_url, location.origin).pathname : `/${target.owner}/${target.name}/issues/${moved?.number ?? ''}`;
        toast({ kind: 'success', title: `Transferred to ${target.owner}/${target.name}` });
        navigate(path, { replace: true });
      },
      () => setBusy(false),
    );
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Transfer this issue"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!target || busy} loading={busy} onClick={submit}>
            Transfer issue
          </Button>
        </>
      }
    >
      <p className={styles.dialogText}>
        Move <strong>#{issue.number}</strong> to another repository owned by <strong>{repo.owner}</strong>. Labels and milestones are kept when the target has ones with the same
        name; assignees are kept when they can be assigned there.
      </p>
      {targets.length === 0 ? (
        <p className={styles.subtle}>No other repositories you can write to under {repo.owner}.</p>
      ) : (
        <div className={styles.repoChoices} role="radiogroup" aria-label="Target repository">
          {targets.map((r) => (
            <label key={r.id} className={styles.repoChoice}>
              <input type="radio" name="transfer-target" checked={targetId === r.id} onChange={() => setTargetId(r.id)} />
              <RepoIcon size={16} />
              <span>
                {r.owner}/<strong>{r.name}</strong>
              </span>
              {r.private && <span className={styles.assoc}>Private</span>}
            </label>
          ))}
        </div>
      )}
    </Dialog>
  );
});

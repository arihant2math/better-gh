import { useId, useState } from 'react';
import { actionsKey, listPendingDeployments, reviewPendingDeployments, type PendingDeployment } from '../../api/actions';
import { invalidate, refresh, useResource } from '../../api/cache';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { ClockIcon, RocketIcon } from '../../ui/icons';
import { Field, Textarea } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import { refreshRun } from './data';
import styles from './Run.module.css';

export const pendingKey = (owner: string, repo: string, runId: number) => actionsKey(owner, repo, 'pending', runId);

function reviewerNames(p: PendingDeployment): string {
  return p.reviewers.map((r) => (r.type === 'Team' ? (r.reviewer.slug ?? r.reviewer.name ?? 'team') : (r.reviewer.login ?? 'user'))).join(', ');
}

function timerLeft(p: PendingDeployment): number {
  const end = Date.parse(p.wait_timer_started_at) + p.wait_timer * 60_000;
  return Math.max(0, Math.ceil((end - Date.now()) / 60_000));
}

/** "Review pending deployments" banner of a `waiting` run, with the review dialog. */
export function PendingDeployments({ owner, repo, runId }: { owner: string; repo: string; runId: number }) {
  const key = pendingKey(owner, repo, runId);
  const loader = () => listPendingDeployments(owner, repo, runId);
  const { data } = useResource(key, loader, { ttlMs: 5_000 });
  const [open, setOpen] = useState(false);
  if (!data || data.length === 0) return null;
  const canReview = data.some((p) => p.current_user_can_approve);
  const names = data.map((p) => p.environment.name).join(', ');
  const reviewing = data.filter((p) => p.reviewers.length > 0);
  const timers = data.filter((p) => p.wait_timer > 0 && timerLeft(p) > 0);
  return (
    <section className={styles.pending} aria-label="Pending deployments">
      <RocketIcon size={16} className={styles.pendingIcon} />
      <div className={styles.pendingText}>
        <strong>
          {data.length === 1 ? 'A deployment is' : `${data.length} deployments are`} waiting: {names}
        </strong>
        <span className={styles.pendingSub}>
          {reviewing.length > 0 && <>Review required from {reviewing.map(reviewerNames).join('; ')}. </>}
          {timers.map((p) => (
            <span key={p.environment.id}>
              <ClockIcon size={12} /> {p.environment.name} waits {timerLeft(p)} more minute{timerLeft(p) === 1 ? '' : 's'}.{' '}
            </span>
          ))}
        </span>
      </div>
      {canReview && (
        <Button size="sm" variant="primary" onClick={() => setOpen(true)}>
          Review deployments
        </Button>
      )}
      <Dialog open={open} onClose={() => setOpen(false)} title="Review pending deployments">
        {open && (
          <ReviewForm
            pending={data}
            onClose={() => setOpen(false)}
            onSubmit={async (ids, state, comment) => {
              await reviewPendingDeployments(owner, repo, runId, ids, state, comment);
              invalidate(key);
              void refresh(key, loader).catch(() => undefined);
              refreshRun(owner, repo, runId);
              toast({ kind: 'success', title: state === 'approved' ? 'Deployment approved' : 'Deployment rejected' });
            }}
          />
        )}
      </Dialog>
    </section>
  );
}

function ReviewForm({
  pending,
  onClose,
  onSubmit,
}: {
  pending: PendingDeployment[];
  onClose: () => void;
  onSubmit: (ids: number[], state: 'approved' | 'rejected', comment: string) => Promise<void>;
}) {
  const id = useId();
  const allowed = pending.filter((p) => p.current_user_can_approve);
  const [selected, setSelected] = useState<Set<number>>(() => new Set(allowed.map((p) => p.environment.id)));
  const [comment, setComment] = useState('');
  const [busy, setBusy] = useState<'approved' | 'rejected' | null>(null);
  const submit = async (state: 'approved' | 'rejected') => {
    if (busy || selected.size === 0) return;
    setBusy(state);
    try {
      await onSubmit([...selected], state, comment.trim());
      onClose();
    } catch (e) {
      toast({ kind: 'error', title: "Couldn't review the deployment", description: (e as Error).message });
      setBusy(null);
    }
  };
  return (
    <div className={styles.reviewForm}>
      <fieldset className={styles.reviewEnvs}>
        <legend className={styles.reviewLegend}>Environments</legend>
        {pending.map((p) => {
          const can = p.current_user_can_approve;
          return (
            <label key={p.environment.id} className={styles.reviewEnv} data-disabled={!can || undefined}>
              <input
                type="checkbox"
                checked={selected.has(p.environment.id)}
                disabled={!can}
                onChange={(e) =>
                  setSelected((s) => {
                    const n = new Set(s);
                    if (e.target.checked) n.add(p.environment.id);
                    else n.delete(p.environment.id);
                    return n;
                  })
                }
              />
              <span className={styles.reviewEnvName}>{p.environment.name}</span>
              <span className={styles.reviewEnvMeta}>
                {p.reviewers.slice(0, 3).map((r) => (r.type === 'User' && r.reviewer.login ? <Avatar key={r.reviewer.id} user={{ login: r.reviewer.login, avatarUrl: r.reviewer.avatar_url ?? '' }} size={16} /> : null))}
                {!can ? 'You are not a required reviewer' : <>Waiting since <RelativeTime date={p.wait_timer_started_at} /></>}
              </span>
            </label>
          );
        })}
      </fieldset>
      <Field label="Comment" htmlFor={`${id}-comment`}>
        <Textarea id={`${id}-comment`} rows={3} value={comment} placeholder="Leave a comment (optional)" onChange={(e) => setComment(e.target.value)} />
      </Field>
      <div className={styles.reviewActions}>
        <Button onClick={onClose}>Cancel</Button>
        <Button variant="danger" loading={busy === 'rejected'} disabled={selected.size === 0 || !!busy} onClick={() => void submit('rejected')}>
          Reject
        </Button>
        <Button variant="primary" loading={busy === 'approved'} disabled={selected.size === 0 || !!busy} onClick={() => void submit('approved')}>
          Approve and deploy
        </Button>
      </div>
    </div>
  );
}

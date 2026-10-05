import { observer } from 'mobx-react-lite';
import { useId, useRef, useState } from 'react';
import { mutate, useResource } from '../../api/cache';
import {
  RESOLUTIONS,
  alertsPrefix,
  getAlert,
  listLocations,
  maskSecret,
  resolutionLabel,
  ssKeys,
  updateAlert,
  type Resolution,
  type SecretScanningAlert,
} from '../../api/secretScanning';
import { invalidateLists } from '../../components/admin/usePagedList';
import { CopyButton, errorMessage } from '../../components/settings/kit';
import { Link, useParams } from '../../router';
import { Button, IconButton } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { ArrowLeftIcon, CheckCircleIcon, EyeClosedIcon, EyeIcon, FileIcon, GitCommitIcon, IssueReopenedIcon, KeyAsteriskIcon, ShieldIcon, ShieldLockIcon, TriangleDownIcon } from '../../ui/icons';
import { Field, Textarea } from '../../ui/Input';
import { Popover } from '../../ui/Popover';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import styles from './Security.module.css';
import { AlertStateBadge, BypassBadge, SecurityFrame, UserLink, blobHref, isDisabledError } from './shared';

/** `/:owner/:repo/security/secret-scanning/:number`: one alert, its locations, close / reopen. */
export default observer(function SecretScanningAlertPage() {
  const { owner, repo, number } = useParams<{ owner: string; repo: string; number: string }>();
  const n = Number(number);
  const key = ssKeys.alert(owner, repo, n);
  const res = useResource(Number.isInteger(n) && n > 0 ? key : null, () => getAlert(owner, repo, n));
  const base = `/${owner}/${repo}/security/secret-scanning`;
  const a = res.data;

  return (
    <SecurityFrame owner={owner} repo={repo}>
      <Link to={base} className={`${styles.small} ${styles.muted}`}>
        <ArrowLeftIcon size={14} /> All secret scanning alerts
      </Link>
      {!a ? (
        res.error || !(n > 0) ? (
          <EmptyState icon={KeyAsteriskIcon} title={isDisabledError(res.error) ? 'Secret scanning is disabled' : 'Alert not found'}>
            {res.error ? errorMessage(res.error) : `There is no alert #${number}.`}
          </EmptyState>
        ) : (
          <div className={styles.section} aria-busy="true">
            <Skeleton width="45%" height={24} />
            <Skeleton width="30%" />
            <Skeleton height={40} />
          </div>
        )
      ) : (
        <AlertDetail owner={owner} repo={repo} alert={a} onChange={(next) => mutate(key, () => next)} />
      )}
    </SecurityFrame>
  );
});

function AlertDetail({ owner, repo, alert: a, onChange }: { owner: string; repo: string; alert: SecretScanningAlert; onChange: (a: SecretScanningAlert) => void }) {
  const [revealed, setRevealed] = useState(false);
  const [busy, setBusy] = useState(false);
  const locations = useResource(ssKeys.locations(owner, repo, a.number), () => listLocations(owner, repo, a.number));

  const save = async (body: Parameters<typeof updateAlert>[3]) => {
    setBusy(true);
    try {
      const next = await updateAlert(owner, repo, a.number, body);
      onChange(next);
      invalidateLists(alertsPrefix(owner, repo));
      toast({ kind: 'success', title: next.state === 'open' ? `Alert #${a.number} reopened` : `Alert #${a.number} closed as ${resolutionLabel(next.resolution).toLowerCase()}` });
      return true;
    } catch (e) {
      toast({ kind: 'error', title: errorMessage(e) });
      return false;
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div className={styles.detailHead}>
        <div className={styles.header}>
          <h1 className={styles.title}>
            {a.secret_type_display_name} <span className={styles.muted}>#{a.number}</span>
          </h1>
          <div className={styles.detailActions}>
            {a.state === 'open' ? (
              <CloseAs busy={busy} onClose={(resolution, comment) => save({ state: 'resolved', resolution, resolution_comment: comment || null })} />
            ) : (
              <Button leadingIcon={IssueReopenedIcon} loading={busy} onClick={() => void save({ state: 'open' })}>
                Reopen alert
              </Button>
            )}
          </div>
        </div>
        <div className={styles.detailMeta}>
          <AlertStateBadge state={a.state} resolution={a.resolution} />
          {a.push_protection_bypassed && <BypassBadge />}
          <span>
            Opened <RelativeTime date={a.created_at} /> · <span className={styles.mono}>{a.secret_type}</span>
          </span>
        </div>
      </div>

      <section className={styles.section} aria-label="Secret">
        <h2 className={styles.sectionTitle}>Secret</h2>
        <div className={styles.secretBox}>
          <KeyAsteriskIcon size={16} />
          <code className={styles.secretValue} data-testid="secret-value">
            {revealed ? a.secret : maskSecret(a.secret)}
          </code>
          <IconButton icon={revealed ? EyeClosedIcon : EyeIcon} label={revealed ? 'Hide secret' : 'Reveal secret'} size="sm" onClick={() => setRevealed((r) => !r)} aria-pressed={revealed} />
          <CopyButton value={a.secret} label="Copy" />
        </div>
        <p className={`${styles.small} ${styles.muted}`}>
          Rotate this secret with its provider first: closing the alert does not revoke it, and the secret stays in the git history.
        </p>
      </section>

      <section className={styles.section} aria-label="Activity">
        <h2 className={styles.sectionTitle}>Activity</h2>
        <ul className={styles.timeline}>
          <li>
            <ShieldIcon size={16} />
            <span>
              Secret detected <RelativeTime date={a.created_at} />
              {a.first_location_detected && (
                <>
                  {' '}
                  in <span className={styles.mono}>{a.first_location_detected.path}</span>
                </>
              )}
            </span>
          </li>
          {a.push_protection_bypassed && (
            <li>
              <ShieldLockIcon size={16} />
              <span>
                Push protection bypassed by <UserLink user={a.push_protection_bypassed_by} />
                {a.push_protection_bypassed_at && (
                  <>
                    {' '}
                    <RelativeTime date={a.push_protection_bypassed_at} />
                  </>
                )}
              </span>
            </li>
          )}
          {a.state === 'resolved' && (
            <li>
              <CheckCircleIcon size={16} />
              <div>
                <span>
                  Closed as <strong>{resolutionLabel(a.resolution).toLowerCase()}</strong> by <UserLink user={a.resolved_by} />
                  {a.resolved_at && (
                    <>
                      {' '}
                      <RelativeTime date={a.resolved_at} />
                    </>
                  )}
                </span>
                {a.resolution_comment && <p className={styles.comment}>{a.resolution_comment}</p>}
              </div>
            </li>
          )}
        </ul>
      </section>

      <section className={styles.section} aria-label="Locations">
        <h2 className={styles.sectionTitle}>Locations{locations.data ? ` (${locations.data.length})` : ''}</h2>
        <div className={styles.box}>
          {locations.error ? (
            <div className={styles.empty}>{errorMessage(locations.error)}</div>
          ) : !locations.data ? (
            <ul className={styles.rows}>
              <li className={styles.row}>
                <Skeleton width="50%" />
              </li>
            </ul>
          ) : locations.data.length === 0 ? (
            <div className={styles.empty}>No locations recorded.</div>
          ) : (
            <ul className={styles.rows} aria-label="Secret locations">
              {locations.data.map((l, i) => (
                <li key={i} className={styles.row}>
                  <FileIcon size={16} className={styles.rowIcon} />
                  <div className={styles.rowMain}>
                    <Link to={blobHref(owner, repo, l.details)} className={`${styles.rowTitle} ${styles.mono}`}>
                      {l.details.path}:{l.details.start_line}
                      {l.details.end_line !== l.details.start_line && `-${l.details.end_line}`}
                    </Link>
                    <span className={styles.rowMeta}>
                      <GitCommitIcon size={14} /> Commit{' '}
                      <Link to={`/${owner}/${repo}/commit/${l.details.commit_sha}`} className={styles.mono}>
                        {l.details.commit_sha.slice(0, 7)}
                      </Link>
                      {' · '}line {l.details.start_line}, columns {l.details.start_column}–{l.details.end_column}
                    </span>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </div>
      </section>
    </>
  );
}

/** "Close as" dropdown: resolution radios + optional comment. */
function CloseAs({ busy, onClose }: { busy: boolean; onClose: (r: Resolution, comment: string) => Promise<boolean> }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [resolution, setResolution] = useState<Resolution | null>(null);
  const [comment, setComment] = useState('');
  const id = useId();
  return (
    <>
      <Button ref={anchor} trailingIcon={TriangleDownIcon} onClick={() => setOpen((o) => !o)} aria-haspopup="dialog" aria-expanded={open}>
        Close as
      </Button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={anchor} placement="bottom-end" role="dialog" aria-label="Close alert">
        <form
          className={styles.closeForm}
          onSubmit={(e) => {
            e.preventDefault();
            if (!resolution) return;
            void onClose(resolution, comment.trim()).then((ok) => {
              if (!ok) return;
              setOpen(false);
              setResolution(null);
              setComment('');
            });
          }}
        >
          <div role="radiogroup" aria-label="Resolution" className={styles.section}>
            {RESOLUTIONS.map((r) => (
              <label key={r.value} className={styles.radio}>
                <input type="radio" name={`${id}-resolution`} value={r.value} checked={resolution === r.value} onChange={() => setResolution(r.value)} />
                <span className={styles.radioText}>
                  <strong>{r.label}</strong>
                  <span className={styles.radioDesc}>{r.description}</span>
                </span>
              </label>
            ))}
          </div>
          <Field label="Comment (optional)" htmlFor={`${id}-comment`}>
            <Textarea id={`${id}-comment`} rows={3} maxLength={280} value={comment} onChange={(e) => setComment(e.target.value)} placeholder="Add a comment" />
          </Field>
          <Button type="submit" variant="primary" disabled={!resolution} loading={busy}>
            Close alert
          </Button>
        </form>
      </Popover>
    </>
  );
}

import { useState } from 'react';
import { mutate, useResource } from '../../api/cache';
import { BYPASS_REASONS, createBypass, getPushBlock, ssKeys, type BypassReason, type PushBlock } from '../../api/secretScanning';
import { Banner, errorMessage } from '../../components/settings/kit';
import { formatDateTime } from '../../components/admin/format';
import { Link, useParams } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { CheckCircleIcon, ShieldLockIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './Security.module.css';
import { blobHref } from './shared';

/**
 * `/:owner/:repo/security/secret-scanning/unblock-secret/:placeholder`: the
 * URL printed by `git push` when push protection blocks a secret. Lets a
 * writer allow that one secret for a while (GitHub: 3 hours).
 */
export default function UnblockSecretPage() {
  const { owner, repo, placeholder } = useParams<{ owner: string; repo: string; placeholder: string }>();
  const key = ssKeys.pushBlock(owner, repo, placeholder);
  const res = useResource(key, () => getPushBlock(owner, repo, placeholder));
  const b = res.data;
  return (
    <div className={styles.narrow}>
      <div className={styles.header}>
        <ShieldLockIcon size={24} />
        <h1 className={styles.title}>Push protection blocked a secret</h1>
      </div>
      {!b ? (
        res.error ? (
          <EmptyState icon={ShieldLockIcon} title="Blocked push not found">
            {errorMessage(res.error)}. The link may be wrong, or you may not have write access to {owner}/{repo}.
          </EmptyState>
        ) : (
          <div className={styles.section} aria-busy="true">
            <Skeleton width="60%" />
            <Skeleton width="40%" />
            <Skeleton height={80} />
          </div>
        )
      ) : (
        <Unblock owner={owner} repo={repo} block={b} onBypassed={(next) => mutate(key, () => next)} />
      )}
    </div>
  );
}

function Unblock({ owner, repo, block: b, onBypassed }: { owner: string; repo: string; block: PushBlock; onBypassed: (b: PushBlock) => void }) {
  const [reason, setReason] = useState<BypassReason | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const active = !!b.bypassed_at && (!b.expires_at || Date.parse(b.expires_at) > Date.now());
  const expired = !!b.bypassed_at && !active;

  const submit = async () => {
    if (!reason || busy) return;
    setBusy(true);
    setError(null);
    try {
      const r = await createBypass(owner, repo, reason, b.placeholder_id);
      onBypassed({ ...b, reason: r.reason, bypassed_at: new Date().toISOString(), expires_at: r.expire_at });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <p className={styles.muted}>
        A push to <Link to={`/${owner}/${repo}`}>{owner}/{repo}</Link> contained a secret. Remove it from your commits (recommended), or allow this secret
        if you are sure it is safe to push.
      </p>
      <div className={styles.box} style={{ padding: 'var(--sp-3) var(--sp-4)' }}>
        <dl className={styles.kv}>
          <dt>Secret type</dt>
          <dd>
            <strong>{b.secret_type_display_name}</strong>
          </dd>
          <dt>Secret</dt>
          <dd className={styles.mono}>{b.secret_preview}</dd>
          <dt>Location</dt>
          <dd>
            <Link to={blobHref(owner, repo, b)} className={styles.mono}>
              {b.path}:{b.start_line}
            </Link>
          </dd>
          <dt>Commit</dt>
          <dd>
            <Link to={`/${owner}/${repo}/commit/${b.commit_sha}`} className={styles.mono}>
              {b.commit_sha.slice(0, 7)}
            </Link>
          </dd>
          <dt>Blocked</dt>
          <dd>
            <RelativeTime date={b.created_at} />
          </dd>
        </dl>
      </div>

      {active ? (
        <Banner tone="success" icon={CheckCircleIcon}>
          <strong>You can now push this secret until {formatDateTime(b.expires_at)}.</strong> Retry your push.
          {b.reason && <> Reason: {BYPASS_REASONS.find((r) => r.value === b.reason)?.label ?? b.reason}.</>}
        </Banner>
      ) : (
        <form
          className={styles.section}
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          {expired && <Banner tone="warning">The previous permission to push this secret expired {formatDateTime(b.expires_at)}. Allow it again to retry the push.</Banner>}
          <fieldset className={styles.reasons}>
            <legend>Why is it safe to push this secret?</legend>
            {BYPASS_REASONS.map((r) => (
              <label key={r.value} className={styles.radio}>
                <input type="radio" name="bypass-reason" value={r.value} checked={reason === r.value} onChange={() => setReason(r.value)} />
                <span className={styles.radioText}>
                  <strong>{r.label}</strong>
                  <span className={styles.radioDesc}>{r.description}</span>
                </span>
              </label>
            ))}
          </fieldset>
          {error && <Banner tone="danger">{error}</Banner>}
          <div>
            <Button type="submit" variant="danger" disabled={!reason} loading={busy}>
              Allow me to push this secret
            </Button>
          </div>
        </form>
      )}
    </>
  );
}

import { observer } from 'mobx-react-lite';
import { useEffect, useId, useState } from 'react';
import { cancelImport, getImport, IMPORT_PHASES, phaseIndex, retryImport, type RepoImport } from '../../api/imports';
import { ApiError } from '../../api/client';
import { formatBytes } from '../../components/admin/format';
import { Banner, ButtonRow, errorMessage, FormStack, PageHeader } from '../../components/settings/kit';
import { Link, useParams } from '../../router';
import { store } from '../../sync';
import { repoByName } from '../../sync/selectors';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, CheckCircleFillIcon, CircleIcon, StopIcon, SyncIcon, XCircleFillIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { Spinner } from '../../ui/Spinner';
import { toast } from '../../ui/Toast';
import styles from './ImportProgress.module.css';

const POLL_MS = 1000;
const running = (s?: string) => s === 'queued' || s === 'importing';

/** Poll the import while it runs (stops once it finished). */
function useImport(owner: string, repo: string) {
  const [state, setState] = useState<{ key: string; data?: RepoImport; error?: unknown }>({ key: '' });
  const key = `${owner}/${repo}`;
  const current = state.key === key ? state : { key, data: undefined, error: undefined };
  const status = current.data?.status;
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      try {
        const data = await getImport(owner, repo);
        if (cancelled) return;
        setState({ key, data });
        if (running(data.status)) timer = setTimeout(tick, POLL_MS);
      } catch (error) {
        if (!cancelled) setState({ key, error });
      }
    };
    if (!status || running(status)) void tick();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [owner, repo, key, status]);
  return { data: current.data, error: current.error, set: (data: RepoImport) => setState({ key, data }) };
}

/** `/:owner/:repo/import`: import progress, cancel and retry. */
export default observer(function ImportProgressPage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const repo = repoByName(owner, name);
  const isAdmin = repo ? store().get('viewerRepo', repo.id)?.permission === 'admin' : false;
  const imp = useImport(owner, name);
  const [busy, setBusy] = useState(false);
  const [showCreds, setShowCreds] = useState(false);
  const [username, setUsername] = useState('');
  const [secret, setSecret] = useState('');
  const ids = { user: useId(), secret: useId() };

  if (imp.error) {
    const missing = imp.error instanceof ApiError && imp.error.status === 404;
    return (
      <div className={styles.page}>
        <EmptyState icon={AlertIcon} title={missing ? 'No import for this repository' : 'Could not load the import'}>
          {missing ? <Link to={`/${owner}/${name}`}>Go to the repository</Link> : errorMessage(imp.error)}
        </EmptyState>
      </div>
    );
  }
  const d = imp.data;
  if (!d) {
    return (
      <div className={styles.page}>
        <Skeleton height={28} width="40%" />
        <div className={styles.gap} />
        <Skeleton height={120} />
      </div>
    );
  }

  const act = async (fn: () => Promise<RepoImport>, done: string) => {
    setBusy(true);
    try {
      imp.set(await fn());
      toast({ kind: 'success', title: done });
    } catch (e) {
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  const current = phaseIndex(d.phase);
  const pct = d.objects_total > 0 ? Math.min(100, Math.round((d.objects_received / d.objects_total) * 100)) : 0;
  const base = `/${d.repository.owner}/${d.repository.name}`;

  return (
    <div className={styles.page}>
      <PageHeader
        title={d.status === 'complete' ? 'Import complete' : d.status === 'failed' ? 'Import failed' : d.status === 'cancelled' ? 'Import cancelled' : 'Importing your project'}
        description={
          <>
            {d.mirror ? 'Mirroring' : 'Importing'} <span className={styles.mono}>{d.source_url}</span> into{' '}
            <Link to={base}>{d.repository.full_name}</Link>.
          </>
        }
      />
      <ol className={styles.steps} aria-label="Import progress">
        {IMPORT_PHASES.filter((p) => p.id !== 'lfs' || d.include_lfs).map((p) => {
          const i = phaseIndex(p.id);
          const state =
            d.status === 'complete' ? 'done' : d.status === 'queued' ? 'pending' : i < current ? 'done' : i === current && d.status === 'importing' ? 'active' : i === current ? 'stopped' : 'pending';
          return (
            <li key={p.id} className={styles.step} data-state={state} aria-current={state === 'active' ? 'step' : undefined}>
              <span className={styles.stepIcon} aria-hidden>
                {state === 'done' ? (
                  <CheckCircleFillIcon size={16} />
                ) : state === 'active' ? (
                  <Spinner size={16} label="" />
                ) : state === 'stopped' ? (
                  <XCircleFillIcon size={16} />
                ) : (
                  <CircleIcon size={16} />
                )}
              </span>
              <span className={styles.stepLabel}>{p.label}</span>
              {p.id === 'receiving' && d.objects_total > 0 && (
                <span className={styles.stepMeta}>
                  {d.objects_received.toLocaleString()} / {d.objects_total.toLocaleString()} objects
                  {d.bytes_received > 0 && <> · {formatBytes(d.bytes_received)}</>}
                </span>
              )}
              {p.id === 'lfs' && d.lfs_objects_total > 0 && (
                <span className={styles.stepMeta}>
                  {d.lfs_objects_received} / {d.lfs_objects_total} files
                </span>
              )}
            </li>
          );
        })}
      </ol>
      {running(d.status) && (
        <div className={styles.bar} role="progressbar" aria-label="Objects received" aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct}>
          <span style={{ width: `${d.status === 'queued' ? 0 : Math.max(pct, 4)}%` }} />
        </div>
      )}
      {d.status === 'queued' && <p className={styles.muted}>Waiting for a worker to pick up the import…</p>}

      {d.status === 'complete' && (
        <Banner tone="success" icon={CheckCircleFillIcon}>
          {d.repository.full_name} is ready{d.mirror ? ' and will stay in sync with its source' : ''}.{' '}
          <Link to={base}>Open the repository</Link>
        </Banner>
      )}
      {d.status === 'failed' && (
        <Banner tone="danger" icon={AlertIcon}>
          <strong>The import failed.</strong> <span className={styles.error}>{d.error}</span>
        </Banner>
      )}
      {d.status === 'cancelled' && <Banner tone="warning">The import was cancelled. The repository is empty; you can retry or delete it.</Banner>}

      {isAdmin && (
        <div className={styles.actions}>
          {running(d.status) && (
            <Button variant="danger" leadingIcon={StopIcon} loading={busy} onClick={() => void act(() => cancelImport(owner, name), 'Import cancelled')}>
              Cancel import
            </Button>
          )}
          {(d.status === 'failed' || d.status === 'cancelled') && !showCreds && (
            <ButtonRow>
              <Button variant="primary" leadingIcon={SyncIcon} loading={busy} onClick={() => void act(() => retryImport(owner, name), 'Import restarted')}>
                Retry import
              </Button>
              <Button onClick={() => setShowCreds(true)}>Retry with new credentials</Button>
            </ButtonRow>
          )}
          {(d.status === 'failed' || d.status === 'cancelled') && showCreds && (
            <form
              onSubmit={(e) => {
                e.preventDefault();
                void act(() => retryImport(owner, name, { username: username.trim() || undefined, password_or_token: secret }), 'Import restarted').then(() => {
                  setSecret('');
                  setShowCreds(false);
                });
              }}
            >
              <FormStack>
                <Field label="Username" htmlFor={ids.user}>
                  <Input id={ids.user} value={username} autoComplete="off" onChange={(e) => setUsername(e.target.value)} />
                </Field>
                <Field label="Password or access token" htmlFor={ids.secret}>
                  <Input id={ids.secret} type="password" value={secret} autoComplete="new-password" required onChange={(e) => setSecret(e.target.value)} />
                </Field>
                <ButtonRow>
                  <Button type="submit" variant="primary" loading={busy} disabled={!secret}>
                    Retry import
                  </Button>
                  <Button onClick={() => setShowCreds(false)}>Cancel</Button>
                </ButtonRow>
              </FormStack>
            </form>
          )}
        </div>
      )}
    </div>
  );
});

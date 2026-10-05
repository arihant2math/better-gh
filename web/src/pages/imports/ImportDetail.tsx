import { useEffect, useRef, useState } from 'react';
import {
  IMPORT_STATS,
  IMPORT_STEPS,
  cancelMetadataImport,
  getImportLog,
  getMetadataImport,
  isActive,
  resumeMetadataImport,
  type ImportLogEntry,
  type MetadataImport,
  type StepState,
} from '../../api/metadataImports';
import { ErrorState, Panel, StatusPill, attempt, type PillStatus } from '../../components/admin/kit';
import { Link } from '../../router';
import { Button } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { CheckCircleFillIcon, CircleIcon, DotFillIcon, SkipIcon, XCircleFillIcon, type Icon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import s from './imports.module.css';

const POLL_MS = 1500;

export const STATUS_PILL: Record<MetadataImport['status'], [PillStatus, string]> = {
  queued: ['neutral', 'Queued'],
  running: ['info', 'Importing'],
  waiting: ['warning', 'Waiting for rate limit'],
  complete: ['ok', 'Complete'],
  failed: ['error', 'Failed'],
  cancelled: ['neutral', 'Cancelled'],
};

const STEP_ICON: Record<StepState, Icon> = {
  pending: CircleIcon,
  running: DotFillIcon,
  done: CheckCircleFillIcon,
  failed: XCircleFillIcon,
  skipped: SkipIcon,
};

/**
 * Import status, steps, counters and the live log; polls while active.
 * `mannequinsPath` links the reclaim page once mannequins were created.
 */
export function ImportDetail({ id, mannequinsPath }: { id: number; mannequinsPath?: string }) {
  const [imp, setImp] = useState<MetadataImport | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [log, setLog] = useState<ImportLogEntry[]>([]);
  const [newToken, setNewToken] = useState('');
  const [askToken, setAskToken] = useState(false);
  const lastLog = useRef(0);
  const logEnd = useRef<HTMLLIElement>(null);
  const status = imp?.status;
  // Polling restarts when a resume makes the import active again.
  const active = status !== undefined && isActive(status);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const tick = async () => {
      try {
        const [next, entries] = await Promise.all([getMetadataImport(id), getImportLog(id, lastLog.current)]);
        if (cancelled) return;
        setImp(next);
        setError(null);
        if (entries.entries.length) {
          lastLog.current = entries.entries[entries.entries.length - 1]!.id;
          setLog((l) => [...l, ...entries.entries]);
        }
        if (isActive(next.status)) timer = setTimeout(() => void tick(), POLL_MS);
      } catch (err) {
        if (!cancelled) setError(err);
      }
    };
    void tick();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [id, active]);

  useEffect(() => {
    logEnd.current?.scrollIntoView?.({ block: 'nearest' });
  }, [log.length]);

  if (error && !imp) return <ErrorState error={error} />;
  if (!imp) return <Skeleton height={240} />;

  const [pill, label] = STATUS_PILL[imp.status];
  const target = imp.repository?.full_name ?? `${imp.owner}/${imp.repo_name}`;
  const resume = (token?: string) =>
    attempt(
      imp.status === 'complete' ? 'Rerun import' : 'Resume import',
      () =>
        resumeMetadataImport(imp.id, token).then((next) => {
          setImp(next);
          setAskToken(false);
          setNewToken('');
        }),
      imp.status === 'complete' ? 'Import queued again' : 'Import resumed',
    );

  return (
    <div className={s.detail}>
      <div className={s.headline}>
        <StatusPill status={pill}>{label}</StatusPill>
        <a href={imp.source_url} target="_blank" rel="noreferrer noopener">
          {imp.source_repo}
        </a>
        <span className={s.muted}>→</span>
        {imp.repository ? <Link to={`/${target}`}>{target}</Link> : <span>{target}</span>}
        <span className={s.muted}>
          started <RelativeTime date={imp.created_at} />
          {imp.completed_at && (
            <>
              {' '}
              · finished <RelativeTime date={imp.completed_at} />
            </>
          )}
        </span>
      </div>

      {imp.error && (
        <div className={s.error} role="alert">
          {imp.error}
        </div>
      )}
      {imp.status === 'waiting' && imp.resume_at && (
        <p className={s.muted}>
          The source asked to slow down. The import continues <RelativeTime date={imp.resume_at} />.
        </p>
      )}

      <Panel title="Steps">
        <ol className={s.steps} aria-label="Import steps">
          {imp.steps.map((st) => {
            const I = STEP_ICON[st.state];
            return (
              <li key={st.name} className={s.step} data-state={st.state}>
                <I size={14} />
                <span className={s.stepName}>{IMPORT_STEPS[st.name] ?? st.name}</span>
                {st.name === 'git' && st.state === 'running' && imp.git && (
                  <span className={s.muted}>
                    {imp.git.phase}
                    {imp.git.objects_total > 0 && ` ${Math.round((100 * imp.git.objects_received) / imp.git.objects_total)}%`}
                  </span>
                )}
              </li>
            );
          })}
        </ol>
      </Panel>

      <Panel title="Imported">
        <dl className={s.stats}>
          {IMPORT_STATS.filter(([k]) => imp.stats[k] !== undefined || ['issues', 'comments', 'labels', 'releases'].includes(k)).map(([k, l]) => (
            <div key={k} className={s.stat}>
              <dt>{l}</dt>
              <dd>{imp.stats[k] ?? 0}</dd>
            </div>
          ))}
        </dl>
        {mannequinsPath && (imp.stats.mannequins ?? 0) > 0 && !isActive(imp.status) && (
          <p className={s.muted}>
            {imp.stats.mannequins === 1 ? 'One source user' : `${imp.stats.mannequins} source users`} had no account here and became mannequins.{' '}
            <Link to={mannequinsPath}>Reclaim mannequins</Link> to move their contributions to real accounts.
          </p>
        )}
      </Panel>

      <div className={s.actions}>
        {isActive(imp.status) && (
          <Button variant="danger" onClick={() => void attempt('Cancel import', () => cancelMetadataImport(imp.id).then(setImp), 'Import cancelled')}>
            Cancel import
          </Button>
        )}
        {(imp.status === 'failed' || imp.status === 'cancelled') && (
          <>
            <Button variant="primary" onClick={() => void resume()}>
              Resume
            </Button>
            <Button onClick={() => setAskToken((v) => !v)}>Resume with a new token</Button>
          </>
        )}
        {imp.status === 'complete' && (
          <Button onClick={() => void resume()} title="Imports only what is new on the source">
            Import again
          </Button>
        )}
      </div>
      {askToken && (
        <form
          className={s.actions}
          onSubmit={(e) => {
            e.preventDefault();
            if (newToken.trim()) void resume(newToken.trim());
          }}
        >
          <Input type="password" autoComplete="off" aria-label="New access token" placeholder="New access token" value={newToken} onChange={(e) => setNewToken(e.target.value)} />
          <Button type="submit" variant="primary" disabled={!newToken.trim()}>
            Resume
          </Button>
        </form>
      )}

      <Panel title="Log" padded={false}>
        <ol className={s.log} aria-label="Import log" aria-live="polite">
          {log.map((e) => (
            <li key={e.id} data-level={e.level}>
              <span className={s.logTime}>{new Date(e.created_at).toLocaleTimeString()}</span>
              {e.message}
            </li>
          ))}
          <li ref={logEnd} aria-hidden />
        </ol>
      </Panel>
    </div>
  );
}

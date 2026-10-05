import { useEffect } from 'react';
import { useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { getCheckRun } from '../../api/endpoints';
import type { RestCheckRun } from '../../api/types';
import { NotFound } from '../../app/NotFound';
import { Link, navigate, useParams } from '../../router';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, CheckCircleIcon, ClockIcon, LinkExternalIcon, XCircleIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './RepoNav.module.css';

/** Same-origin `details_url` → in-app path (Actions job pages); `null` for external URLs. */
export function internalPath(url: string | null | undefined, origin = window.location.origin): string | null {
  if (!url) return null;
  try {
    const u = new URL(url, origin);
    return u.origin === origin ? u.pathname + u.search + u.hash : null;
  } catch {
    return null;
  }
}

/**
 * `/:owner/:repo/runs/:id` (check run `html_url`): Actions check runs go to
 * their job page; external ones show a summary with a link to `details_url`.
 */
export default function CheckRunPage() {
  const { owner, repo, id } = useParams<{ owner: string; repo: string; id: string }>();
  const num = Number(id);
  const { data: run, error } = useResource<RestCheckRun>(Number.isFinite(num) ? `check-run:${owner}/${repo}:${num}` : null, () => getCheckRun(owner, repo, num));
  const inApp = internalPath(run?.details_url);
  useEffect(() => {
    if (inApp && !inApp.startsWith(`/${owner}/${repo}/runs/`)) navigate(inApp, { replace: true });
  }, [inApp, owner, repo]);

  if (!Number.isFinite(num) || (error instanceof ApiError && error.status === 404)) return <NotFound what="check run" />;
  if (error) {
    return (
      <EmptyState icon={AlertIcon} title="Couldn’t load this check run">
        {(error as Error).message}
      </EmptyState>
    );
  }
  if (!run || inApp) {
    return (
      <div className={styles.page}>
        <Skeleton width="40%" height={24} />
      </div>
    );
  }
  const Icon = run.status !== 'completed' ? ClockIcon : run.conclusion === 'success' || run.conclusion === 'neutral' || run.conclusion === 'skipped' ? CheckCircleIcon : XCircleIcon;
  return (
    <div className={styles.page}>
      <div className={styles.runCard}>
        <h2>
          <Icon size={20} className={run.status !== 'completed' ? styles.muted : Icon === CheckCircleIcon ? styles.ok : styles.fail} /> {run.name}
        </h2>
        <span className={styles.muted}>
          {run.app?.name ? `${run.app.name} · ` : ''}
          {run.status === 'completed' ? run.conclusion : run.status.replace('_', ' ')} on{' '}
          <Link to={`/${owner}/${repo}/commit/${run.head_sha}`}>
            <code>{run.head_sha.slice(0, 7)}</code>
          </Link>
          {run.completed_at ? (
            <>
              {' '}
              · <RelativeTime date={run.completed_at} />
            </>
          ) : run.started_at ? (
            <>
              {' '}
              · started <RelativeTime date={run.started_at} />
            </>
          ) : null}
        </span>
        {run.output?.title && <strong>{run.output.title}</strong>}
        {run.output?.summary && <p style={{ margin: 0, whiteSpace: 'pre-wrap' }}>{run.output.summary}</p>}
        {run.details_url && /^https?:\/\//i.test(run.details_url) && (
          <a href={run.details_url} target="_blank" rel="noopener noreferrer" className={styles.linkButton} style={{ alignSelf: 'flex-start' }}>
            <LinkExternalIcon size={16} /> View more details on {hostOf(run.details_url)}
          </a>
        )}
      </div>
    </div>
  );
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

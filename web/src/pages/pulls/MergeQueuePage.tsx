import { observer } from 'mobx-react-lite';
import { usePollWhileVisible, useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { getMergeQueue } from '../../api/endpoints';
import type { MergeQueue, MergeQueueEntry } from '../../api/types';
import { Link, useParams } from '../../router';
import { Avatar } from '../../ui/Badge';
import { cx } from '../../ui/Button';
import { Box, EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, CheckCircleIcon, ClockIcon, DotFillIcon, GitMergeQueueIcon, XCircleFillIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import { entryState, etaLabel, positionLabel, QUEUE_POLL_MS, type QueueTone } from './mergeQueue';
import styles from './MergeQueue.module.css';
import { useRouteRepo } from '../repo/useRouteRepo';

const METHOD: Record<string, string> = { MERGE: 'merge commit', SQUASH: 'squash', REBASE: 'rebase' };

/** `/:owner/:repo/queue/*`: the merge queue of one branch (P39). */
export default observer(function MergeQueuePage() {
  const params = useParams<{ '*': string }>();
  const repo = useRouteRepo();
  const { owner, name } = repo;
  const branch = params['*'] || repo.defaultBranch;
  const key = branch ? `merge-queue:${owner}/${name}:${branch}` : null;
  const load = () => getMergeQueue(owner, name, branch);
  const { data, error, loading } = useResource<MergeQueue>(key, load, { ttlMs: 10_000 });

  // The queue moves on its own: revalidate while the page is open.
  usePollWhileVisible(key, load, QUEUE_POLL_MS, { ttlMs: 10_000 });

  const notFound = error instanceof ApiError && error.status === 404;
  return (
    <div className={styles.page}>
      <header className={styles.header}>
        <h2 className={styles.title}>
          <GitMergeQueueIcon size={20} /> Merge queue
        </h2>
        <code className={styles.branch} title={branch}>
          {branch}
        </code>
        {data?.enabled && (
          <span className={styles.muted}>
            {data.entries.length} {data.entries.length === 1 ? 'pull request' : 'pull requests'} queued
          </span>
        )}
      </header>
      {data?.enabled && data.config && (
        <p className={styles.config}>
          Merges with a {METHOD[data.config.merge_method] ?? data.config.merge_method.toLowerCase()} · builds up to {data.config.max_entries_to_build}{' '}
          {data.config.max_entries_to_build === 1 ? 'entry' : 'entries'} at a time · merges {data.config.min_entries_to_merge}–{data.config.max_entries_to_merge} per group
          {data.config.grouping_strategy === 'HEADGREEN' ? ' · only the group head must pass' : ''}
        </p>
      )}
      {!data && loading ? (
        <Box>
          {[0, 1, 2].map((i) => (
            <div key={i} className={styles.skeletonRow}>
              <Skeleton width={28} height={20} />
              <Skeleton width="50%" />
            </div>
          ))}
        </Box>
      ) : !data ? (
        <EmptyState icon={AlertIcon} title={notFound ? 'Repository not found' : 'Couldn’t load the merge queue'}>
          {notFound ? `${owner}/${name} doesn’t exist or you don’t have access to it.` : error instanceof Error ? error.message : null}
        </EmptyState>
      ) : !data.enabled ? (
        <EmptyState icon={GitMergeQueueIcon} title="Merge queue is not enabled for this branch">
          Pull requests into <code>{branch}</code> are merged directly. Repository admins can require a merge queue with a branch ruleset.
        </EmptyState>
      ) : data.entries.length === 0 ? (
        <EmptyState icon={GitMergeQueueIcon} title="The merge queue is empty">
          Pull requests added to the queue for <code>{branch}</code> will show up here.
        </EmptyState>
      ) : (
        <ol className={styles.list} aria-label={`Merge queue for ${branch}`}>
          {data.entries.map((e) => (
            <QueueRow key={e.id} entry={e} owner={owner} repo={name} />
          ))}
        </ol>
      )}
    </div>
  );
});

function ToneIcon({ tone }: { tone: QueueTone }) {
  if (tone === 'running') return <Spinner size={14} />;
  if (tone === 'ok') return <CheckCircleIcon size={14} />;
  if (tone === 'fail') return <XCircleFillIcon size={14} />;
  return <DotFillIcon size={14} />;
}

function QueueRow({ entry, owner, repo }: { entry: MergeQueueEntry; owner: string; repo: string }) {
  const st = entryState(entry);
  const eta = etaLabel(entry.estimated_time_to_merge);
  const author = entry.pull.user;
  return (
    <li className={styles.row}>
      <span className={styles.position} title={positionLabel(entry)} aria-label={positionLabel(entry)}>
        {entry.position}
      </span>
      <div className={styles.main}>
        <Link to={`/${owner}/${repo}/pull/${entry.pull.number}`} className={styles.prLink}>
          {entry.pull.title} <span className={styles.number}>#{entry.pull.number}</span>
        </Link>
        <div className={styles.meta}>
          <span className={styles.author}>
            <Avatar user={{ login: author.login, avatarUrl: author.avatar_url }} size={16} />
            {author.login}
          </span>
          <span>
            <ClockIcon size={14} /> queued <RelativeTime date={entry.enqueued_at} />
          </span>
          {eta && <span>ETA {eta}</span>}
          {entry.jump && <span>jumped the queue</span>}
        </div>
      </div>
      <span className={cx(styles.state, styles[st.tone])} title={st.label}>
        <ToneIcon tone={st.tone} />
        {st.label}
      </span>
    </li>
  );
}

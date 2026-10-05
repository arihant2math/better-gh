import { useResource } from '../../api/cache';
import { codeKeys, getCommitStatuses, type CommitStatuses } from '../../api/code';
import type { BrowseCommit } from '../../api/types';
import { Link } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Skeleton } from '../../ui/EmptyState';
import { CheckIcon, DotFillIcon, HistoryIcon, XIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tooltip } from '../../ui/Tooltip';
import styles from './Code.module.css';
import { useLastCommit } from './data';
import type { CodeTarget } from './util';

export function CommitAuthor({ c, size = 20 }: { c: BrowseCommit; size?: number }) {
  const login = c.author.login;
  return (
    <>
      <Avatar user={{ login: login ?? c.author.name, avatarUrl: c.author.avatar_url ?? '', name: c.author.name }} size={size} />
      {login ? (
        <Link to={`/${login}`} className={styles.author}>
          {login}
        </Link>
      ) : (
        <span className={styles.author} title={c.author.email}>
          {c.author.name}
        </span>
      )}
    </>
  );
}

/** CI rollup icon for one commit (batched endpoint, one SHA). */
export function CiIcon({ owner, repo, sha }: { owner: string; repo: string; sha: string }) {
  const { data } = useResource<CommitStatuses>(codeKeys.statuses(owner, repo, [sha]), () => getCommitStatuses(owner, repo, [sha]), { ttlMs: 15_000 });
  const s = data?.statuses[sha];
  if (!s) return null;
  const label = `${s.success}/${s.total} checks passed${s.pending ? `, ${s.pending} pending` : ''}${s.failure ? `, ${s.failure} failing` : ''}`;
  return (
    <Tooltip label={label}>
      <span className={styles[`ci_${s.state}`] ?? styles.ci_pending} aria-label={label}>
        {s.state === 'success' ? <CheckIcon size={16} /> : s.state === 'failure' || s.state === 'error' ? <XIcon size={16} /> : <DotFillIcon size={16} />}
      </span>
    </Tooltip>
  );
}

/** "author · message · sha · time · History" bar above listings and files. */
export function LastCommitBar({ t, path }: { t: CodeTarget; path: string }) {
  const { data } = useLastCommit(t, path);
  const c = data?.commits[0];
  const historyUrl = `/${t.owner}/${t.repo}/commits/${t.ref}${path ? `/${path}` : ''}`;
  return (
    <div className={styles.lastCommit}>
      {c ? (
        <>
          <CommitAuthor c={c} />
          <Link to={`/${t.owner}/${t.repo}/commit/${c.sha}`} className={styles.commitMsg} title={c.message}>
            {c.summary}
          </Link>
          <CiIcon owner={t.owner} repo={t.repo} sha={c.sha} />
          <span className={styles.grow} />
          <Link to={`/${t.owner}/${t.repo}/commit/${c.sha}`} className={styles.sha}>
            {c.sha.slice(0, 7)}
          </Link>
          <span className={styles.muted}>
            · <RelativeTime date={c.committer.date} />
          </span>
        </>
      ) : (
        <>
          <Skeleton width={20} height={20} />
          <Skeleton width={280} />
          <span className={styles.grow} />
        </>
      )}
      <Link to={historyUrl} className={styles.historyLink}>
        <HistoryIcon size={16} /> History
      </Link>
    </div>
  );
}

import { observer } from 'mobx-react-lite';
import { useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { getCommitDiff, listPullCommits } from '../../api/endpoints';
import type { RestCommit } from '../../api/types';
import { DiffViewer } from '../../components/diff/DiffViewer';
import { Link, navigate, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import type { Issue, Repo } from '../../sync/models';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, ArrowLeftIcon, ArrowRightIcon, GitCommitIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import styles from '../issues/IssueView.module.css';
import pr from './PullDetail.module.css';

function useCommits(repo: Repo, issue: Issue) {
  return useResource<RestCommit[]>(`commits:${repo.owner}/${repo.name}#${issue.number}@${issue.headSha}`, () => listPullCommits(repo.owner, repo.name, issue.number), { immutable: true });
}

/** Commits tab: commit list grouped by day; `/pull/{n}/commits/{sha}` shows one commit's diff. */
export default observer(function CommitsTab({ repo, pr: issue, sha, base }: { repo: Repo; pr: Issue; sha?: string; base: string }) {
  const { data, loading, error } = useCommits(repo, issue);
  if (sha) return <CommitDiff repo={repo} sha={sha} commits={data} base={base} />;
  if (error) return <EmptyState icon={AlertIcon} title="Couldn’t load commits" />;
  if (loading || !data) {
    return (
      <div className={pr.commits}>
        {Array.from({ length: Math.min(issue.commits ?? 3, 6) }, (_, i) => (
          <div key={i} className={pr.commit}>
            <Skeleton width="50%" />
          </div>
        ))}
      </div>
    );
  }
  const byDay = new Map<string, RestCommit[]>();
  for (const c of data) {
    const day = new Date(c.commit.author.date).toLocaleDateString('en', { month: 'short', day: 'numeric', year: 'numeric' });
    byDay.set(day, [...(byDay.get(day) ?? []), c]);
  }
  return (
    <div className={pr.commits}>
      {[...byDay.entries()].map(([day, list]) => (
        <section key={day} className={pr.commitDay}>
          <h3 className={pr.commitDayTitle}>
            <GitCommitIcon size={16} /> Commits on {day}
          </h3>
          <div className={pr.commitList}>
            {list.map((c) => (
              <Link key={c.sha} to={`${base}/commits/${c.sha}`} className={`${pr.commit} ${pr.commitLink}`}>
                <div className={pr.commitMain}>
                  <div className={pr.commitMsg}>{c.commit.message.split('\n')[0]}</div>
                  <div className={styles.subtle}>
                    <Avatar user={c.author ? { login: c.author.login, avatarUrl: c.author.avatar_url } : null} size={16} /> <strong>{c.author?.login ?? c.commit.author.name}</strong> committed{' '}
                    <RelativeTime date={c.commit.author.date} />
                  </div>
                </div>
                <code className={pr.sha}>{c.sha.slice(0, 7)}</code>
              </Link>
            ))}
          </div>
        </section>
      ))}
    </div>
  );
});

const CommitDiff = observer(function CommitDiff({ repo, sha, commits, base }: { repo: Repo; sha: string; commits?: RestCommit[]; base: string }) {
  const { data, loading, error } = useResource(`commit-diff:${repo.owner}/${repo.name}@${sha}`, () => getCommitDiff(repo.owner, repo.name, sha), { immutable: true });
  const mode = useQuery().get('diff') === 'split' ? 'split' : 'unified';
  const i = commits?.findIndex((c) => c.sha === sha || c.sha.startsWith(sha)) ?? -1;
  const commit = i >= 0 ? commits![i] : undefined;
  const prev = i > 0 ? commits![i - 1] : undefined;
  const next = i >= 0 && commits && i < commits.length - 1 ? commits[i + 1] : undefined;
  useShortcuts('Commit', {
    '[': { handler: () => prev && navigate(`${base}/commits/${prev.sha}`), description: 'Previous commit', group: 'Commits' },
    ']': { handler: () => next && navigate(`${base}/commits/${next.sha}`), description: 'Next commit', group: 'Commits' },
    u: { handler: () => navigate(`${base}/commits`), description: 'Back to commits', group: 'Commits' },
  });
  return (
    <div className={pr.commitPage}>
      <div className={pr.commitHead}>
        <div className={pr.commitNav}>
          <Link to={`${base}/commits`} className={styles.subtle}>
            ← All commits
          </Link>
          <span style={{ flex: 1 }} />
          {commits && i >= 0 && (
            <span className={styles.subtle}>
              Commit {i + 1} of {commits.length}
            </span>
          )}
          <Button size="sm" leadingIcon={ArrowLeftIcon} disabled={!prev} onClick={() => prev && navigate(`${base}/commits/${prev.sha}`)} title="Previous commit ([)">
            Prev
          </Button>
          <Button size="sm" trailingIcon={ArrowRightIcon} disabled={!next} onClick={() => next && navigate(`${base}/commits/${next.sha}`)} title="Next commit (])">
            Next
          </Button>
        </div>
        {commit && (
          <div>
            <div className={pr.commitMsg} style={{ fontSize: 16 }}>
              {commit.commit.message.split('\n')[0]}
            </div>
            {commit.commit.message.includes('\n') && <pre className={styles.subtle}>{commit.commit.message.split('\n').slice(1).join('\n').trim()}</pre>}
            <div className={styles.subtle}>
              <Avatar user={commit.author ? { login: commit.author.login, avatarUrl: commit.author.avatar_url } : null} size={16} /> <strong>{commit.author?.login ?? commit.commit.author.name}</strong> committed{' '}
              <RelativeTime date={commit.commit.author.date} /> · <code className={pr.sha}>{sha.slice(0, 7)}</code>
            </div>
          </div>
        )}
      </div>
      {error ? (
        <EmptyState icon={AlertIcon} title={error instanceof ApiError && error.status === 404 ? 'Commit not found' : 'Couldn’t load the commit'} />
      ) : loading || data === undefined ? (
        <div className={pr.loading}>
          <Spinner /> Loading diff…
        </div>
      ) : (
        <div className={pr.files}>
          <DiffViewer diff={data} mode={mode} keyboard />
        </div>
      )}
    </div>
  );
});

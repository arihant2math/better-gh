import { observer } from 'mobx-react-lite';
import { useMemo } from 'react';
import { peek, prefetch, useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { codeKeys, getCommit, getCommitDiff, getCommitStatuses } from '../../api/code';
import { isSha } from '../../api/endpoints';
import type { GitPerson, RestCommitDetail, SimpleUser } from '../../api/types';
import type { DiffSource } from '../../components/diff/DiffView';
import { DiffViewer } from '../../components/diff/DiffViewer';
import { Link, navigate, useParams } from '../../router';
import { repoRefOf, treeUrl } from '../../components/code/urls';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import type { Repo } from '../../sync/models';
import { Button, IconButton } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, CodeIcon, CopyIcon, GitCommitIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import { splitMessage } from './group';
import { CommitCommentThread, useCommitCommentAnnotations } from './CommitComments';
import { CiIcon, Person, copyText } from './parts';
import { SignatureBadge, useSignatures } from './Signature';
import commentStyles from './CommitComments.module.css';
import styles from './Commits.module.css';
import { useRouteRepo } from '../repo/useRouteRepo';

/** Single commit: message, people, parents, CI, stats and the diff. */
export default observer(function CommitPage() {
  const params = useParams<{ sha: string }>();
  const repo = useRouteRepo();
  return <CommitView key={params.sha} repo={repo} refName={params.sha} />;
});

function CommitView({ repo, refName }: { repo: Repo; refName: string }) {
  const o = repo.owner;
  const r = repo.name;
  const opts = { immutable: isSha(refName) };
  const commit = useResource<RestCommitDetail>(codeKeys.commit(o, r, refName), () => getCommit(o, r, refName), opts);
  const sha = commit.data?.sha;
  const base = `/${o}/${r}`;

  useShortcuts('Commit', {
    y: {
      handler: () => {
        if (!sha || sha === refName) return false;
        // Seed the full-SHA (immutable) keys so the permalink renders without refetching.
        const data = commit.data!;
        prefetch(codeKeys.commit(o, r, sha), () => Promise.resolve(data), { immutable: true });
        const diff = peek<string>(codeKeys.commitDiff(o, r, refName));
        if (diff !== undefined) prefetch(codeKeys.commitDiff(o, r, sha), () => Promise.resolve(diff), { immutable: true });
        navigate(`${base}/commit/${sha}`, { replace: true });
      },
      description: 'Expand URL to its canonical form (permalink)',
      group: 'Commit',
    },
  });

  if (commit.error && !commit.data) {
    const missing = commit.error instanceof ApiError && (commit.error.status === 404 || commit.error.status === 422);
    return (
      <EmptyState icon={AlertIcon} title={missing ? 'Commit not found' : 'Couldn’t load this commit'}>
        {missing ? (
          <>
            No commit found for <code>{refName}</code> in {o}/{r}.
          </>
        ) : (
          'Try again in a moment.'
        )}
      </EmptyState>
    );
  }

  return (
    <div className={styles.commitPage}>
      {commit.data ? <CommitHeader repo={repo} c={commit.data} /> : <HeaderSkeleton />}
      {commit.data && <Stats c={commit.data} />}
      <Diff repo={repo} refName={refName} sha={sha} parent={commit.data?.parents.length === 1 ? commit.data.parents[0]!.sha : undefined} />
    </div>
  );
}

function asPerson(user: SimpleUser | null, git: GitPerson) {
  return { name: git.name, login: user?.login ?? null, avatar_url: user?.avatar_url ?? null };
}

function CommitHeader({ repo, c }: { repo: Repo; c: RestCommitDetail }) {
  const base = `/${repo.owner}/${repo.name}`;
  const { summary, body } = splitMessage(c.commit.message);
  const ci = useResource(codeKeys.statuses(repo.owner, repo.name, [c.sha]), () => getCommitStatuses(repo.owner, repo.name, [c.sha]));
  const author = asPerson(c.author, c.commit.author);
  const committer = asPerson(c.committer, c.commit.committer);
  const sameCommitter = (author.login ?? author.name) === (committer.login ?? committer.name);
  const signed = !!c.commit.verification?.signature;
  const sigs = useSignatures(repo.owner, repo.name, signed ? [[c.sha]] : []);
  return (
    <div className={styles.commitHeader}>
      <div className={styles.commitTitleRow}>
        <h1 className={styles.commitTitle}>{summary}</h1>
        <Button size="sm" leadingIcon={CodeIcon} onClick={() => navigate(treeUrl(repoRefOf(repo), c.sha))}>
          Browse files
        </Button>
      </div>
      {body && <pre className={styles.commitBody}>{body}</pre>}
      <div className={styles.commitMeta}>
        <span className={styles.commitPeople}>
          <Person person={author} />
          {sameCommitter ? (
            <span>
              committed <RelativeTime date={c.commit.committer.date} />
            </span>
          ) : (
            <>
              <span>
                authored <RelativeTime date={c.commit.author.date} /> and
              </span>
              <Person person={committer} />
              <span>
                committed <RelativeTime date={c.commit.committer.date} />
              </span>
            </>
          )}
          <CiIcon status={ci.data?.statuses[c.sha]} />
          <SignatureBadge signature={sigs[c.sha]} size="md" />
        </span>
        <span className={styles.commitRefs}>
          <span>
            {c.parents.length === 0 ? 'No parents' : `${c.parents.length} parent${c.parents.length === 1 ? '' : 's'}`}{' '}
            {c.parents.map((p, i) => (
              <span key={p.sha}>
                {i > 0 && ' + '}
                <Link to={`${base}/commit/${p.sha}`} className={styles.monoLink}>
                  {p.sha.slice(0, 7)}
                </Link>
              </span>
            ))}
          </span>
          <span className={styles.fullSha}>
            <GitCommitIcon size={16} className={styles.muted} />
            commit {c.sha}
            <IconButton icon={CopyIcon} label="Copy full SHA" size="sm" onClick={() => copyText(c.sha, `Copied ${c.sha.slice(0, 7)}`)} />
          </span>
        </span>
      </div>
    </div>
  );
}

function Stats({ c }: { c: RestCommitDetail }) {
  if (!c.files && !c.stats) return null;
  const files = c.files?.length;
  const add = c.stats?.additions ?? c.files?.reduce((n, f) => n + f.additions, 0) ?? 0;
  const del = c.stats?.deletions ?? c.files?.reduce((n, f) => n + f.deletions, 0) ?? 0;
  return (
    <div className={styles.stats}>
      Showing
      {files !== undefined && (
        <strong>
          {files} changed file{files === 1 ? '' : 's'}
        </strong>
      )}
      with <span className={styles.add}>{add} addition{add === 1 ? '' : 's'}</span> and{' '}
      <span className={styles.del}>
        {del} deletion{del === 1 ? '' : 's'}
      </span>
    </div>
  );
}

/** The diff with inline commit comments; the general comment thread follows the last file. */
function Diff({ repo, refName, sha, parent }: { repo: Repo; refName: string; sha: string | undefined; parent?: string }) {
  const annotations = useCommitCommentAnnotations(repo, sha);
  // Highlighting, context expansion and image diffs (single-parent commits only).
  const source = useMemo<DiffSource | undefined>(() => (sha && parent ? { owner: repo.owner, repo: repo.name, oldRef: parent, newRef: sha } : undefined), [repo.owner, repo.name, sha, parent]);
  const thread = <CommitCommentThread repo={repo} sha={sha} />;
  const { data, error } = useResource<string>(codeKeys.commitDiff(repo.owner, repo.name, refName), () => getCommitDiff(repo.owner, repo.name, refName), {
    immutable: isSha(refName),
  });
  if (error && data === undefined) {
    const tooLarge = error instanceof ApiError && error.status === 406;
    return (
      <div className={commentStyles.standalone}>
        <EmptyState icon={AlertIcon} title={tooLarge ? 'This diff is too large to display' : 'Couldn’t load the diff'} />
        {thread}
      </div>
    );
  }
  if (data === undefined) {
    return (
      <div className={styles.loading}>
        <Spinner /> Loading diff…
      </div>
    );
  }
  if (!data.trim()) {
    return (
      <div className={commentStyles.standalone}>
        <EmptyState icon={GitCommitIcon} title="No changes in this commit" />
        {thread}
      </div>
    );
  }
  return (
    <div className={styles.diff}>
      <DiffViewer diff={data} annotations={annotations} footer={thread} source={source} />
    </div>
  );
}

function HeaderSkeleton() {
  return (
    <div className={styles.commitHeader} aria-busy="true">
      <Skeleton width="50%" height={24} />
      <div className={styles.commitMeta}>
        <Skeleton width={220} />
        <Skeleton width={260} />
      </div>
    </div>
  );
}

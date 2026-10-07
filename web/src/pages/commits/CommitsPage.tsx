import { observer } from 'mobx-react-lite';
import { useEffect, useMemo, useReducer, useState } from 'react';
import { load, peek } from '../../api/cache';
import { ApiError } from '../../api/client';
import { codeKeys, getCommitStatuses, type CommitStatusRollup, type CommitStatuses } from '../../api/code';
import { getHistory, isSha } from '../../api/endpoints';
import { usePager } from '../../api/pager';
import type { BrowseCommit, History } from '../../api/types';
import { RefPicker } from '../../components/code/RefPicker';
import { LoadMore } from '../../components/LoadMore';
import { Link, navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import type { Repo } from '../../sync/models';
import { repoByName } from '../../sync/selectors';
import { IconButton, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, CodeIcon, CopyIcon, GitCommitIcon, HistoryIcon, KebabHorizontalIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { VirtualList } from '../../ui/VirtualList';
import { useRefsData } from '../code/data';
import { COMMITS_PER_PAGE } from '../code/prefetch';
import { historyUrl, repoRefOf, treeUrl } from '../../components/code/urls';
import { resolveTarget } from '../code/util';
import { commitDate, groupByDay, splitMessage, type CommitListRow } from './group';
import { CiIcon, Person, copyText, samePerson } from './parts';
import { SignatureBadge, useSignatures, type CommitSignature } from './Signature';
import styles from './Commits.module.css';

/** Commits list / file history: `/:owner/:repo/commits[/:ref/*path]`. */
export default observer(function CommitsPage() {
  const params = useParams<{ owner: string; repo: string; ref?: string; '*'?: string }>();
  const repo = repoByName(params.owner, params.repo);
  // Splits `{ref}/{path}` once the ref list arrives (refs may contain slashes).
  useRefsData(params.owner, params.repo);
  if (!repo) return null;
  // The history fetch keeps the URL's split (the server resolves the joined
  // spec itself, and the route prefetch uses the same key); the header shows
  // the resolved ref and path.
  const ref = params.ref || repo.defaultBranch;
  const path = (params['*'] ?? '').replace(/\/+$/, '');
  const t = resolveTarget(repo.owner, repo.name, ref, path);
  return (
    <div className={styles.page}>
      <Header repo={repo} refName={t.ref} path={t.path} />
      {/* Remount per ref/path: page state (loaded pages, cursor) starts fresh. */}
      <CommitList key={`${ref}:${path}`} repo={repo} refName={ref} path={path} label={`${t.ref}${t.path ? `:${t.path}` : ''}`} />
    </div>
  );
});

function Header({ repo, refName, path }: { repo: Repo; refName: string; path: string }) {
  const parts = path ? path.split('/') : [];
  return (
    <div className={styles.header}>
      <RefPicker owner={repo.owner} repo={repo.name} value={refName} onSelect={(r) => navigate(historyUrl(repoRefOf(repo), r, path))} />
      {path ? (
        <h1 className={styles.title}>
          <HistoryIcon size={16} className={styles.muted} />
          <span>History for</span>
          <nav className={styles.crumbs} aria-label="Path">
            <Link to={historyUrl(repoRefOf(repo), refName)}>{repo.name}</Link>
            {parts.map((p, i) => (
              <span key={i}>
                <span className={styles.sep}>/</span>
                {i === parts.length - 1 ? <strong>{p}</strong> : <Link to={historyUrl(repoRefOf(repo), refName, parts.slice(0, i + 1).join('/'))}>{p}</Link>}
              </span>
            ))}
          </nav>
        </h1>
      ) : (
        <h1 className={styles.title}>
          <GitCommitIcon size={16} className={styles.muted} />
          Commits
        </h1>
      )}
    </div>
  );
}

/** History pages of `ref`/`path`; pages still in the cache (back navigation) are restored on mount. */
function useHistoryPages(repo: Repo, ref: string, path: string) {
  return usePager<History>(`${repo.owner}/${repo.name}@${ref}:${path}`, {
    key: (page) => codeKeys.history(repo.owner, repo.name, ref, path, page),
    loader: (page) => getHistory(repo.owner, repo.name, ref, path, { page, perPage: COMMITS_PER_PAGE }),
    hasMore: (last) => !!last.has_more,
    immutable: isSha(ref),
  });
}

/** One batched commit-status request per loaded page, merged. */
function useCiStatuses(repo: Repo, pages: History[]): Record<string, CommitStatusRollup> {
  const [, bump] = useReducer((x: number) => x + 1, 0);
  const shaLists = pages.map((p) => p.commits.map((c) => c.sha)).filter((l) => l.length > 0);
  const keys = shaLists.map((l) => codeKeys.statuses(repo.owner, repo.name, l));
  const joined = keys.join('|');
  useEffect(() => {
    let alive = true;
    shaLists.forEach((shas, i) => {
      load(keys[i]!, () => getCommitStatuses(repo.owner, repo.name, shas)).then(
        () => alive && bump(),
        () => undefined,
      );
    });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed by the joined cache keys
  }, [joined]);
  const merged: Record<string, CommitStatusRollup> = {};
  for (const k of keys) Object.assign(merged, peek<CommitStatuses>(k)?.statuses);
  return merged;
}

type Row = CommitListRow | { kind: 'more' };

function CommitList({ repo, refName, path, label }: { repo: Repo; refName: string; path: string; label: string }) {
  const pager = useHistoryPages(repo, refName, path);
  const { first, pages, hasMore, loadMore } = pager;
  const ci = useCiStatuses(repo, pages);
  const sigs = useSignatures(
    repo.owner,
    repo.name,
    pages.map((p) => p.commits.map((c) => c.sha)),
  );
  const [cursor, setCursor] = useState(-1);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());

  const commits = useMemo(() => pages.flatMap((p) => p.commits), [pages]);
  const grouped = useMemo(() => groupByDay(commits), [commits]);
  const rows = useMemo<Row[]>(() => (hasMore ? [...grouped, { kind: 'more' }] : grouped), [grouped, hasMore]);
  const commitRows = useMemo(() => {
    const idx: number[] = [];
    rows.forEach((r, i) => r.kind === 'commit' && idx.push(i));
    return idx;
  }, [rows]);
  const count = commitRows.length;
  const current = cursor >= 0 && cursor < count ? (rows[commitRows[cursor]!] as Extract<Row, { kind: 'commit' }>).commit : undefined;
  const base = `/${repo.owner}/${repo.name}`;

  const move = (d: number) => {
    if (!count) return;
    const next = Math.min(count - 1, Math.max(0, cursor + d));
    setCursor(next);
    if (next >= count - 3 && hasMore) loadMore();
  };
  useShortcuts('Commits', {
    j: { handler: () => move(1), description: 'Next commit', group: 'Commits' },
    k: { handler: () => move(-1), description: 'Previous commit', group: 'Commits' },
    enter: { handler: () => (current ? navigate(`${base}/commit/${current.sha}`) : false), description: 'Open commit', group: 'Commits' },
    o: { handler: () => (current ? navigate(`${base}/commit/${current.sha}`) : false), description: 'Open commit', group: 'Commits' },
    y: { handler: () => (current ? copyText(current.sha, `Copied ${current.sha.slice(0, 7)}`) : false), description: 'Copy commit SHA', group: 'Commits' },
  });

  if (first.error && !first.data) {
    const missing = first.error instanceof ApiError && (first.error.status === 404 || first.error.status === 422);
    return (
      <EmptyState icon={AlertIcon} title={missing ? 'Nothing to show' : 'Couldn’t load the commit history'}>
        {missing ? `${label} doesn’t exist in this repository.` : 'Try again in a moment.'}
      </EmptyState>
    );
  }
  if (!first.data) return <SkeletonRows />;
  if (!rows.length) return <EmptyState icon={GitCommitIcon} title="No commits found" />;

  const toggle = (sha: string) =>
    setExpanded((s) => {
      const n = new Set(s);
      if (n.has(sha)) n.delete(sha);
      else n.add(sha);
      return n;
    });

  return (
    <VirtualList
      className={styles.list}
      items={rows}
      estimateSize={56}
      activeIndex={cursor >= 0 ? commitRows[cursor] : undefined}
      aria-label="Commits"
      getKey={(r) => (r.kind === 'commit' ? r.commit.sha : r.kind === 'day' ? `day:${r.key}` : 'more')}
      renderItem={(r, i) => {
        if (r.kind === 'day') {
          return (
            <div className={styles.day}>
              <GitCommitIcon size={16} className={styles.dayIcon} />
              {r.label}
            </div>
          );
        }
        if (r.kind === 'more') return <LoadMore pager={pager} auto className={styles.more} loadingLabel="Loading more commits…" />;
        return (
          <CommitRow
            repo={repo}
            commit={r.commit}
            ci={ci[r.commit.sha]}
            signature={sigs[r.commit.sha]}
            active={r.index === cursor}
            first={rows[i - 1]?.kind !== 'commit'}
            last={rows[i + 1]?.kind !== 'commit'}
            expanded={expanded.has(r.commit.sha)}
            onToggle={() => toggle(r.commit.sha)}
            onPointer={() => setCursor(r.index)}
          />
        );
      }}
    />
  );
}

function CommitRow({
  repo,
  commit: c,
  ci,
  signature,
  active,
  first,
  last,
  expanded,
  onToggle,
  onPointer,
}: {
  repo: Repo;
  commit: BrowseCommit;
  ci: CommitStatusRollup | undefined;
  signature: CommitSignature | undefined;
  active: boolean;
  first: boolean;
  last: boolean;
  expanded: boolean;
  onToggle: () => void;
  onPointer: () => void;
}) {
  const base = `/${repo.owner}/${repo.name}`;
  const { body } = splitMessage(c.message);
  const summary = c.summary || splitMessage(c.message).summary;
  return (
    <div className={cx(styles.row, first && styles.first, last && styles.last, active && styles.active)} role="listitem" aria-current={active || undefined} onPointerDown={onPointer}>
      <div className={styles.main}>
        <div className={styles.summaryLine}>
          <Link to={`${base}/commit/${c.sha}`} className={styles.summary} title={summary}>
            {summary}
          </Link>
          {body && (
            <button type="button" className={styles.expand} aria-expanded={expanded} aria-label={expanded ? 'Hide commit message' : 'Show commit message'} onClick={onToggle}>
              <KebabHorizontalIcon size={12} />
            </button>
          )}
        </div>
        {expanded && body && <pre className={styles.body}>{body}</pre>}
        <div className={styles.meta}>
          <Person person={c.author} size={16} />
          {samePerson(c.author, c.committer) ? (
            <span>
              committed <RelativeTime date={commitDate(c)} />
            </span>
          ) : (
            <span>
              authored <RelativeTime date={c.author.date} /> · <Person person={c.committer} avatar={false} /> committed <RelativeTime date={commitDate(c)} />
            </span>
          )}
          <CiIcon status={ci} size={14} />
        </div>
      </div>
      <div className={styles.actions}>
        {signature ? <SignatureBadge signature={signature} /> : <span className={styles.verifySlot} aria-hidden />}
        <span className={styles.shaGroup}>
          <Link to={`${base}/commit/${c.sha}`} className={styles.sha} title={c.sha}>
            {c.sha.slice(0, 7)}
          </Link>
          <IconButton icon={CopyIcon} label="Copy full SHA" size="sm" onClick={() => copyText(c.sha, `Copied ${c.sha.slice(0, 7)}`)} />
        </span>
        <Link to={treeUrl(repoRefOf(repo), c.sha)} className={styles.browse} aria-label="Browse repository at this point in the history" title="Browse repository at this point in the history">
          <CodeIcon size={16} />
        </Link>
      </div>
    </div>
  );
}

function SkeletonRows() {
  return (
    <div className={styles.list} aria-busy="true">
      <div className={styles.day}>
        <Skeleton width={180} />
      </div>
      {Array.from({ length: 8 }, (_, i) => (
        <div key={i} className={cx(styles.row, i === 0 && styles.first, i === 7 && styles.last)}>
          <div className={styles.main}>
            <Skeleton width={`${40 + ((i * 17) % 40)}%`} />
            <Skeleton width={160} height={12} style={{ marginTop: 8 }} />
          </div>
        </div>
      ))}
    </div>
  );
}

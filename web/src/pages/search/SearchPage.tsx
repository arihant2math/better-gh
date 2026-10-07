import { observer } from 'mobx-react-lite';
import { Fragment, useEffect, useMemo, useState, type CSSProperties } from 'react';
import { ApiError } from '../../api/client';
import { Link, navigate, setQuery, useQuery } from '../../router';
import {
  search,
  searchCount,
  type SearchCodeItem,
  type SearchCommitItem,
  type SearchIssueItem,
  type SearchItemMap,
  type SearchPage as Page,
  type SearchRepoItem,
  type SearchUserItem,
} from '../../search/api';
import { recordPerf } from '../../search/perf';
import type { SearchType } from '../../search/qualifiers';
import { QueryInput } from '../../search/QueryInput';
import { storeValueSource } from '../../search/storeSource';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Avatar, StateIcon } from '../../ui/Badge';
import { Button, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import {
  CodeIcon,
  CommentIcon,
  FileCodeIcon,
  GitCommitIcon,
  GitPullRequestIcon,
  IssueOpenedIcon,
  LockIcon,
  PersonIcon,
  RepoIcon,
  SearchIcon,
  StarIcon,
  type Icon,
} from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Select } from '../../ui/Input';
import { resultsHeading } from './heading';
import { fragmentLines, highlightRanges, toPath } from './highlight';
import styles from './SearchPage.module.css';

const TYPES: { id: SearchType; label: string; icon: Icon }[] = [
  { id: 'code', label: 'Code', icon: CodeIcon },
  { id: 'repositories', label: 'Repositories', icon: RepoIcon },
  { id: 'issues', label: 'Issues', icon: IssueOpenedIcon },
  { id: 'pulls', label: 'Pull requests', icon: GitPullRequestIcon },
  { id: 'users', label: 'Users', icon: PersonIcon },
  { id: 'commits', label: 'Commits', icon: GitCommitIcon },
];

const SORTS: Record<SearchType, { id: string; label: string }[]> = {
  code: [{ id: '', label: 'Best match' }],
  repositories: [
    { id: '', label: 'Best match' },
    { id: 'stars', label: 'Most stars' },
    { id: 'forks', label: 'Most forks' },
    { id: 'updated', label: 'Recently updated' },
  ],
  issues: [
    { id: '', label: 'Best match' },
    { id: 'created', label: 'Newest' },
    { id: 'created-asc', label: 'Oldest' },
    { id: 'updated', label: 'Recently updated' },
    { id: 'comments', label: 'Most commented' },
  ],
  pulls: [
    { id: '', label: 'Best match' },
    { id: 'created', label: 'Newest' },
    { id: 'created-asc', label: 'Oldest' },
    { id: 'updated', label: 'Recently updated' },
    { id: 'comments', label: 'Most commented' },
  ],
  users: [
    { id: '', label: 'Best match' },
    { id: 'followers', label: 'Most followers' },
    { id: 'joined', label: 'Most recently joined' },
  ],
  commits: [
    { id: '', label: 'Best match' },
    { id: 'author-date', label: 'Recently authored' },
    { id: 'committer-date', label: 'Recently committed' },
  ],
};

const PER_PAGE = 25;
const MAX_RESULTS = 1000;

function isType(t: string | null): t is SearchType {
  return TYPES.some((x) => x.id === t);
}

// ------------------------------------------------------------------ data

interface Loaded<T> {
  data?: Page<T>;
  error?: string;
  loading: boolean;
}

const pageCache = new Map<string, Page<unknown>>();
const countCache = new Map<string, number>();

/** One search request per (type, q, page, sort); aborted when the inputs change; cached for back/forward. */
function useSearch<T extends SearchType>(type: T, q: string, page: number, sort: string): Loaded<SearchItemMap[T]> {
  const key = `${type}|${q}|${page}|${sort}`;
  const cached = pageCache.get(key) as Page<SearchItemMap[T]> | undefined;
  const [state, setState] = useState<{ key: string; data?: Page<SearchItemMap[T]>; error?: string }>({ key: '' });
  useEffect(() => {
    if (!q || pageCache.has(key)) return;
    const ctrl = new AbortController();
    const [s, o] = sort.endsWith('-asc') ? [sort.slice(0, -4), 'asc' as const] : [sort, sort ? ('desc' as const) : undefined];
    const t0 = performance.now();
    search(type, { q, page, perPage: PER_PAGE, sort: s || undefined, order: o }, ctrl.signal).then(
      (data) => {
        recordPerf(`search.${type}`, performance.now() - t0);
        pageCache.set(key, data);
        if (pageCache.size > 60) pageCache.delete(pageCache.keys().next().value!);
        if (!ctrl.signal.aborted) setState({ key, data });
      },
      (e: unknown) => {
        if (!ctrl.signal.aborted) setState({ key, error: e instanceof ApiError ? e.message : 'Search failed' });
      },
    );
    return () => ctrl.abort();
  }, [key, q, type, page, sort]);
  if (!q) return { loading: false };
  if (cached) return { data: cached, loading: false };
  if (state.key === key) return { data: state.data, error: state.error, loading: false };
  return { loading: true };
}

function useCounts(q: string): Partial<Record<SearchType, number>> {
  const [, rerender] = useState(0);
  useEffect(() => {
    if (!q) return;
    const ctrl = new AbortController();
    for (const t of TYPES) {
      const key = `${t.id}|${q}`;
      if (countCache.has(key)) continue;
      searchCount(t.id, q, ctrl.signal).then(
        (n) => {
          countCache.set(key, n);
          if (!ctrl.signal.aborted) rerender((x) => x + 1);
        },
        () => undefined,
      );
    }
    return () => ctrl.abort();
  }, [q]);
  const out: Partial<Record<SearchType, number>> = {};
  for (const t of TYPES) {
    const n = countCache.get(`${t.id}|${q}`);
    if (n !== undefined) out[t.id] = n;
  }
  return out;
}

function compact(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}k` : String(n);
}

// ------------------------------------------------------------------ page

/** `/search?q=&type=&p=&s=` — GitHub-style results with tabs, qualifier autocomplete and pagination. */
export default observer(function SearchPage() {
  const params = useQuery();
  const q = (params.get('q') ?? '').trim();
  const type: SearchType = isType(params.get('type')) ? (params.get('type') as SearchType) : 'issues';
  const page = Math.max(1, Number(params.get('p')) || 1);
  const sort = params.get('s') ?? '';
  const [draft, setDraft] = useState(q);
  const [prevQ, setPrevQ] = useState(q);
  if (prevQ !== q) {
    setPrevQ(q);
    setDraft(q);
  }
  const source = useMemo(() => storeValueSource(), []);
  const counts = useCounts(q);
  const result = useSearch(type, q, page, sort);
  const [cursor, setCursor] = useState(0);
  const items = (result.data?.items ?? []) as unknown[];

  const submit = (next: string) => {
    if (!next) return;
    navigate(`/search?q=${encodeURIComponent(next)}&type=${type}`);
  };

  const hrefOf = (item: unknown): string => {
    switch (type) {
      case 'repositories':
        return `/${(item as SearchRepoItem).full_name}`;
      case 'users':
        return `/${(item as SearchUserItem).login}`;
      case 'code': {
        const c = item as SearchCodeItem;
        const line = c.line_numbers?.[0];
        return toPath(c.html_url) + (line ? `#L${line}` : '');
      }
      default:
        return toPath((item as SearchIssueItem | SearchCommitItem).html_url);
    }
  };

  const total = result.data?.total_count ?? 0;
  const pages = Math.max(1, Math.ceil(Math.min(total, MAX_RESULTS) / PER_PAGE));
  const goPage = (p: number) => {
    if (p < 1 || p > pages || p === page) return;
    setQuery({ p: p === 1 ? null : String(p) });
    setCursor(0);
    document.getElementById('content')?.scrollTo({ top: 0 });
  };

  useShortcuts('Search results', {
    j: { handler: () => setCursor((c) => Math.min(items.length - 1, c + 1)), description: 'Next result', group: 'Search' },
    k: { handler: () => setCursor((c) => Math.max(0, c - 1)), description: 'Previous result', group: 'Search' },
    enter: { handler: () => (items[cursor] ? navigate(hrefOf(items[cursor])) : false), description: 'Open result', group: 'Search' },
    'shift+arrowright': { handler: () => goPage(page + 1), description: 'Next page', group: 'Search' },
    'shift+arrowleft': { handler: () => goPage(page - 1), description: 'Previous page', group: 'Search' },
  });


  const qualifierSet = type === 'pulls' ? 'pulls' : type;

  return (
    <div className={styles.page}>
      <form
        className={styles.searchRow}
        role="search"
        onSubmit={(e) => {
          e.preventDefault();
          submit(draft.trim());
        }}
      >
        <QueryInput
          className={styles.search}
          size="lg"
          set={qualifierSet}
          source={source}
          value={draft}
          onChange={setDraft}
          onSubmit={submit}
          autoFocus={!q}
          leadingIcon={SearchIcon}
          placeholder={type === 'code' ? 'Search code — e.g. "connection pool" language:rust repo:acme/api' : 'Search — e.g. is:open label:bug author:@me'}
          aria-label="Search"
        />
        <Button type="submit" variant="primary" size="lg">
          Search
        </Button>
      </form>
      <div className={styles.body}>
        <nav className={styles.types} aria-label="Result types">
          <div className={styles.typesTitle}>Filter by</div>
          {TYPES.map((t) => (
            <Link
              key={t.id}
              to={`/search?q=${encodeURIComponent(q)}&type=${t.id}`}
              className={styles.typeLink}
              aria-current={t.id === type ? 'page' : undefined}
            >
              <t.icon size={16} />
              <span className={styles.typeLabel}>{t.label}</span>
              {q && <span className={styles.typeCount}>{counts[t.id] !== undefined ? compact(counts[t.id]!) : '·'}</span>}
            </Link>
          ))}
          <QualifierHelp type={type} onInsert={(tok) => setDraft((d) => `${d.trim()} ${tok}`.trim())} />
        </nav>
        <main className={styles.results} aria-busy={result.loading}>
          {!q ? (
            <EmptyState icon={SearchIcon} title="Search Better GitHub">
              Find code, repositories, issues, pull requests, people and commits. Use qualifiers like <code>repo:</code>, <code>is:open</code>, <code>label:</code>, <code>author:@me</code>,{' '}
              <code>language:</code>. Press <kbd>Tab</kbd> to complete them.
            </EmptyState>
          ) : (
            <>
              <header className={styles.resultsHeader}>
                <h1 className={styles.resultsTitle}>
                  {result.data ? resultsHeading(type, total) : result.loading ? 'Searching…' : ''}
                  {result.data?.incomplete_results && <span className={styles.incomplete}> (incomplete)</span>}
                </h1>
                {SORTS[type].length > 1 && (
                  <Select aria-label="Sort" value={sort} onChange={(e) => setQuery({ s: e.target.value || null, p: null })}>
                    {SORTS[type].map((s) => (
                      <option key={s.id} value={s.id}>
                        Sort: {s.label}
                      </option>
                    ))}
                  </Select>
                )}
              </header>
              {result.error ? (
                <EmptyState icon={SearchIcon} title="We couldn’t perform that search">
                  {result.error}
                </EmptyState>
              ) : result.loading && !result.data ? (
                <ResultSkeleton />
              ) : items.length === 0 ? (
                <EmptyState icon={SearchIcon} title={`No ${TYPES.find((t) => t.id === type)!.label.toLowerCase()} matched “${q}”`}>
                  Try a different type on the left, or remove some qualifiers.
                </EmptyState>
              ) : (
                <ol className={styles.list}>
                  {items.map((item, i) => (
                    <li key={i} className={cx(styles.item, i === cursor && styles.itemActive)} onPointerEnter={() => setCursor(i)}>
                      <ResultItem type={type} item={item} href={hrefOf(item)} />
                    </li>
                  ))}
                </ol>
              )}
              {pages > 1 && <Pagination page={page} pages={pages} onPage={goPage} />}
            </>
          )}
        </main>
      </div>
    </div>
  );
});

function ResultSkeleton() {
  return (
    <div className={styles.list}>
      {Array.from({ length: 6 }, (_, i) => (
        <div key={i} className={styles.item}>
          <Skeleton width="40%" />
          <Skeleton width="85%" style={{ marginTop: 8 }} />
        </div>
      ))}
    </div>
  );
}

function Pagination({ page, pages, onPage }: { page: number; pages: number; onPage: (p: number) => void }) {
  const nums: (number | '…')[] = [];
  for (let p = 1; p <= pages; p++) {
    if (p === 1 || p === pages || Math.abs(p - page) <= 2) nums.push(p);
    else if (nums[nums.length - 1] !== '…') nums.push('…');
  }
  return (
    <nav className={styles.pagination} aria-label="Pagination">
      <Button size="sm" variant="ghost" disabled={page <= 1} onClick={() => onPage(page - 1)}>
        ‹ Previous
      </Button>
      {nums.map((n, i) =>
        n === '…' ? (
          <span key={`e${i}`} className={styles.gap}>
            …
          </span>
        ) : (
          <button key={n} type="button" className={styles.pageNum} aria-current={n === page ? 'page' : undefined} onClick={() => onPage(n)}>
            {n}
          </button>
        ),
      )}
      <Button size="sm" variant="ghost" disabled={page >= pages} onClick={() => onPage(page + 1)}>
        Next ›
      </Button>
    </nav>
  );
}

const EXAMPLES: Record<SearchType, string[]> = {
  code: ['language:rust', 'path:src/', 'extension:ts', 'repo:', 'NOT test'],
  repositories: ['stars:>10', 'language:go', 'is:private', 'topic:', 'pushed:>2026-01-01'],
  issues: ['is:open', 'label:bug', 'author:@me', 'assignee:@me', 'no:assignee', 'comments:>5'],
  pulls: ['is:open', 'review-requested:@me', 'is:merged', 'draft:true', 'base:main'],
  users: ['type:user', 'type:org', 'followers:>10', 'repos:>5'],
  commits: ['author:@me', 'merge:false', 'author-date:>2026-01-01'],
};

function QualifierHelp({ type, onInsert }: { type: SearchType; onInsert: (tok: string) => void }) {
  return (
    <div className={styles.help}>
      <div className={styles.typesTitle}>Qualifiers</div>
      <div className={styles.examples}>
        {EXAMPLES[type].map((e) => (
          <button key={e} type="button" className={styles.example} onClick={() => onInsert(e)}>
            {e}
          </button>
        ))}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ result renderers

function ResultItem({ type, item, href }: { type: SearchType; item: unknown; href: string }) {
  switch (type) {
    case 'repositories':
      return <RepoResult r={item as SearchRepoItem} href={href} />;
    case 'users':
      return <UserResult u={item as SearchUserItem} href={href} />;
    case 'code':
      return <CodeResult c={item as SearchCodeItem} href={href} />;
    case 'commits':
      return <CommitResult c={item as SearchCommitItem} href={href} />;
    default:
      return <IssueResult i={item as SearchIssueItem} href={href} />;
  }
}

function repoFromUrl(url: string): string {
  const m = /\/repos\/([^/]+\/[^/?#]+)/.exec(url);
  return m ? m[1]! : '';
}

function IssueResult({ i, href }: { i: SearchIssueItem; href: string }) {
  const isPr = !!i.pull_request;
  const repo = repoFromUrl(i.repository_url);
  const body = i.text_matches?.find((m) => m.property === 'body' && m.matches.length);
  const title = i.text_matches?.find((m) => m.property === 'title' && m.matches.length);
  return (
    <div className={styles.issue}>
      <span className={styles.lead}>
        <StateIcon issue={{ isPr, state: i.state, stateReason: (i.state_reason as 'not_planned' | null) ?? null, merged: !!i.pull_request?.merged_at, draft: i.draft }} />
      </span>
      <div className={styles.main}>
        <div className={styles.repoLine}>
          <Link to={`/${repo}`} className={cx(styles.subtle, styles.ellipsis)}>
            {repo}
          </Link>
          <span className={styles.subtle}>#{i.number}</span>
        </div>
        <Link to={href} className={styles.title}>
          {title && title.fragment === i.title ? highlightRanges(i.title, title.matches.map((m) => m.indices)) : i.title}
        </Link>
        {body && <p className={styles.snippet}>{highlightRanges(body.fragment, body.matches.map((m) => m.indices))}</p>}
        <div className={styles.meta}>
          {i.labels.slice(0, 4).map((l) => (
            <span key={l.id} className={styles.label} style={{ '--c': `#${l.color}` } as CSSProperties}>
              {l.name}
            </span>
          ))}
          {i.user && (
            <span>
              <Avatar user={{ login: i.user.login, avatarUrl: i.user.avatar_url }} size={14} /> <span className={styles.ellipsis}>{i.user.login}</span>
            </span>
          )}
          <span>
            updated <RelativeTime date={i.updated_at} />
          </span>
          {i.comments > 0 && (
            <span>
              <CommentIcon size={12} /> {i.comments}
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function RepoResult({ r, href }: { r: SearchRepoItem; href: string }) {
  const desc = r.text_matches?.find((m) => m.property === 'description' && m.matches.length);
  return (
    <div className={styles.issue}>
      <span className={styles.lead}>{r.private ? <LockIcon size={16} /> : <RepoIcon size={16} />}</span>
      <div className={styles.main}>
        <Link to={href} className={styles.title}>
          {r.full_name}
        </Link>
        {r.description && <p className={styles.snippet}>{desc && desc.fragment === r.description ? highlightRanges(r.description, desc.matches.map((m) => m.indices)) : r.description}</p>}
        {r.topics && r.topics.length > 0 && (
          <div className={styles.topics}>
            {r.topics.slice(0, 6).map((t) => (
              <Link key={t} to={`/search?q=${encodeURIComponent(`topic:${t}`)}&type=repositories`} className={styles.topic}>
                {t}
              </Link>
            ))}
          </div>
        )}
        <div className={styles.meta}>
          {r.language && <span>{r.language}</span>}
          <span>
            <StarIcon size={12} /> {r.stargazers_count ?? 0}
          </span>
          {r.updated_at && (
            <span>
              Updated <RelativeTime date={r.pushed_at ?? r.updated_at} />
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

function UserResult({ u, href }: { u: SearchUserItem; href: string }) {
  return (
    <div className={styles.issue}>
      <span className={styles.lead}>
        <Avatar user={{ login: u.login, avatarUrl: u.avatar_url }} size={32} square={u.type === 'Organization'} />
      </span>
      <div className={styles.main}>
        <Link to={href} className={styles.title}>
          {u.name ? (
            <>
              {u.name} <span className={styles.subtle}>{u.login}</span>
            </>
          ) : (
            u.login
          )}
        </Link>
        {u.bio && <p className={styles.snippet}>{u.bio}</p>}
        <div className={styles.meta}>{u.type === 'Organization' ? 'Organization' : u.type === 'Bot' ? 'Bot' : 'User'}</div>
      </div>
    </div>
  );
}

function CodeResult({ c, href }: { c: SearchCodeItem; href: string }) {
  const m = c.text_matches?.find((x) => x.property === 'content');
  const first = c.line_numbers?.[0] ? Number(c.line_numbers[0]) : undefined;
  const lines = m ? fragmentLines(m, first) : [];
  const base = toPath(c.html_url);
  return (
    <div className={styles.code}>
      <div className={styles.codeHeader}>
        <FileCodeIcon size={16} />
        <Link to={`/${c.repository.full_name}`} className={cx(styles.subtle, styles.ellipsis)}>
          {c.repository.full_name}
        </Link>
        <span className={styles.subtle}>·</span>
        <Link to={href} className={styles.path}>
          {c.path}
        </Link>
        {c.language && <span className={styles.lang}>{c.language}</span>}
      </div>
      {lines.length > 0 && (
        <pre className={styles.codeBody}>
          {lines.map((l, idx) => (
            <Fragment key={`${l.number}-${idx}`}>
              {idx > 0 && l.number > lines[idx - 1]!.number + 1 && (
                <span className={styles.codeGap} aria-hidden>
                  ⋯
                </span>
              )}
              <Link to={`${base}#L${l.number}`} className={cx(styles.codeLine, l.ranges.length > 0 && styles.codeHit)}>
                <span className={styles.lineNo}>{l.number}</span>
                <code>{l.ranges.length ? highlightRanges(l.text, l.ranges, styles.mark) : l.text || ' '}</code>
              </Link>
            </Fragment>
          ))}
        </pre>
      )}
    </div>
  );
}

function CommitResult({ c, href }: { c: SearchCommitItem; href: string }) {
  const [subject, ...rest] = c.commit.message.split('\n');
  const tm = c.text_matches?.find((m) => m.property === 'message' && m.fragment === subject);
  return (
    <div className={styles.issue}>
      <span className={styles.lead}>
        <GitCommitIcon size={16} />
      </span>
      <div className={styles.main}>
        <div className={styles.repoLine}>
          <Link to={`/${c.repository.full_name}`} className={cx(styles.subtle, styles.ellipsis)}>
            {c.repository.full_name}
          </Link>
        </div>
        <Link to={href} className={styles.title}>
          {tm ? highlightRanges(subject!, tm.matches.map((m) => m.indices)) : subject}
        </Link>
        {rest.join('\n').trim() && <p className={styles.snippet}>{rest.join('\n').trim().slice(0, 200)}</p>}
        <div className={styles.meta}>
          {c.author ? (
            <span>
              <Avatar user={{ login: c.author.login, avatarUrl: c.author.avatar_url }} size={14} /> <span className={styles.ellipsis}>{c.author.login}</span>
            </span>
          ) : (
            <span className={styles.ellipsis}>{c.commit.author.name}</span>
          )}
          <span>
            committed <RelativeTime date={c.commit.author.date} />
          </span>
          <code className={styles.sha}>{c.sha.slice(0, 7)}</code>
        </div>
      </div>
    </div>
  );
}

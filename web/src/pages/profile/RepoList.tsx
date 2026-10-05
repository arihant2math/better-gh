import { observer } from 'mobx-react-lite';
import { useMemo, useRef, type ReactNode } from 'react';
import { Link, setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import { setStarred } from '../../sync/mutations';
import { Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Box, EmptyState, Skeleton } from '../../ui/EmptyState';
import { RepoForkedIcon, RepoIcon, RepoTemplateIcon, SearchIcon, StarFillIcon, StarIcon } from '../../ui/icons';
import { Input, Select } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { VirtualList } from '../../ui/VirtualList';
import styles from './ProfilePage.module.css';
import { filterRepos, languageColor, languagesOf, REPO_SORTS, REPO_TYPES, type RepoItem, type RepoSort, type RepoType } from './repoList';

const VIRTUALIZE_OVER = 100;

export function visibilityLabel(r: Pick<RepoItem, 'visibility' | 'archived' | 'isTemplate'>): string {
  const v = r.visibility === 'public' ? 'Public' : r.visibility === 'internal' ? 'Internal' : 'Private';
  return r.archived ? `${v} archive` : r.isTemplate ? `${v} template` : v;
}

/** Star / Unstar for repositories in the synced store (optimistic). */
export const StarButton = observer(function StarButton({ repoId }: { repoId: number }) {
  const repo = store().get('repo', repoId);
  const vr = store().get('viewerRepo', repoId);
  if (!repo || !vr) return null;
  const on = vr.starred;
  return (
    <Button size="sm" leadingIcon={on ? StarFillIcon : StarIcon} aria-pressed={on} onClick={() => setStarred(repo, !on)}>
      {on ? 'Starred' : 'Star'}
    </Button>
  );
});

function Highlight({ text, q }: { text: string; q: string }) {
  if (!q) return <>{text}</>;
  const i = text.toLowerCase().indexOf(q.toLowerCase());
  if (i < 0) return <>{text}</>;
  return (
    <>
      {text.slice(0, i)}
      <mark className={styles.highlight}>{text.slice(i, i + q.length)}</mark>
      {text.slice(i + q.length)}
    </>
  );
}

export function RepoMeta({ r }: { r: RepoItem }) {
  return (
    <div className={styles.meta}>
      {r.language && (
        <span>
          <span className={styles.langDot} style={{ background: languageColor(r.language) }} />
          {r.language}
        </span>
      )}
      {r.stars > 0 && (
        <span title="Stars">
          <StarIcon size={16} />
          {r.stars.toLocaleString()}
        </span>
      )}
      {r.forks > 0 && (
        <span title="Forks">
          <RepoForkedIcon size={16} />
          {r.forks.toLocaleString()}
        </span>
      )}
      {r.updatedAt && (
        <span>
          Updated <RelativeTime date={r.updatedAt} />
        </span>
      )}
    </div>
  );
}

function RepoRow({ r, q, showOwner }: { r: RepoItem; q: string; showOwner?: boolean }) {
  return (
    <div className={styles.repoRow} role="listitem">
      <div className={styles.repoRowMain}>
        <div className={styles.repoRowTitle}>
          <Link to={`/${r.owner}/${r.name}`}>
            {showOwner && `${r.owner} / `}
            <Highlight text={r.name} q={q} />
          </Link>
          <Tag>{visibilityLabel(r)}</Tag>
        </div>
        {r.description && <p className={styles.repoRowDesc}>{r.description}</p>}
        {r.topics.length > 0 && (
          <div className={styles.topics}>
            {r.topics.slice(0, 8).map((t) => (
              <span key={t} className={styles.topic}>
                {t}
              </span>
            ))}
          </div>
        )}
        <RepoMeta r={r} />
      </div>
      <div className={styles.repoRowSide}>{r.synced && <StarButton repoId={r.id} />}</div>
    </div>
  );
}

/** Card for "Popular repositories". */
export function RepoCard({ r }: { r: RepoItem }) {
  return (
    <div className={styles.repoCard}>
      <div className={styles.repoCardTitle}>
        {r.isTemplate ? <RepoTemplateIcon size={16} /> : <RepoIcon size={16} />}
        <Link to={`/${r.owner}/${r.name}`}>{r.name}</Link>
        <Tag>{visibilityLabel(r)}</Tag>
      </div>
      <p className={styles.repoCardDesc}>{r.description}</p>
      <RepoMeta r={r} />
    </div>
  );
}

export function ListSkeleton({ rows = 4 }: { rows?: number }) {
  return (
    <div className={styles.skelStack} aria-busy="true" aria-label="Loading">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className={styles.repoRow}>
          <div className={styles.repoRowMain}>
            <Skeleton width="30%" height={18} />
            <Skeleton width="70%" />
            <Skeleton width="40%" height={12} />
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * Searchable, filterable repository list (profile "Repositories" and
 * "Stars" tabs). Filters live in the query string like GitHub
 * (`?tab=repositories&q=&type=&language=&sort=`); `/` focuses the search.
 */
export function RepoList({
  items,
  loading,
  error,
  sorts = REPO_SORTS,
  defaultSort = 'updated',
  types = true,
  showOwner,
  emptyTitle,
  emptyBody,
  label,
}: {
  items: RepoItem[];
  loading: boolean;
  error?: ReactNode;
  sorts?: { id: RepoSort; label: string }[];
  defaultSort?: RepoSort;
  types?: boolean;
  showOwner?: boolean;
  emptyTitle: string;
  emptyBody?: ReactNode;
  label: string;
}) {
  const query = useQuery();
  const q = query.get('q') ?? '';
  const type = (types ? (query.get('type') as RepoType | null) : null) ?? 'all';
  const language = query.get('language') ?? '';
  const sortParam = query.get('sort') as RepoSort | null;
  const sort = sortParam && sorts.some((s) => s.id === sortParam) ? sortParam : defaultSort;
  const searchRef = useRef<HTMLInputElement>(null);

  useShortcuts('Repositories', {
    '/': {
      handler: () => {
        searchRef.current?.focus();
        searchRef.current?.select();
      },
      description: 'Find a repository',
      group: 'Profile',
    },
  });

  const languages = useMemo(() => languagesOf(items), [items]);
  const shown = useMemo(() => filterRepos(items, { q, type, language, sort }), [items, q, type, language, sort]);
  const filtered = !!q || type !== 'all' || !!language;

  const toolbar = (
    <div className={styles.toolbar} role="search">
      <div className={styles.toolbarSearch}>
        <Input
          ref={searchRef}
          type="search"
          leadingIcon={SearchIcon}
          placeholder={types ? 'Find a repository…' : 'Search stars'}
          aria-label={types ? 'Find a repository' : 'Search starred repositories'}
          value={q}
          onChange={(e) => setQuery({ q: e.target.value })}
          onKeyDown={(e) => {
            if (e.key === 'Escape' && q) {
              e.preventDefault();
              setQuery({ q: null });
            } else if (e.key === 'Escape') {
              e.currentTarget.blur();
            }
          }}
        />
      </div>
      {types && (
        <Select aria-label="Type" value={type} onChange={(e) => setQuery({ type: e.target.value === 'all' ? null : e.target.value })}>
          {REPO_TYPES.map((t) => (
            <option key={t.id} value={t.id}>
              Type: {t.label}
            </option>
          ))}
        </Select>
      )}
      <Select aria-label="Language" value={language} onChange={(e) => setQuery({ language: e.target.value || null })}>
        <option value="">Language: All</option>
        {languages.map((l) => (
          <option key={l} value={l}>
            {l}
          </option>
        ))}
      </Select>
      <Select aria-label="Sort" value={sort} onChange={(e) => setQuery({ sort: e.target.value === defaultSort ? null : e.target.value })}>
        {sorts.map((s) => (
          <option key={s.id} value={s.id}>
            Sort: {s.label}
          </option>
        ))}
      </Select>
    </div>
  );

  let body: ReactNode;
  if (items.length === 0 && loading) body = <ListSkeleton />;
  else if (items.length === 0 && error) body = <Box padded>{error}</Box>;
  else if (shown.length === 0)
    body = filtered ? (
      <EmptyState icon={SearchIcon} title="No repositories match">
        Try a different search or filter.
      </EmptyState>
    ) : (
      <EmptyState icon={types ? RepoIcon : StarIcon} title={emptyTitle}>
        {emptyBody}
      </EmptyState>
    );
  else if (shown.length > VIRTUALIZE_OVER)
    body = (
      <VirtualList
        className={styles.virtual}
        items={shown}
        getKey={(r) => r.id}
        estimateSize={120}
        aria-label={label}
        renderItem={(r) => <RepoRow r={r} q={q} showOwner={showOwner} />}
      />
    );
  else
    body = (
      <div className={styles.repoRows} role="list" aria-label={label}>
        {shown.map((r) => (
          <RepoRow key={r.id} r={r} q={q} showOwner={showOwner} />
        ))}
      </div>
    );

  return (
    <div>
      {toolbar}
      {filtered && (
        <div className={styles.resultLine} role="status">
          <span>
            <strong>{shown.length}</strong> {shown.length === 1 ? 'result' : 'results'}
            {type !== 'all' && (
              <>
                {' '}
                for <strong>{REPO_TYPES.find((t) => t.id === type)?.label.toLowerCase()}</strong>
              </>
            )}{' '}
            repositories
            {q && (
              <>
                {' '}
                matching <strong>{q}</strong>
              </>
            )}
            {language && (
              <>
                {' '}
                written in <strong>{language}</strong>
              </>
            )}
          </span>
          <Button size="sm" variant="ghost" onClick={() => setQuery({ q: null, type: null, language: null })}>
            Clear filter
          </Button>
        </div>
      )}
      {body}
    </div>
  );
}

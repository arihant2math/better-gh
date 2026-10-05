import { useState } from 'react';
import { useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { compareWiki, revertWikiPage, wikiKey } from '../../api/wiki';
import { DiffViewer } from '../../components/diff/DiffViewer';
import { Link, navigate, setQuery, useParams, useQuery } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { ArrowLeftIcon, FileDiffIcon, HistoryIcon, UndoIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import { invalidateWiki, useWikiIndex, useWikiPageHistory } from './data';
import styles from './Wiki.module.css';

/** `/:owner/:repo/wiki/:slug/history` — revisions, compare (`?compare=a...b`), revert. */
export default function WikiHistoryPage() {
  const { owner, repo, slug } = useParams<{ owner: string; repo: string; slug: string }>();
  const compare = useQuery().get('compare');
  const history = useWikiPageHistory(owner, repo, slug);
  const index = useWikiIndex(owner, repo);
  const [picked, setPicked] = useState<string[]>([]);
  const base = `/${owner}/${repo}/wiki`;
  const commits = history.data ?? [];
  const title = slug.replace(/-/g, ' ');

  const toggle = (sha: string) => setPicked((p) => (p.includes(sha) ? p.filter((x) => x !== sha) : [...p.slice(-1), sha]));
  const doCompare = () => {
    if (picked.length !== 2) return;
    // Older first (history is newest first).
    const [a, b] = [...picked].sort((x, y) => commits.findIndex((c) => c.sha === y) - commits.findIndex((c) => c.sha === x));
    setQuery({ compare: `${a}...${b}` }, { replace: false });
  };
  const revert = async (sha: string) => {
    try {
      await revertWikiPage(owner, repo, slug, sha);
      invalidateWiki(owner, repo);
      toast({ kind: 'success', title: `Reverted ${title} to ${sha.slice(0, 7)}` });
      navigate(`${base}/${slug}`);
    } catch (e) {
      toast({ kind: 'error', title: 'Revert failed', description: e instanceof ApiError ? e.message : String(e) });
    }
  };

  return (
    <div className={styles.history}>
      <header className={styles.pageHead}>
        <div>
          <Link to={`${base}/${slug}`} className={styles.back}>
            <ArrowLeftIcon size={14} /> {title}
          </Link>
          <h1 className={styles.h1}>
            <HistoryIcon size={20} /> History
          </h1>
        </div>
        {!compare && (
          <Button leadingIcon={FileDiffIcon} disabled={picked.length !== 2} onClick={doCompare}>
            Compare revisions
          </Button>
        )}
      </header>
      {compare ? (
        <Compare owner={owner} repo={repo} slug={slug} range={compare} onBack={() => setQuery({ compare: null })} />
      ) : history.error ? (
        <EmptyState icon={HistoryIcon} title="Could not load the history" />
      ) : !history.data ? (
        <Skeleton width="100%" height={160} />
      ) : (
        <ul className={styles.commits} aria-label="Revisions">
          {commits.map((c, i) => (
            <li key={c.sha} className={styles.commit}>
              <input
                type="checkbox"
                checked={picked.includes(c.sha)}
                onChange={() => toggle(c.sha)}
                aria-label={`Select ${c.sha.slice(0, 7)} for comparison`}
              />
              <Avatar user={{ login: c.author.login ?? c.author.name, avatarUrl: c.author.avatarUrl ?? '', name: c.author.name }} size={20} />
              <div className={styles.commitMain}>
                <div className={styles.commitMsg}>{c.message}</div>
                <div className={styles.meta}>
                  <strong>{c.author.login ?? c.author.name}</strong> · <RelativeTime date={c.date} />
                  {i === 0 && <span className={styles.latest}>Latest</span>}
                </div>
              </div>
              <Link to={`${base}/${slug}?rev=${c.sha}`} className={styles.sha} title="View the page at this revision">
                {c.sha.slice(0, 7)}
              </Link>
              {i < commits.length - 1 && (
                <Button size="sm" variant="ghost" onClick={() => setQuery({ compare: `${commits[i + 1]!.sha}...${c.sha}` }, { replace: false })}>
                  Diff
                </Button>
              )}
              {index.data?.canEdit && i > 0 && (
                <Button size="sm" leadingIcon={UndoIcon} onClick={() => void revert(c.sha)}>
                  Revert to this
                </Button>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Compare({ owner, repo, slug, range, onBack }: { owner: string; repo: string; slug: string; range: string; onBack: () => void }) {
  const [a, b] = range.split('...');
  const res = useResource(wikiKey(owner, repo, 'compare', slug, range), () => compareWiki(owner, repo, a!, b!, slug), { immutable: true });
  return (
    <div className={styles.compare}>
      <div className={styles.compareHead}>
        <Button size="sm" variant="ghost" leadingIcon={ArrowLeftIcon} onClick={onBack}>
          All revisions
        </Button>
        <span>
          Comparing <code>{a?.slice(0, 7)}</code> … <code>{b?.slice(0, 7)}</code>
        </span>
      </div>
      {res.data ? (
        res.data.diff.trim() ? (
          <div className={styles.diff}>
            <DiffViewer diff={res.data.diff} showTree={false} />
          </div>
        ) : (
          <EmptyState icon={FileDiffIcon} title="No changes between these revisions" />
        )
      ) : res.error ? (
        <EmptyState icon={FileDiffIcon} title="Could not load the comparison" />
      ) : (
        <Skeleton width="100%" height={200} />
      )}
    </div>
  );
}

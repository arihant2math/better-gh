import { useDeferredValue, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { navigate } from '../../router';
import { Dialog } from '../../ui/Dialog';
import { fuzzyScore } from '../../ui/fuzzy';
import { FileIcon, SearchIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { Kbd } from '../../ui/Badge';
import { Spinner } from '../../ui/Spinner';
import styles from './Code.module.css';
import { prefetchBlob, useFileList } from './data';
import { codeUrl, isSettled, type CodeTarget } from './util';

const MAX_RESULTS = 60;

/** Rank paths: fuzzy over the full path, bonus when the file name matches. */
export function rankPaths(paths: readonly string[], query: string, limit = MAX_RESULTS): string[] {
  const q = query.trim().replace(/\s+/g, '');
  if (!q) return paths.slice(0, limit);
  const top: { p: string; s: number }[] = [];
  let floor = 0;
  for (const p of paths) {
    let s = fuzzyScore(q, p);
    if (!s) continue;
    const name = p.slice(p.lastIndexOf('/') + 1);
    const ns = fuzzyScore(q, name);
    if (ns) s += ns * 1.5;
    if (top.length >= limit && s <= floor) continue;
    top.push({ p, s });
    if (top.length > limit * 2) {
      top.sort((a, b) => b.s - a.s || a.p.length - b.p.length);
      top.length = limit;
      floor = top[top.length - 1]!.s;
    }
  }
  top.sort((a, b) => b.s - a.s || a.p.length - b.p.length);
  return top.slice(0, limit).map((x) => x.p);
}

/** `t` file finder over every path of the current commit (lazy chunk). */
export default function FileFinder({ t, open, onClose }: { t: CodeTarget; open: boolean; onClose: () => void }) {
  const { data, error } = useFileList(t, open && isSettled(t));
  const [query, setQuery] = useState('');
  const deferred = useDeferredValue(query);
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const results = useMemo(() => (data ? rankPaths(data.paths, deferred) : []), [data, deferred]);
  const cursor = Math.min(active, Math.max(0, results.length - 1));

  const go = (path: string) => {
    onClose();
    navigate(codeUrl(t, 'blob', t.ref, path));
  };
  const move = (d: number) => {
    const next = (cursor + d + results.length) % Math.max(1, results.length);
    setActive(next);
    const p = results[next];
    if (p) prefetchBlob(t, p);
    listRef.current?.querySelector(`[data-index="${next}"]`)?.scrollIntoView({ block: 'nearest' });
  };
  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) move(1);
    else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) move(-1);
    else if (e.key === 'Enter' && results[cursor]) go(results[cursor]);
    else return;
    e.preventDefault();
  };

  return (
    <Dialog open={open} onClose={onClose} position="top" hideHeader aria-label="Go to file" className={styles.finderDialog}>
      <div className={styles.finder} onKeyDown={onKeyDown}>
        <div className={styles.finderInput}>
          <Input
            autoFocus
            leadingIcon={SearchIcon}
            placeholder={`Go to file in ${t.repo} @ ${t.ref.length === 40 ? t.ref.slice(0, 7) : t.ref}`}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setActive(0);
            }}
            aria-label="File name"
            trailing={!data && !error ? <Spinner size={14} /> : undefined}
          />
        </div>
        <div ref={listRef} className={styles.finderList} role="listbox" aria-label="Files">
          {error ? (
            <div className={styles.finderEmpty}>Could not load the file list.</div>
          ) : data && !results.length ? (
            <div className={styles.finderEmpty}>No matching files</div>
          ) : (
            results.map((p, i) => {
              const slash = p.lastIndexOf('/');
              return (
                <button
                  key={p}
                  type="button"
                  role="option"
                  aria-selected={i === cursor}
                  data-index={i}
                  className={styles.finderItem}
                  data-active={i === cursor}
                  onPointerMove={() => i !== cursor && setActive(i)}
                  onMouseEnter={() => prefetchBlob(t, p)}
                  onClick={() => go(p)}
                >
                  <FileIcon size={14} />
                  <span className={styles.finderName}>{p.slice(slash + 1)}</span>
                  <span className={styles.finderDir}>{slash > 0 ? p.slice(0, slash) : ''}</span>
                </button>
              );
            })
          )}
        </div>
        <div className={styles.finderFoot}>
          <span>
            <Kbd>↑</Kbd> <Kbd>↓</Kbd> to navigate · <Kbd>↵</Kbd> to open · <Kbd>esc</Kbd> to close
          </span>
          {data && <span>{data.paths.length.toLocaleString()} files{data.truncated ? ' (truncated)' : ''}</span>}
        </div>
      </div>
    </Dialog>
  );
}

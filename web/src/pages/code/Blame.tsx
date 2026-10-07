import { useCallback, useMemo } from 'react';
import type { Blame, BlameCommit } from '../../api/code';
import type { BlobView } from '../../api/types';
import { Link } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Skeleton } from '../../ui/EmptyState';
import { VersionsIcon } from '../../ui/icons';
import { formatShort } from '../../ui/RelativeTime';
import { Tooltip } from '../../ui/Tooltip';
import styles from './Code.module.css';
import { CodeLines } from './CodeLines';
import { useBlame } from './data';
import { type CodeTarget, type LineRange } from './util';
import { codeUrl } from '../../components/code/urls';

interface LineInfo {
  commit: BlameCommit | undefined;
  sha: string;
  first: boolean;
  origLine: number;
  /** 0 (oldest) … 9 (newest). */
  age: number;
}

const BUCKETS = 10;

function commitTime(c: BlameCommit | undefined): number {
  if (!c) return 0;
  if (c.author.time) return c.author.time * 1000;
  return c.author.date ? Date.parse(c.author.date) : 0;
}

/** Map blame ranges to per-line info with an age bucket for the heatmap. */
export function blameLines(blame: Blame, lineCount: number): LineInfo[] {
  const times = Object.values(blame.commits).map(commitTime).filter(Boolean);
  const min = Math.min(...times);
  const max = Math.max(...times);
  const out: LineInfo[] = new Array(lineCount);
  for (const r of blame.ranges) {
    const c = blame.commits[r.sha];
    const span = max - min;
    const age = span > 0 ? Math.min(BUCKETS - 1, Math.floor(((commitTime(c) - min) / span) * BUCKETS)) : BUCKETS - 1;
    for (let k = 0; k < r.count; k++) {
      const i = r.line - 1 + k;
      if (i < lineCount) out[i] = { commit: c, sha: r.sha, first: k === 0, origLine: r.orig_line + k, age };
    }
  }
  for (let i = 0; i < lineCount; i++) out[i] ??= { commit: undefined, sha: '', first: false, origLine: i + 1, age: 0 };
  return out;
}

const GUTTER_W = 360;

/** Blame: per-range commit info + age heatmap next to the highlighted lines. */
export function BlameBody({
  t,
  blob,
  selection,
  onSelect,
}: {
  t: CodeTarget;
  blob: BlobView;
  selection: LineRange | null;
  onSelect: (r: LineRange | null, line: number, extend: boolean) => void;
}) {
  const { data, error } = useBlame(t, !!blob.lines);
  const info = useMemo(() => (data && blob.lines ? blameLines(data, blob.lines.length) : null), [data, blob.lines]);
  const { owner, repo } = t;
  const base = `/${owner}/${repo}`;
  // Stable so memoized code rows don't all re-render on each selection change.
  const gutter = useCallback(
    (i: number) => {
      const l = info![i]!;
      const c = l.commit;
      return (
        <div className={styles.blameCell}>
          <span className={styles.age} data-age={l.age} aria-hidden />
          {l.first && c ? (
            <>
              <span className={styles.blameDate}>{c.author.date ? formatShort(c.author.date) : ''}</span>
              <Avatar user={{ login: c.author.login ?? c.author.name, avatarUrl: c.author.avatar_url ?? '', name: c.author.name }} size={16} />
              <Link to={`${base}/commit/${c.sha}`} className={styles.blameMsg} title={`${c.summary}\n${c.author.name} · ${c.sha.slice(0, 7)}`}>
                {c.summary}
              </Link>
              {c.previous ? (
                <Tooltip label="Blame prior to this change">
                  <Link to={codeUrl({ owner, repo }, 'blame', c.previous.sha, c.previous.path) + `#L${l.origLine}`} className={styles.blamePrior} aria-label="Blame prior to this change">
                    <VersionsIcon size={14} />
                  </Link>
                </Tooltip>
              ) : (
                <span className={styles.blamePrior} />
              )}
            </>
          ) : null}
        </div>
      );
    },
    [info, base, owner, repo],
  );
  const rowClass = useCallback((i: number) => (info![i]!.first && i > 0 ? styles.blameStart : undefined), [info]);
  if (!blob.lines) return <div className={styles.notice}>Blame is not available for this file.</div>;
  if (error) return <div className={styles.notice}>Could not compute blame for this file.</div>;
  if (!info) {
    return (
      <div className={styles.fileLoading}>
        {Array.from({ length: 10 }, (_, i) => (
          <Skeleton key={i} width={`${30 + ((i * 29) % 55)}%`} />
        ))}
      </div>
    );
  }
  return (
    <div className={styles.codeScroll}>
      <div className={styles.blameLegend}>
        <span>Older</span>
        {Array.from({ length: BUCKETS }, (_, i) => (
          <span key={i} className={styles.ageSwatch} data-age={i} />
        ))}
        <span>Newer</span>
      </div>
      <CodeLines lines={blob.lines} selection={selection} onSelect={onSelect} gutter={gutter} gutterWidth={GUTTER_W} rowClass={rowClass} />
    </div>
  );
}

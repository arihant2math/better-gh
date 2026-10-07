/**
 * Rendered ("rich") diff of a Markdown file (P37): the old and new
 * versions rendered side by side, or just one of them. Loaded lazily from
 * DiffView. Uses the shared client Markdown renderer (P35 upgrades it).
 */
import { useState } from 'react';
import { useResource } from '../../api/cache';
import { getBlobLines } from '../../api/endpoints';
import { cx } from '../../ui/Button';
import { Markdown } from '../../ui/Markdown';
import { Spinner } from '../../ui/Spinner';
import type { DiffFileEntry } from './DiffView';
import styles from './DiffViewer.module.css';
import type { DiffSource } from './useDiffExtras';

type View = 'split' | 'before' | 'after';

function useText(source: DiffSource, ref: string, path: string, skip: boolean) {
  return useResource(skip ? null : `blob-lines:${source.owner}/${source.repo}@${ref}:${path}:text`, () =>
    getBlobLines(source.owner, source.repo, ref, path).then((r) => (r.lines ?? []).join('\n')),
  { immutable: true });
}

export default function RichDiff({ file, source }: { file: DiffFileEntry; source: DiffSource }) {
  const status = file.status === 'removed' ? 'deleted' : file.status;
  const hasOld = status !== 'added';
  const hasNew = status !== 'deleted';
  const [view, setView] = useState<View>(hasOld && hasNew ? 'split' : hasNew ? 'after' : 'before');
  const before = useText(source, source.oldRef, file.oldPath ?? file.path, !hasOld);
  const after = useText(source, source.newRef, file.path, !hasNew);
  const repo = `${source.owner}/${source.repo}`;
  const pane = (label: string, r: typeof before, cls: string) => (
    <section className={cx(styles.richPane, cls)} aria-label={label}>
      <div className={styles.richLabel}>{label}</div>
      {r.error ? <div className={styles.richNote}>Couldn’t load this version.</div> : r.data === undefined ? <Spinner size={14} /> : <Markdown source={r.data} repo={repo} />}
    </section>
  );
  return (
    <div className={styles.rich} data-testid="rich-diff">
      {hasOld && hasNew && (
        <div className={styles.richBar} role="group" aria-label="Rendered view">
          {(['split', 'before', 'after'] as const).map((v) => (
            <button key={v} type="button" aria-pressed={view === v} onClick={() => setView(v)}>
              {v === 'split' ? 'Before and after' : v === 'before' ? 'Before' : 'After'}
            </button>
          ))}
        </div>
      )}
      <div className={cx(styles.richPanes, view === 'split' && styles.richSplit)}>
        {hasOld && view !== 'after' && pane('Before', before, styles.richBefore!)}
        {hasNew && view !== 'before' && pane('After', after, styles.richAfter!)}
      </div>
    </div>
  );
}

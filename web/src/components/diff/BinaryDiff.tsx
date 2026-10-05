/**
 * Binary file diffs (P37): sizes of both versions, and for images 2-up,
 * swipe and onion-skin views via the raw URLs at both commits. Loaded
 * lazily from DiffView.
 */
import { useState } from 'react';
import { useResource } from '../../api/cache';
import { getBlobLines } from '../../api/endpoints';
import type { BlobLines } from '../../api/types';
import { cx } from '../../ui/Button';
import { Spinner } from '../../ui/Spinner';
import type { DiffFileEntry } from './DiffView';
import styles from './DiffViewer.module.css';
import type { DiffSource } from './useDiffExtras';

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10 * 1024 ? 1 : 0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function useMeta(source: DiffSource, ref: string, path: string, skip: boolean) {
  return useResource<BlobLines>(skip ? null : `blob-lines:${source.owner}/${source.repo}@${ref}:${path}:meta`, () => getBlobLines(source.owner, source.repo, ref, path, { start: 1, end: 0, text: false }), { immutable: true });
}

type Mode = '2-up' | 'swipe' | 'onion';

export default function BinaryDiff({ file, source }: { file: DiffFileEntry; source: DiffSource }) {
  const status = file.status === 'removed' ? 'deleted' : file.status;
  const hasOld = status !== 'added';
  const hasNew = status !== 'deleted';
  const before = useMeta(source, source.oldRef, file.oldPath ?? file.path, !hasOld);
  const after = useMeta(source, source.newRef, file.path, !hasNew);
  const loading = (hasOld && before.data === undefined && !before.error) || (hasNew && after.data === undefined && !after.error);
  if (loading) {
    return (
      <div className={styles.binary}>
        <Spinner size={14} /> Loading…
      </div>
    );
  }
  const old = hasOld ? before.data : undefined;
  const cur = hasNew ? after.data : undefined;
  if ((old?.image || !hasOld) && (cur?.image || !hasNew) && (old || cur)) return <ImageDiff before={old} after={cur} />;
  // No patch and no line changes but text on both sides: an empty file or a mode change.
  if ((old || cur) && !old?.binary && !cur?.binary) {
    return <div className={styles.binary}>{(cur ?? old)!.size === 0 ? 'Empty file.' : 'File mode changed.'}</div>;
  }
  return (
    <div className={styles.binary} data-testid="binary-diff">
      Binary file not shown.
      {(old || cur) && (
        <span className={styles.binarySizes}>
          {old ? formatBytes(old.size) : '—'} → {cur ? formatBytes(cur.size) : '—'}
        </span>
      )}
    </div>
  );
}

interface Dims {
  w: number;
  h: number;
}

function ImageDiff({ before, after }: { before?: BlobLines; after?: BlobLines }) {
  const both = !!before && !!after;
  const [mode, setMode] = useState<Mode>('2-up');
  const [swipe, setSwipe] = useState(50);
  const [onion, setOnion] = useState(50);
  const [dims, setDims] = useState<{ before?: Dims; after?: Dims }>({});
  const onLoad = (side: 'before' | 'after') => (e: React.SyntheticEvent<HTMLImageElement>) => {
    const img = e.currentTarget;
    setDims((d) => ({ ...d, [side]: { w: img.naturalWidth, h: img.naturalHeight } }));
  };
  const caption = (label: string, b: BlobLines, d?: Dims) => (
    <figcaption>
      <span className={styles.imageLabel}>{label}</span> {d ? `${d.w} × ${d.h} px · ` : ''}
      {formatBytes(b.size)}
    </figcaption>
  );
  // Stack frame for swipe/onion: the larger of the two images.
  const frame = {
    width: Math.max(dims.before?.w ?? 0, dims.after?.w ?? 0) || undefined,
    height: Math.max(dims.before?.h ?? 0, dims.after?.h ?? 0) || undefined,
  };
  return (
    <div className={styles.imageDiff} data-testid="image-diff" data-mode={mode}>
      {both && (
        <div className={styles.richBar} role="group" aria-label="Image view">
          {(['2-up', 'swipe', 'onion'] as const).map((m) => (
            <button key={m} type="button" aria-pressed={mode === m} onClick={() => setMode(m)}>
              {m === '2-up' ? '2-up' : m === 'swipe' ? 'Swipe' : 'Onion skin'}
            </button>
          ))}
        </div>
      )}
      {(!both || mode === '2-up') && (
        <div className={styles.imageTwoUp}>
          {before && (
            <figure className={cx(styles.imageFigure, styles.imageBefore)}>
              <img src={before.raw_url} alt={`${before.path} (before)`} onLoad={onLoad('before')} />
              {caption('Before', before, dims.before)}
            </figure>
          )}
          {after && (
            <figure className={cx(styles.imageFigure, styles.imageAfter)}>
              <img src={after.raw_url} alt={`${after.path} (after)`} onLoad={onLoad('after')} />
              {caption('After', after, dims.after)}
            </figure>
          )}
        </div>
      )}
      {both && mode === 'swipe' && (
        <div className={styles.imageStackWrap}>
          <div className={styles.imageStack} style={frame}>
            <img src={before.raw_url} alt={`${before.path} (before)`} onLoad={onLoad('before')} />
            <div className={styles.swipeAfter} style={{ width: `${100 - swipe}%` }} data-testid="swipe-after">
              <img src={after.raw_url} alt={`${after.path} (after)`} onLoad={onLoad('after')} style={{ width: frame.width, height: frame.height }} />
            </div>
            <div className={styles.swipeHandle} style={{ left: `${swipe}%` }} />
          </div>
          <input type="range" min={0} max={100} value={swipe} aria-label="Swipe position" onChange={(e) => setSwipe(Number(e.target.value))} />
        </div>
      )}
      {both && mode === 'onion' && (
        <div className={styles.imageStackWrap}>
          <div className={styles.imageStack} style={frame}>
            <img src={before.raw_url} alt={`${before.path} (before)`} onLoad={onLoad('before')} />
            <img className={styles.onionTop} src={after.raw_url} alt={`${after.path} (after)`} onLoad={onLoad('after')} style={{ opacity: onion / 100 }} />
          </div>
          <input type="range" min={0} max={100} value={onion} aria-label="Onion skin opacity" onChange={(e) => setOnion(Number(e.target.value))} />
        </div>
      )}
    </div>
  );
}

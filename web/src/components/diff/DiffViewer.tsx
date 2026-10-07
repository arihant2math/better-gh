import { useCallback, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import { DiffView, type DiffAnnotations, type DiffFileEntry, type DiffSource } from './DiffView';
import { parseDiff, type DiffFile } from './parseDiff';

export function toEntries(files: DiffFile[]): DiffFileEntry[] {
  return files.map((f) => ({
    path: f.path,
    oldPath: f.oldPath,
    status: f.status,
    additions: f.additions,
    deletions: f.deletions,
    binary: f.binary,
    hunks: f.hunks,
  }));
}

/**
 * Read-only diff of a raw `.diff` text (commits, compare previews): file
 * tree, collapse, local "viewed" state, unified/split. Optional
 * `annotations` (inline threads / line composer, e.g. commit comments) and a
 * `footer` rendered after the last file. For review features use `DiffView`
 * directly.
 */
export function DiffViewer({
  diff,
  showTree = true,
  mode = 'unified',
  keyboard = false,
  annotations,
  footer,
  source,
}: {
  diff: string | DiffFile[];
  showTree?: boolean;
  mode?: 'unified' | 'split';
  keyboard?: boolean;
  annotations?: DiffAnnotations;
  footer?: ReactNode;
  /** Enables highlighting, context expansion, image/rich diffs and file actions. */
  source?: DiffSource;
}) {
  const files = useMemo(() => toEntries(typeof diff === 'string' ? parseDiff(diff) : diff), [diff]);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const [viewed, setViewed] = useState<ReadonlySet<string>>(() => new Set());
  const toggle = (set: ReadonlySet<string>, p: string) => {
    const n = new Set(set);
    if (n.has(p)) n.delete(p);
    else n.add(p);
    return n;
  };
  const isViewed = useCallback((p: string) => viewed.has(p), [viewed]);
  return (
    <DiffView
      files={files}
      mode={mode}
      tree={showTree}
      keyboard={keyboard}
      annotations={annotations}
      footer={footer}
      source={source}
      collapsed={collapsed}
      onToggleCollapsed={(p) => setCollapsed((c) => toggle(c, p))}
      isViewed={isViewed}
      onToggleViewed={(p) => {
        const now = !viewed.has(p);
        setViewed((v) => toggle(v, p));
        setCollapsed((c) => {
          const n = new Set(c);
          if (now) n.add(p);
          else n.delete(p);
          return n;
        });
      }}
    />
  );
}

import { useCallback, useMemo, useState } from 'react';
import { DiffView, type DiffFileEntry } from './DiffView';
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
 * tree, collapse, local "viewed" state, unified/split. For review features
 * use `DiffView` directly.
 */
export function DiffViewer({ diff, showTree = true, mode = 'unified', keyboard = false }: { diff: string | DiffFile[]; showTree?: boolean; mode?: 'unified' | 'split'; keyboard?: boolean }) {
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

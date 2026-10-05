import { useMemo, useState } from 'react';
import { cx } from '../../ui/Button';
import { ChevronDownIcon, ChevronRightIcon, FileDirectoryFillIcon, FileIcon } from '../../ui/icons';
import { VirtualList } from '../../ui/VirtualList';
import styles from './DiffViewer.module.css';
import { parseDiff, type DiffFile, type DiffHunk, type DiffLine } from './parseDiff';

type Row =
  | { kind: 'file'; file: DiffFile; index: number }
  | { kind: 'hunk'; hunk: DiffHunk; file: number }
  | { kind: 'line'; line: DiffLine; file: number; key: string }
  | { kind: 'binary'; file: number }
  | { kind: 'gap'; file: number };

interface TreeNode {
  name: string;
  path: string;
  children: Map<string, TreeNode>;
  file?: { index: number; file: DiffFile };
}

function buildTree(files: DiffFile[]): TreeNode {
  const root: TreeNode = { name: '', path: '', children: new Map() };
  files.forEach((file, index) => {
    const parts = file.path.split('/');
    let node = root;
    parts.forEach((part, i) => {
      let child = node.children.get(part);
      if (!child) {
        child = { name: part, path: parts.slice(0, i + 1).join('/'), children: new Map() };
        node.children.set(part, child);
      }
      node = child;
    });
    node.file = { index, file };
  });
  // Compact single-child directories ("src/lib" instead of "src" > "lib").
  const compact = (n: TreeNode): TreeNode => {
    for (const [k, c] of n.children) n.children.set(k, compact(c));
    if (!n.file && n.children.size === 1 && n.name) {
      const only = [...n.children.values()][0]!;
      if (!only.file) return { ...only, name: `${n.name}/${only.name}` };
    }
    return n;
  };
  return compact(root);
}

/**
 * Virtualized unified-diff viewer with collapsible files, "viewed" state and
 * a file-tree sidebar. Takes the raw `.diff` text (or pre-parsed files).
 */
export function DiffViewer({ diff, showTree = true }: { diff: string | DiffFile[]; showTree?: boolean }) {
  const files = useMemo(() => (typeof diff === 'string' ? parseDiff(diff) : diff), [diff]);
  const [collapsed, setCollapsed] = useState<Set<number>>(() => new Set());
  const [viewed, setViewed] = useState<Set<number>>(() => new Set());
  const [jump, setJump] = useState<{ index: number; nonce: number } | null>(null);

  const rows = useMemo(() => {
    const out: Row[] = [];
    files.forEach((file, fi) => {
      out.push({ kind: 'file', file, index: fi });
      if (collapsed.has(fi)) return;
      if (file.binary) out.push({ kind: 'binary', file: fi });
      file.hunks.forEach((hunk, hi) => {
        out.push({ kind: 'hunk', hunk, file: fi });
        hunk.lines.forEach((line, li) => out.push({ kind: 'line', line, file: fi, key: `${fi}:${hi}:${li}` }));
      });
      out.push({ kind: 'gap', file: fi });
    });
    return out;
  }, [files, collapsed]);

  const fileRowIndex = useMemo(() => {
    const m = new Map<number, number>();
    rows.forEach((r, i) => r.kind === 'file' && m.set(r.index, i));
    return m;
  }, [rows]);

  const toggle = (set: Set<number>, i: number) => {
    const next = new Set(set);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    return next;
  };

  const totals = files.reduce((acc, f) => ({ add: acc.add + f.additions, del: acc.del + f.deletions }), { add: 0, del: 0 });
  const tree = useMemo(() => buildTree(files), [files]);

  return (
    <div className={cx(styles.viewer, !showTree && styles.noTree)}>
      {showTree && (
        <nav className={styles.tree} aria-label="Changed files">
          <div className={styles.treeHeader}>
            {files.length} file{files.length === 1 ? '' : 's'} · <span className={styles.add}>+{totals.add}</span> <span className={styles.del}>−{totals.del}</span>
            <span className={styles.viewedCount}>
              {viewed.size}/{files.length} viewed
            </span>
          </div>
          <TreeView node={tree} depth={0} viewed={viewed} onPick={(i) => setJump({ index: fileRowIndex.get(i) ?? 0, nonce: Date.now() })} />
        </nav>
      )}
      <VirtualList
        className={styles.diff}
        items={rows}
        estimateSize={20}
        overscan={30}
        activeIndex={jump?.index}
        activeAlign="start"
        scrollNonce={jump?.nonce}
        getKey={(r, i) => (r.kind === 'line' ? r.key : `${r.kind}:${r.kind === 'file' ? r.index : r.file}:${i}`)}
        renderItem={(r) => {
          switch (r.kind) {
            case 'file': {
              const f = r.file;
              const isCollapsed = collapsed.has(r.index);
              return (
                <div className={cx(styles.fileHeader, isCollapsed && styles.fileHeaderCollapsed)}>
                  <button
                    type="button"
                    className={styles.collapse}
                    aria-expanded={!isCollapsed}
                    aria-label={isCollapsed ? 'Expand file' : 'Collapse file'}
                    onClick={() => setCollapsed((c) => toggle(c, r.index))}
                  >
                    {isCollapsed ? <ChevronRightIcon size={16} /> : <ChevronDownIcon size={16} />}
                  </button>
                  <span className={styles.stat}>
                    <span className={styles.add}>+{f.additions}</span> <span className={styles.del}>−{f.deletions}</span>
                  </span>
                  <span className={styles.path} title={f.path}>
                    {f.status === 'renamed' ? `${f.oldPath} → ${f.newPath}` : f.path}
                  </span>
                  {f.status !== 'modified' && <span className={cx(styles.status, styles[f.status])}>{f.status}</span>}
                  <label className={styles.viewed}>
                    <input
                      type="checkbox"
                      checked={viewed.has(r.index)}
                      onChange={() => {
                        const nowViewed = !viewed.has(r.index);
                        setViewed((v) => toggle(v, r.index));
                        setCollapsed((c) => {
                          const n = new Set(c);
                          if (nowViewed) n.add(r.index);
                          else n.delete(r.index);
                          return n;
                        });
                      }}
                    />
                    Viewed
                  </label>
                </div>
              );
            }
            case 'hunk':
              return (
                <div className={styles.hunk}>
                  <span className={styles.num} />
                  <span className={styles.num} />
                  <span className={styles.code}>{r.hunk.header}</span>
                </div>
              );
            case 'line': {
              const l = r.line;
              return (
                <div className={cx(styles.line, styles[l.type])}>
                  <span className={styles.num}>{l.oldNo ?? ''}</span>
                  <span className={styles.num}>{l.newNo ?? ''}</span>
                  <span className={styles.code}>
                    <span className={styles.marker}>{l.type === 'add' ? '+' : l.type === 'del' ? '-' : ' '}</span>
                    {l.text}
                  </span>
                </div>
              );
            }
            case 'binary':
              return <div className={styles.binary}>Binary file not shown.</div>;
            case 'gap':
              return <div className={styles.gap} />;
          }
        }}
      />
    </div>
  );
}

function TreeView({ node, depth, viewed, onPick }: { node: TreeNode; depth: number; viewed: Set<number>; onPick: (i: number) => void }) {
  const [open, setOpen] = useState(true);
  const children = [...node.children.values()].sort((a, b) => (!!a.file === !!b.file ? a.name.localeCompare(b.name) : a.file ? 1 : -1));
  if (node.file) {
    const f = node.file.file;
    return (
      <button type="button" className={cx(styles.treeItem, viewed.has(node.file.index) && styles.treeViewed)} style={{ paddingLeft: 8 + depth * 12 }} onClick={() => onPick(node.file!.index)} title={f.path}>
        <FileIcon size={14} />
        <span className={styles.treeName}>{node.name}</span>
        <span className={cx(styles.dotStatus, styles[f.status])} />
      </button>
    );
  }
  return (
    <div>
      {node.name && (
        <button type="button" className={styles.treeItem} style={{ paddingLeft: 8 + depth * 12 }} onClick={() => setOpen((o) => !o)} aria-expanded={open}>
          {open ? <ChevronDownIcon size={12} /> : <ChevronRightIcon size={12} />}
          <FileDirectoryFillIcon size={14} className={styles.dirIcon} />
          <span className={styles.treeName}>{node.name}</span>
        </button>
      )}
      {open && children.map((c) => <TreeView key={c.path} node={c} depth={node.name ? depth + 1 : depth} viewed={viewed} onPick={onPick} />)}
    </div>
  );
}

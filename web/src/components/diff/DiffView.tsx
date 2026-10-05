import { lazy, memo, Suspense, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button, cx } from '../../ui/Button';
import type { CommitAnnotation } from '../../api/types';
import { Link } from '../../router';
import { toast } from '../../ui/Toast';
import {
  AlertIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CommentIcon,
  CopyIcon,
  EyeIcon,
  FileCodeIcon,
  FileDirectoryFillIcon,
  FileIcon,
  FoldDownIcon,
  FoldUpIcon,
  InfoIcon,
  PencilIcon,
  PlusIcon,
  UnfoldIcon,
  XCircleIcon,
} from '../../ui/icons';
import { Spinner } from '../../ui/Spinner';
import { VirtualList } from '../../ui/VirtualList';
import styles from './DiffViewer.module.css';
import type { ExpandDir, GapControls } from './expand';
import { isGenerated, isMarkdown, lineHtml, type FileHighlight } from './highlight';
import { splitHunk, type DiffHunk, type DiffLine } from './parseDiff';
import { useDiffExtras, type DiffExtras, type DiffSource } from './useDiffExtras';

export type { DiffSource } from './useDiffExtras';

// Rendered (Markdown) and image diffs load on demand, never with the diff itself.
const RichDiff = lazy(() => import('./RichDiff'));
const BinaryDiff = lazy(() => import('./BinaryDiff'));

export type Side = 'LEFT' | 'RIGHT';

/** One file of a diff. `hunks === undefined` = not loaded yet; `null` = not available. */
export interface DiffFileEntry {
  path: string;
  oldPath?: string;
  status: 'added' | 'deleted' | 'modified' | 'renamed' | 'removed' | 'copied' | 'changed';
  additions: number;
  deletions: number;
  binary?: boolean;
  hunks: DiffHunk[] | null | undefined;
  /** Why `hunks` is null (shown instead of the diff). */
  unavailable?: string;
}

/** A line range picked for commenting (single side). */
export interface LineSelection {
  path: string;
  side: Side;
  start: number;
  end: number;
}

export interface DiffAnnotations {
  /**
   * Anchors that have extra content for a file: `L12` / `R40` (rendered
   * below that line), `file` (below the file header). Unknown line anchors
   * are rendered at the end of the file.
   */
  anchors(path: string): readonly string[];
  render(path: string, anchor: string): ReactNode;
  /** Gutter "+" / drag selection / `c` on the cursor line. */
  onSelect?(sel: LineSelection): void;
  selection?: LineSelection | null;
  /** Count of comments per file for the header badge. */
  commentCount?(path: string): number;
  /** Sides that accept new comments (default both; commit comments are RIGHT only). */
  sides?: readonly Side[];
}

export interface DiffViewProps {
  files: readonly DiffFileEntry[];
  mode?: 'unified' | 'split';
  /** Paths with collapsed bodies. */
  collapsed: ReadonlySet<string>;
  onToggleCollapsed(path: string): void;
  isViewed?(path: string): boolean;
  onToggleViewed?(path: string): void;
  /** Called (once per mount of its placeholder) when a file with `hunks === undefined` scrolls into view. */
  onNeedFile?(path: string): void;
  annotations?: DiffAnnotations;
  /** Rendered above the rows (inside the scroller). */
  header?: ReactNode;
  /** Rendered after the last file (inside the scroller), e.g. a commit's comment thread. */
  footer?: ReactNode;
  /** Extra controls in each file header (before "Viewed"). */
  fileActions?(file: DiffFileEntry): ReactNode;
  /** Number of files still being listed (rendered as a trailing placeholder). */
  pendingFiles?: number;
  onNeedMoreFiles?(): void;
  tree?: boolean;
  /** Enable j/k/n/p/c and friends. */
  keyboard?: boolean;
  /** Scroll to this file (changes of `nonce` re-scroll). */
  jumpTo?: { path: string; nonce: number } | null;
  emptyText?: string;
  /**
   * Where the two sides live: enables syntax highlighting, context
   * expansion, image and rendered diffs, file actions (view / edit / copy
   * path) and, with `source.annotations`, inline check-run annotations.
   */
  source?: DiffSource;
}

type Row =
  | { k: 'file'; f: number }
  | { k: 'extra'; f: number; anchor: string }
  | { k: 'loading'; f: number }
  | { k: 'unavailable'; f: number }
  | { k: 'hunk'; f: number; h: number; hunk: DiffHunk; gap: number | null; ctl: GapControls | null }
  | { k: 'tail'; f: number; gap: number; ctl: GapControls }
  | { k: 'annot'; f: number; line: number }
  | { k: 'rich'; f: number }
  | { k: 'line'; f: number; h: number; line: DiffLine }
  | { k: 'pair'; f: number; h: number; left: DiffLine | null; right: DiffLine | null }
  | { k: 'end'; f: number }
  | { k: 'more' }
  | { k: 'footer' };

/** Comment anchors of a unified line (context lines answer to both sides). */
function lineAnchors(l: DiffLine): string[] {
  if (l.type === 'add') return [`R${l.newNo}`];
  if (l.type === 'del') return [`L${l.oldNo}`];
  if (l.type === 'ctx') return [`R${l.newNo}`, `L${l.oldNo}`];
  return [];
}

/** The side/number a click on a unified line comments on. */
function lineTarget(l: DiffLine): { side: Side; no: number } | null {
  if (l.type === 'del') return { side: 'LEFT', no: l.oldNo! };
  if (l.type === 'add' || l.type === 'ctx') return { side: 'RIGHT', no: l.newNo! };
  return null;
}

function numOn(l: DiffLine | null | undefined, side: Side): number | undefined {
  if (!l || l.type === 'meta') return undefined;
  if (side === 'LEFT') return l.type === 'add' ? undefined : l.oldNo;
  return l.type === 'del' ? undefined : l.newNo;
}

/** What `buildRows` needs from the diff extras (P37). */
export type RowExtras = Pick<DiffExtras, 'shown' | 'rich' | 'annotations'>;

export function buildRows(files: readonly DiffFileEntry[], mode: 'unified' | 'split', collapsed: ReadonlySet<string>, annotations?: DiffAnnotations, pendingFiles = 0, extras?: RowExtras): Row[] {
  const out: Row[] = [];
  files.forEach((file, f) => {
    out.push({ k: 'file', f });
    if (collapsed.has(file.path)) return;
    const anchors = new Set(annotations?.anchors(file.path) ?? []);
    if (anchors.delete('file')) out.push({ k: 'extra', f, anchor: 'file' });
    const checks = new Set(extras?.annotations(file.path)?.keys() ?? []);
    if (extras?.rich.has(file.path) && file.hunks !== undefined) out.push({ k: 'rich', f });
    else if (file.hunks === undefined) out.push({ k: 'loading', f });
    else if (file.hunks === null || file.binary) out.push({ k: 'unavailable', f });
    else {
      const flush = (keys: string[], rn: number | undefined) => {
        for (const a of keys) {
          if (anchors.delete(a)) out.push({ k: 'extra', f, anchor: a });
        }
        if (rn != null && checks.delete(rn)) out.push({ k: 'annot', f, line: rn });
      };
      const shown = extras?.shown(file);
      const hunks = shown?.hunks ?? file.hunks;
      hunks.forEach((hunk, h) => {
        const gap = shown ? shown.hunks[h]!.gapAbove : null;
        const ctl = gap != null ? (shown?.controls.get(gap) ?? null) : null;
        out.push({ k: 'hunk', f, h, hunk, gap, ctl: ctl && ctl.hidden > 0 ? ctl : null });
        if (mode === 'unified') {
          for (const line of hunk.lines) {
            out.push({ k: 'line', f, h, line });
            if (anchors.size || checks.size) flush(lineAnchors(line), numOn(line, 'RIGHT'));
          }
        } else {
          for (const { left, right } of splitHunk(hunk)) {
            out.push({ k: 'pair', f, h, left: left?.line ?? null, right: right?.line ?? null });
            if (anchors.size || checks.size) {
              const keys: string[] = [];
              const ln = numOn(left?.line, 'LEFT');
              const rn = numOn(right?.line, 'RIGHT');
              if (ln != null) keys.push(`L${ln}`);
              if (rn != null) keys.push(`R${rn}`);
              flush(keys, rn);
            }
          }
        }
      });
      const tailCtl = shown?.tail ? shown.controls.get(shown.tail.index) : undefined;
      if (shown?.tail && tailCtl && tailCtl.hidden > 0) out.push({ k: 'tail', f, gap: shown.tail.index, ctl: tailCtl });
    }
    // Anchors not on a visible line (e.g. outside the hunks) go last.
    for (const a of anchors) out.push({ k: 'extra', f, anchor: a });
    for (const n of [...checks].sort((a, b) => a - b)) out.push({ k: 'annot', f, line: n });
    out.push({ k: 'end', f });
  });
  if (pendingFiles > 0) out.push({ k: 'more' });
  return out;
}

function rowKey(r: Row, files: readonly DiffFileEntry[], i: number): string {
  if (r.k === 'more' || r.k === 'footer') return r.k;
  const p = files[r.f]!.path;
  switch (r.k) {
    case 'file':
    case 'loading':
    case 'unavailable':
    case 'end':
      return `${r.k}:${p}`;
    case 'extra':
      return `x:${p}:${r.anchor}`;
    case 'hunk':
      return `h:${p}:${r.hunk.header}:${i}`;
    case 'tail':
      return `t:${p}`;
    case 'annot':
      return `a:${p}:${r.line}`;
    case 'rich':
      return `r:${p}`;
    case 'line':
      return `l:${p}:${r.line.oldNo ?? ''}:${r.line.newNo ?? ''}:${r.line.type}:${i}`;
    case 'pair':
      return `p:${p}:${r.left?.oldNo ?? ''}:${r.right?.newNo ?? ''}:${i}`;
  }
}

function inSel(sel: LineSelection | null | undefined, path: string, side: Side, no: number | undefined): boolean {
  return !!sel && no != null && sel.path === path && sel.side === side && no >= sel.start && no <= sel.end;
}

interface Drag {
  path: string;
  /** Ranges can't span hunks (GitHub rejects them). */
  hunk: number;
  side: Side;
  from: number;
  to: number;
}

/**
 * Virtualized diff viewer: unified or split, lazy per-file bodies, a file
 * tree, "viewed" toggles, line selection (click/drag/shift-click the gutter)
 * and anchored extra rows (review threads, comment composer).
 */
export function DiffView(props: DiffViewProps) {
  const { files, mode = 'unified', collapsed, annotations, tree = true, keyboard = false } = props;
  const hasFooter = props.footer != null;
  const extras = useDiffExtras(props.source);
  const rows = useMemo(() => {
    const out = buildRows(files, mode, collapsed, annotations, props.pendingFiles, extras);
    if (hasFooter) out.push({ k: 'footer' });
    return out;
  }, [files, mode, collapsed, annotations, props.pendingFiles, hasFooter, extras]);
  const [cursor, setCursor] = useState<number>(-1);
  const [jump, setJump] = useState<{ index: number; nonce: number } | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const dragRef = useRef<Drag | null>(null);
  dragRef.current = drag;

  const fileRowIndex = useMemo(() => {
    const m = new Map<string, number>();
    rows.forEach((r, i) => r.k === 'file' && m.set(files[r.f]!.path, i));
    return m;
  }, [rows, files]);

  const jumpToPath = useCallback(
    (path: string) => {
      const i = fileRowIndex.get(path);
      if (i != null) setJump({ index: i, nonce: Date.now() });
    },
    [fileRowIndex],
  );
  useEffect(() => {
    if (props.jumpTo) jumpToPath(props.jumpTo.path);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only on explicit jumps
  }, [props.jumpTo?.nonce]);

  // Finish a drag anywhere.
  useEffect(() => {
    if (!drag) return;
    const up = () => {
      const d = dragRef.current;
      setDrag(null);
      if (d) annotations?.onSelect?.({ path: d.path, side: d.side, start: Math.min(d.from, d.to), end: Math.max(d.from, d.to) });
    };
    window.addEventListener('mouseup', up);
    return () => window.removeEventListener('mouseup', up);
  }, [drag, annotations]);

  const isLineRow = (r: Row | undefined) => r?.k === 'line' || r?.k === 'pair';
  const moveCursor = (dir: 1 | -1) => {
    let i = cursor < 0 ? (dir > 0 ? -1 : rows.length) : cursor;
    do i += dir;
    while (i >= 0 && i < rows.length && !isLineRow(rows[i]));
    if (i >= 0 && i < rows.length) {
      setCursor(i);
      setJump({ index: i, nonce: Date.now() });
    }
  };
  const currentFile = (): number => {
    const r = rows[Math.max(0, cursor)];
    return r && r.k !== 'more' && r.k !== 'footer' ? r.f : -1;
  };
  const moveFile = (dir: 1 | -1) => {
    const cur = currentFile();
    const target = Math.max(0, Math.min(files.length - 1, cur + dir));
    const i = fileRowIndex.get(files[target]?.path ?? '');
    if (i == null) return;
    setCursor(i);
    setJump({ index: i, nonce: Date.now() });
  };
  const canSide = (side: Side) => !annotations?.sides || annotations.sides.includes(side);
  const cursorTarget = (): LineSelection | null => {
    const r = rows[cursor];
    if (!r || r.k === 'more' || r.k === 'footer') return null;
    const path = files[r.f]!.path;
    if (r.k === 'line') {
      const t = lineTarget(r.line);
      return t && canSide(t.side) ? { path, side: t.side, start: t.no, end: t.no } : null;
    }
    if (r.k === 'pair') {
      const rn = numOn(r.right, 'RIGHT');
      if (rn != null && canSide('RIGHT')) return { path, side: 'RIGHT', start: rn, end: rn };
      const ln = numOn(r.left, 'LEFT');
      if (ln != null && canSide('LEFT')) return { path, side: 'LEFT', start: ln, end: ln };
    }
    return null;
  };

  useShortcuts(
    'Diff',
    {
      j: { handler: () => moveCursor(1), description: 'Next line', group: 'Files changed' },
      k: { handler: () => moveCursor(-1), description: 'Previous line', group: 'Files changed' },
      n: { handler: () => moveFile(1), description: 'Next file', group: 'Files changed' },
      p: { handler: () => moveFile(-1), description: 'Previous file', group: 'Files changed' },
      c: {
        handler: () => {
          const t = cursorTarget();
          if (!t || !annotations?.onSelect) return false;
          annotations.onSelect(t);
        },
        description: 'Comment on the current line',
        group: 'Files changed',
      },
      'shift+j': {
        handler: () => {
          // Extend the selection downwards from the cursor.
          const t = cursorTarget();
          moveCursor(1);
          if (!t || !annotations?.selection) return false;
        },
        description: 'Next line',
        group: 'Files changed',
      },
      x: {
        handler: () => {
          const f = files[currentFile()];
          if (!f) return false;
          props.onToggleCollapsed(f.path);
        },
        description: 'Collapse / expand file',
        group: 'Files changed',
      },
      v: {
        handler: () => {
          const f = files[currentFile()];
          if (!f || !props.onToggleViewed) return false;
          props.onToggleViewed(f.path);
        },
        description: 'Mark file as viewed',
        group: 'Files changed',
      },
    },
    keyboard,
  );

  const startDrag = (path: string, hunk: number, side: Side, no: number, e: React.MouseEvent) => {
    if (!annotations?.onSelect) return;
    e.preventDefault();
    const sel = annotations.selection;
    if (e.shiftKey && sel && sel.path === path && sel.side === side) {
      annotations.onSelect({ path, side, start: Math.min(sel.start, no), end: Math.max(sel.end, no) });
      return;
    }
    setDrag({ path, hunk, side, from: no, to: no });
  };
  const enterDrag = (path: string, hunk: number, side: Side, no: number | undefined) => {
    const d = dragRef.current;
    if (d && no != null && d.path === path && d.hunk === hunk && d.side === side && d.to !== no) setDrag({ ...d, to: no });
  };
  const liveSel: LineSelection | null | undefined = drag
    ? { path: drag.path, side: drag.side, start: Math.min(drag.from, drag.to), end: Math.max(drag.from, drag.to) }
    : annotations?.selection;

  const totals = useMemo(() => files.reduce((acc, f) => ({ add: acc.add + f.additions, del: acc.del + f.deletions }), { add: 0, del: 0 }), [files]);
  const viewedCount = props.isViewed ? files.filter((f) => props.isViewed!(f.path)).length : 0;
  const treeRoot = useMemo(() => (tree ? buildTree(files) : null), [files, tree]);
  const activePath = files[currentFile()]?.path;
  const commentable = !!annotations?.onSelect;
  const hl = (path: string): FileHighlight | undefined => extras?.highlight(path);

  return (
    <div className={cx(styles.viewer, !tree && styles.noTree)}>
      {treeRoot && (
        <nav className={styles.tree} aria-label="Changed files">
          <div className={styles.treeHeader}>
            {files.length} file{files.length === 1 ? '' : 's'} · <span className={styles.add}>+{totals.add}</span> <span className={styles.del}>−{totals.del}</span>
            {props.isViewed && (
              <span className={styles.viewedCount}>
                {viewedCount}/{files.length} viewed
              </span>
            )}
          </div>
          <TreeView node={treeRoot} depth={0} isViewed={props.isViewed} active={activePath} counts={annotations?.commentCount} onPick={jumpToPath} />
        </nav>
      )}
      <VirtualList
        className={cx(styles.diff, mode === 'split' && styles.split)}
        items={rows}
        estimateSize={20}
        overscan={40}
        role="table"
        aria-label="Diff"
        header={props.header}
        activeIndex={jump?.index}
        activeAlign={jump && rows[jump.index]?.k === 'file' ? 'start' : 'auto'}
        scrollNonce={jump?.nonce}
        getKey={(r, i) => rowKey(r, files, i)}
        renderItem={(r, i) => {
          if (r.k === 'footer') return props.footer;
          if (r.k === 'more') return <MoreFiles count={props.pendingFiles ?? 0} onNeed={props.onNeedMoreFiles} />;
          const file = files[r.f]!;
          const active = i === cursor;
          switch (r.k) {
            case 'file':
              return (
                <FileHeader
                  file={file}
                  extras={extras}
                  collapsed={collapsed.has(file.path)}
                  viewed={props.isViewed?.(file.path)}
                  comments={annotations?.commentCount?.(file.path) ?? 0}
                  onToggle={() => props.onToggleCollapsed(file.path)}
                  onViewed={props.onToggleViewed ? () => props.onToggleViewed!(file.path) : undefined}
                  actions={props.fileActions?.(file)}
                  active={active}
                />
              );
            case 'extra':
              return <div className={styles.extra}>{annotations?.render(file.path, r.anchor)}</div>;
            case 'loading':
              return <LoadingFile path={file.path} onNeed={props.onNeedFile} />;
            case 'unavailable':
              if (file.binary && extras && file.status !== 'renamed')
                return (
                  <Suspense fallback={<div className={styles.binary}>Binary file not shown.</div>}>
                    <BinaryDiff file={file} source={extras.source} />
                  </Suspense>
                );
              return (
                <div className={styles.binary}>
                  {file.binary ? 'Binary file not shown.' : (file.unavailable ?? 'This diff is not available.')}
                  {!file.binary && props.onNeedFile && file.unavailable && /large/i.test(file.unavailable) && (
                    <Button size="sm" variant="ghost" onClick={() => props.onNeedFile!(file.path)} style={{ marginLeft: 8 }}>
                      Load diff
                    </Button>
                  )}
                </div>
              );
            case 'hunk':
              return (
                <div className={cx(styles.hunk, mode === 'split' && styles.hunkSplit, r.ctl && styles.hunkExpandable)}>
                  {r.ctl && extras ? (
                    <Expander ctl={r.ctl} busy={extras.busy.has(`${file.path}:${r.gap}`)} onExpand={(dir) => extras.expand(file, r.gap!, dir)} />
                  ) : (
                    <span className={styles.num} />
                  )}
                  {mode === 'unified' && !r.ctl && <span className={styles.num} />}
                  <span className={styles.code}>{r.hunk.header}</span>
                </div>
              );
            case 'tail':
              return (
                <div className={cx(styles.hunk, mode === 'split' && styles.hunkSplit, styles.hunkExpandable)}>
                  {extras && <Expander ctl={r.ctl} busy={extras.busy.has(`${file.path}:${r.gap}`)} onExpand={(dir) => extras.expand(file, r.gap, dir)} />}
                  <span className={styles.code} />
                </div>
              );
            case 'annot':
              return <CheckAnnotations items={extras?.annotations(file.path)?.get(r.line) ?? []} />;
            case 'rich':
              return extras ? (
                <Suspense
                  fallback={
                    <div className={styles.binary}>
                      <Spinner size={14} /> Loading rendered diff…
                    </div>
                  }
                >
                  <RichDiff file={file} source={extras.source} />
                </Suspense>
              ) : null;
            case 'line': {
              const l = r.line;
              const t = lineTarget(l);
              const selected = t ? inSel(liveSel, file.path, t.side, t.no) || (l.type === 'ctx' && inSel(liveSel, file.path, 'LEFT', l.oldNo)) : false;
              return (
                <UnifiedLine
                  line={l}
                  html={lineHtml(l, hl(file.path))}
                  selected={selected}
                  active={active}
                  commentable={commentable && !!t && canSide(t.side)}
                  onDown={t ? (e) => startDrag(file.path, r.h, t.side, t.no, e) : undefined}
                  onEnter={() => {
                    const d = dragRef.current;
                    if (d) enterDrag(file.path, r.h, d.side, numOn(l, d.side));
                  }}
                  onClick={() => setCursor(i)}
                />
              );
            }
            case 'pair': {
              const ln = numOn(r.left, 'LEFT');
              const rn = numOn(r.right, 'RIGHT');
              return (
                <SplitLine
                  left={r.left}
                  right={r.right}
                  leftHtml={lineHtml(r.left, hl(file.path))}
                  rightHtml={lineHtml(r.right, hl(file.path))}
                  leftSelected={inSel(liveSel, file.path, 'LEFT', ln)}
                  rightSelected={inSel(liveSel, file.path, 'RIGHT', rn)}
                  active={active}
                  commentable={commentable}
                  sides={annotations?.sides}
                  onDown={(side, e) => {
                    const no = side === 'LEFT' ? ln : rn;
                    if (no != null) startDrag(file.path, r.h, side, no, e);
                  }}
                  onEnter={(side) => enterDrag(file.path, r.h, side, side === 'LEFT' ? ln : rn)}
                  onClick={() => setCursor(i)}
                />
              );
            }
            case 'end':
              return <div className={styles.gap} />;
          }
        }}
      />
      {files.length === 0 && !props.pendingFiles && <div className={styles.emptyState}>{props.emptyText ?? 'No changes.'}</div>}
    </div>
  );
}

const FileHeader = memo(function FileHeader({
  file,
  extras,
  collapsed,
  viewed,
  comments,
  onToggle,
  onViewed,
  actions,
  active,
}: {
  file: DiffFileEntry;
  extras?: DiffExtras;
  collapsed: boolean;
  viewed?: boolean;
  comments: number;
  onToggle: () => void;
  onViewed?: () => void;
  actions?: ReactNode;
  active: boolean;
}) {
  const status = file.status === 'removed' ? 'deleted' : file.status;
  // Highlighting is fetched per file once its header is rendered (i.e. scrolled near).
  const ensure = extras?.ensureHighlight;
  const ready = Array.isArray(file.hunks);
  useEffect(() => {
    if (ensure && ready && !collapsed) ensure(file);
  }, [ensure, ready, collapsed, file]);
  const generated = isGenerated(file.path);
  return (
    <div className={cx(styles.fileHeader, collapsed && styles.fileHeaderCollapsed, active && styles.fileActive)} data-path={file.path}>
      <button type="button" className={styles.collapse} aria-expanded={!collapsed} aria-label={collapsed ? 'Expand file' : 'Collapse file'} onClick={onToggle}>
        {collapsed ? <ChevronRightIcon size={16} /> : <ChevronDownIcon size={16} />}
      </button>
      <span className={styles.stat}>
        <span className={styles.add}>+{file.additions}</span> <span className={styles.del}>−{file.deletions}</span>
      </span>
      <span className={styles.path} title={file.path}>
        {status === 'renamed' && file.oldPath ? `${file.oldPath} → ${file.path}` : file.path}
      </span>
      {status !== 'modified' && <span className={cx(styles.status, styles[status])}>{status}</span>}
      {generated && (
        <span className={styles.generated} title="Generated files are collapsed by default">
          Generated
        </span>
      )}
      {comments > 0 && (
        <span className={styles.commentCount} title={`${comments} comment${comments === 1 ? '' : 's'}`}>
          <CommentIcon size={14} /> {comments}
        </span>
      )}
      {extras && <FileActions file={file} extras={extras} />}
      {actions}
      {onViewed && (
        <label className={cx(styles.viewed, viewed && styles.viewedOn)}>
          <input type="checkbox" checked={!!viewed} onChange={onViewed} />
          Viewed
        </label>
      )}
    </div>
  );
});

function Marker({ type }: { type: DiffLine['type'] }) {
  return <span className={styles.marker}>{type === 'add' ? '+' : type === 'del' ? '-' : ' '}</span>;
}

const UnifiedLine = memo(function UnifiedLine({
  line: l,
  html,
  selected,
  active,
  commentable,
  onDown,
  onEnter,
  onClick,
}: {
  line: DiffLine;
  html?: string;
  selected: boolean;
  active: boolean;
  commentable: boolean;
  onDown?: (e: React.MouseEvent) => void;
  onEnter: () => void;
  onClick: () => void;
}) {
  return (
    <div className={cx(styles.line, styles[l.type], selected && styles.selected, active && styles.cursor)} onMouseEnter={onEnter} onClick={onClick} role="row">
      <span className={cx(styles.num, commentable && styles.numClickable)} onMouseDown={commentable ? onDown : undefined}>
        {l.oldNo ?? ''}
      </span>
      <span className={cx(styles.num, commentable && styles.numClickable)} onMouseDown={commentable ? onDown : undefined}>
        {l.newNo ?? ''}
      </span>
      <span className={styles.code}>
        {commentable && (
          <button type="button" className={styles.addComment} aria-label="Add a comment on this line" tabIndex={-1} onMouseDown={onDown}>
            <PlusIcon size={12} />
          </button>
        )}
        {l.type === 'meta' ? l.text : <Marker type={l.type} />}
        {l.type !== 'meta' && <Code text={l.text} html={html} />}
      </span>
    </div>
  );
});

const SplitLine = memo(function SplitLine({
  left,
  right,
  leftHtml,
  rightHtml,
  leftSelected,
  rightSelected,
  active,
  commentable,
  sides,
  onDown,
  onEnter,
  onClick,
}: {
  left: DiffLine | null;
  right: DiffLine | null;
  leftHtml?: string;
  rightHtml?: string;
  leftSelected: boolean;
  rightSelected: boolean;
  active: boolean;
  commentable: boolean;
  sides?: readonly Side[];
  onDown: (side: Side, e: React.MouseEvent) => void;
  onEnter: (side: Side) => void;
  onClick: () => void;
}) {
  const cell = (l: DiffLine | null, side: Side, selected: boolean, html: string | undefined) => {
    const no = l ? (side === 'LEFT' ? l.oldNo : l.newNo) : undefined;
    const type = l ? (l.type === 'ctx' ? 'ctx' : l.type) : 'empty';
    const can = commentable && no != null && (!sides || sides.includes(side));
    return (
      <>
        <span className={cx(styles.num, styles[type], selected && styles.selected, can && styles.numClickable)} onMouseDown={can ? (e) => onDown(side, e) : undefined} onMouseEnter={() => onEnter(side)}>
          {no ?? ''}
        </span>
        <span className={cx(styles.code, styles.cell, styles[type], selected && styles.selected)} onMouseEnter={() => onEnter(side)}>
          {can && (
            <button type="button" className={styles.addComment} aria-label="Add a comment on this line" tabIndex={-1} onMouseDown={(e) => onDown(side, e)}>
              <PlusIcon size={12} />
            </button>
          )}
          {l && <Marker type={l.type} />}
          {l && <Code text={l.text} html={html} />}
        </span>
      </>
    );
  };
  return (
    <div className={cx(styles.splitLine, active && styles.cursor)} onClick={onClick} role="row">
      {cell(left, 'LEFT', leftSelected, leftHtml)}
      {cell(right, 'RIGHT', rightSelected, rightHtml)}
    </div>
  );
});

/** Line text, or the server's highlighted HTML for it (`hl-*` spans of escaped text). */
function Code({ text, html }: { text: string; html?: string }) {
  if (html === undefined) return <>{text}</>;
  return <span dangerouslySetInnerHTML={{ __html: html }} />;
}

/** "Expand up / down / all" controls of a hidden range (P37). */
function Expander({ ctl, busy, onExpand }: { ctl: GapControls; busy: boolean; onExpand: (dir: ExpandDir) => void }) {
  const n = Number.isFinite(ctl.hidden) ? ctl.hidden : null;
  const btn = (dir: ExpandDir, label: string, Icon: typeof UnfoldIcon) => (
    <button type="button" className={styles.expandBtn} aria-label={label} title={label} disabled={busy} onClick={() => onExpand(dir)}>
      <Icon size={14} />
    </button>
  );
  return (
    <span className={styles.expander}>
      {busy ? (
        <Spinner size={12} />
      ) : (
        <>
          {ctl.down && btn('down', n != null && n <= 20 ? `Expand ${n} hidden line${n === 1 ? '' : 's'}` : 'Expand down', FoldDownIcon)}
          {ctl.up && !(ctl.down && n != null && n <= 20) && btn('up', n != null && n <= 20 ? `Expand ${n} hidden line${n === 1 ? '' : 's'}` : 'Expand up', FoldUpIcon)}
          {ctl.all && btn('all', `Expand all${n != null ? ` ${n} lines` : ''}`, UnfoldIcon)}
        </>
      )}
    </span>
  );
}

const LEVEL_ICON = { failure: XCircleIcon, warning: AlertIcon, notice: InfoIcon } as const;

/** Check-run annotations ending on one line (P37). */
function CheckAnnotations({ items }: { items: readonly CommitAnnotation[] }) {
  return (
    <div className={styles.annotations}>
      {items.map((a, i) => {
        const Icon = LEVEL_ICON[a.annotation_level] ?? InfoIcon;
        return (
          <div key={i} className={cx(styles.annotation, styles[`annotation_${a.annotation_level}`])} data-annotation-level={a.annotation_level}>
            <Icon size={16} className={styles.annotationIcon} />
            <div className={styles.annotationBody}>
              <div className={styles.annotationHead}>
                <strong>{a.check_run_name}</strong>
                {a.title && <span> · {a.title}</span>}
                <span className={styles.annotationLines}>
                  {a.start_line === a.end_line ? `Line ${a.end_line}` : `Lines ${a.start_line}–${a.end_line}`}
                </span>
              </div>
              <div className={styles.annotationMessage}>{a.message}</div>
              {a.raw_details && (
                <details>
                  <summary>Raw details</summary>
                  <pre>{a.raw_details}</pre>
                </details>
              )}
            </div>
          </div>
        );
      })}
    </div>
  );
}

/** File header actions with a source: rich diff toggle, view file, edit file, copy path (P37). */
function FileActions({ file, extras }: { file: DiffFileEntry; extras: DiffExtras }) {
  const { source } = extras;
  const status = file.status === 'removed' ? 'deleted' : file.status;
  const base = `/${source.owner}/${source.repo}`;
  const enc = file.path.split('/').map(encodeURIComponent).join('/');
  const rich = extras.rich.has(file.path);
  return (
    <span className={styles.fileActions}>
      {isMarkdown(file.path) && status !== 'deleted' && !file.binary && (
        <span className={styles.richToggle} role="group" aria-label="Diff display">
          <button type="button" aria-pressed={!rich} title="Display the source diff" onClick={() => rich && extras.toggleRich(file.path)}>
            <FileCodeIcon size={14} />
          </button>
          <button type="button" aria-pressed={rich} title="Display the rich diff" onClick={() => !rich && extras.toggleRich(file.path)}>
            <FileIcon size={14} />
          </button>
        </span>
      )}
      <button
        type="button"
        className={styles.fileAction}
        aria-label="Copy file path"
        title="Copy file path"
        onClick={() =>
          void navigator.clipboard?.writeText(file.path).then(
            () => toast({ kind: 'success', title: 'Copied path' }),
            () => toast({ kind: 'error', title: 'Couldn’t copy' }),
          )
        }
      >
        <CopyIcon size={14} />
      </button>
      {status !== 'deleted' && (
        <Link className={styles.fileAction} to={`${base}/blob/${source.newRef}/${enc}`} aria-label="View file" title={`View file @ ${source.newRef.slice(0, 7)}`}>
          <EyeIcon size={14} />
        </Link>
      )}
      {status !== 'deleted' && source.editRef && !file.binary && (
        <Link className={styles.fileAction} to={`${base}/edit/${source.editRef.split('/').map(encodeURIComponent).join('/')}/${enc}`} aria-label="Edit file" title={`Edit file on ${source.editRef}`}>
          <PencilIcon size={14} />
        </Link>
      )}
    </span>
  );
}

function LoadingFile({ path, onNeed }: { path: string; onNeed?: (path: string) => void }) {
  useEffect(() => {
    onNeed?.(path);
  }, [path, onNeed]);
  return (
    <div className={styles.binary}>
      <Spinner size={14} /> Loading diff…
    </div>
  );
}

function MoreFiles({ count, onNeed }: { count: number; onNeed?: () => void }) {
  useEffect(() => {
    onNeed?.();
  }, [onNeed]);
  return (
    <div className={styles.more}>
      <Spinner size={14} /> Loading {count} more file{count === 1 ? '' : 's'}…
    </div>
  );
}

// ------------------------------------------------------------------ tree

interface TreeNode {
  name: string;
  path: string;
  children: Map<string, TreeNode>;
  file?: DiffFileEntry;
}

function buildTree(files: readonly DiffFileEntry[]): TreeNode {
  const root: TreeNode = { name: '', path: '', children: new Map() };
  for (const file of files) {
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
    node.file = file;
  }
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

function TreeView({
  node,
  depth,
  isViewed,
  active,
  counts,
  onPick,
}: {
  node: TreeNode;
  depth: number;
  isViewed?: (p: string) => boolean;
  active?: string;
  counts?: (p: string) => number;
  onPick: (path: string) => void;
}) {
  const [open, setOpen] = useState(true);
  const children = [...node.children.values()].sort((a, b) => (!!a.file === !!b.file ? a.name.localeCompare(b.name) : a.file ? 1 : -1));
  if (node.file) {
    const f = node.file;
    const n = counts?.(f.path) ?? 0;
    const st = f.status === 'removed' ? 'deleted' : f.status;
    return (
      <button
        type="button"
        className={cx(styles.treeItem, isViewed?.(f.path) && styles.treeViewed, active === f.path && styles.treeActive)}
        style={{ paddingLeft: 8 + depth * 12 }}
        onClick={() => onPick(f.path)}
        title={f.path}
      >
        <FileIcon size={14} />
        <span className={styles.treeName}>{node.name}</span>
        {n > 0 && (
          <span className={styles.treeCount}>
            <CommentIcon size={12} />
            {n}
          </span>
        )}
        <span className={cx(styles.dotStatus, styles[st])} />
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
      {open && children.map((c) => <TreeView key={c.path} node={c} depth={node.name ? depth + 1 : depth} isViewed={isViewed} active={active} counts={counts} onPick={onPick} />)}
    </div>
  );
}

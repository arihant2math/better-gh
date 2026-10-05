import { observer } from 'mobx-react-lite';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { load } from '../../api/cache';
import { ApiError } from '../../api/client';
import { getPullFilePatch, listPullFiles } from '../../api/endpoints';
import type { RestDiffEntry } from '../../api/types';
import { DiffView, type DiffAnnotations, type DiffFileEntry, type LineSelection } from '../../components/diff/DiffView';
import { parsePatch, type DiffHunk } from '../../components/diff/parseDiff';
import { setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { useComputed, usePullDetails } from '../../sync/hooks';
import type { Issue, Repo } from '../../sync/models';
import { addPendingComment, addReviewComment } from '../../sync/pullMutations';
import { pendingReview, threadsForPull, type ReviewThread } from '../../sync/pullSelectors';
import { repoFullName } from '../../sync/selectors';
import { Button, IconButton } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { AlertIcon, ColumnsIcon, CommentIcon, FilterIcon, RowsIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { MarkdownEditor } from '../issues/Timeline';
import { ReviewButton } from './ReviewButton';
import styles from './Review.module.css';
import { LineSourceContext, ReviewThreadView, type LineSource } from './ReviewThread';
import { useViewed } from './viewed';

const PAGE = 100;
/** Files with more changed lines start collapsed behind "Load diff" (GitHub: "Large diffs are not rendered by default"). */
const LARGE_LINES = 1500;

type Mode = 'unified' | 'split';

function readPref(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}
function writePref(key: string, v: string): void {
  try {
    localStorage.setItem(key, v);
  } catch {
    /* private mode */
  }
}

/**
 * Changed-file list of a PR, loaded page by page (100 files per request,
 * patches included) as the user scrolls; immutable per base/head SHA.
 */
function usePullFiles(repo: Repo, pr: Issue) {
  const base = `files:${repo.owner}/${repo.name}#${pr.number}@${pr.baseSha}...${pr.headSha}`;
  const total = pr.changedFiles ?? 0;
  const [pages, setPages] = useState<{ key: string; list: RestDiffEntry[][]; error?: unknown; done: boolean }>({ key: base, list: [], done: false });
  const loading = useRef(false);
  const state = pages.key === base ? pages : { key: base, list: [], done: false };

  const loadNext = useCallback(() => {
    if (loading.current) return;
    const cur = pages.key === base ? pages : { key: base, list: [] as RestDiffEntry[][], done: false };
    if (cur.done) return;
    const page = cur.list.length + 1;
    loading.current = true;
    load(`${base}:${page}`, () => listPullFiles(repo.owner, repo.name, pr.number, page, PAGE), { immutable: true }).then(
      (list) => {
        loading.current = false;
        setPages((p) => {
          const prev = p.key === base ? p.list : [];
          if (prev.length >= page) return p;
          return { key: base, list: [...prev, list], done: list.length < PAGE };
        });
      },
      (error: unknown) => {
        loading.current = false;
        setPages((p) => ({ ...(p.key === base ? p : { key: base, list: [] }), error, done: true }));
      },
    );
  }, [base, pages, repo.owner, repo.name, pr.number]);

  useEffect(() => {
    if (state.list.length === 0 && !state.done) loadNext();
  }, [state.list.length, state.done, loadNext]);

  const entries = useMemo(() => state.list.flat(), [state.list]);
  const pending = state.done ? 0 : Math.max(total - entries.length, entries.length === 0 ? 1 : 0);
  return { entries, pending, error: state.error, loadNext, cacheKey: base };
}

function toEntry(e: RestDiffEntry): DiffFileEntry {
  const large = e.additions + e.deletions > LARGE_LINES;
  const hunks = e.patch ? (large ? null : parsePatch(e.patch)) : e.patch === undefined || e.status === 'renamed' ? (e.changes ? null : []) : null;
  return {
    path: e.filename,
    oldPath: e.previous_filename ?? undefined,
    status: e.status === 'unchanged' ? 'modified' : e.status,
    additions: e.additions,
    deletions: e.deletions,
    binary: !e.patch && e.additions + e.deletions === 0 && e.status !== 'renamed' && !!e.changes,
    hunks,
    unavailable: large ? 'Large diffs are not rendered by default.' : !e.patch && (e.changes ?? e.additions + e.deletions) > 0 ? 'Load diff — this patch is too large to include in the file list.' : undefined,
  };
}

/** `[path, side, line]` anchor key used by DiffView (`R12`, `L3`). */
function anchorOf(t: ReviewThread): string {
  if (t.outdated || t.root.subjectType === 'file' || t.root.line == null) return 'file';
  return `${t.root.side === 'LEFT' ? 'L' : 'R'}${t.root.line}`;
}

export default observer(function FilesTab({ repo, pr }: { repo: Repo; pr: Issue }) {
  usePullDetails(pr.id);
  const query = useQuery();
  const mode: Mode = (query.get('diff') as Mode | null) ?? (readPref('bgh:diff:mode', 'unified') as Mode);
  const whitespace = query.get('w') === '1';
  const [filter, setFilter] = useState('');
  const [hideViewed, setHideViewed] = useState(false);
  const [selection, setSelection] = useState<LineSelection | null>(null);
  const [draft, setDraft] = useState('');
  const [jumpTo, setJumpTo] = useState<{ path: string; nonce: number } | null>(null);
  const filterRef = useRef<HTMLInputElement>(null);
  const full = repoFullName(repo);

  const { entries, pending, error, loadNext } = usePullFiles(repo, pr);
  // Filtering needs the whole list: keep paging in the background.
  useEffect(() => {
    if ((filter || hideViewed) && pending > 0) loadNext();
  }, [filter, hideViewed, pending, entries.length, loadNext]);
  const viewed = useViewed(pr, entries);

  // Per-file patches fetched on demand: whitespace-insensitive mode, truncated or "large" files.
  const [patches, setPatches] = useState<Map<string, DiffHunk[] | null>>(() => new Map());
  const patchKey = (path: string) => `${pr.headSha}:${whitespace ? 'w' : ''}:${path}`;
  const requested = useRef(new Set<string>());
  const fetchPatch = useCallback(
    (path: string) => {
      const k = `${pr.headSha}:${whitespace ? 'w' : ''}:${path}`;
      if (requested.current.has(k)) return;
      requested.current.add(k);
      load(`patch:${repo.owner}/${repo.name}#${pr.number}@${pr.baseSha}...${k}`, () => getPullFilePatch(repo.owner, repo.name, pr.number, path, whitespace), { immutable: true }).then(
        (p) => setPatches((m) => new Map(m).set(k, p.patch ? parsePatch(p.patch) : p.truncated ? null : [])),
        () => setPatches((m) => new Map(m).set(k, null)),
      );
    },
    [repo.owner, repo.name, pr.number, pr.baseSha, pr.headSha, whitespace],
  );

  const files = useMemo(() => {
    const terms = filter.toLowerCase().split(/\s+/).filter(Boolean);
    return entries
      .filter((e) => terms.every((t) => e.filename.toLowerCase().includes(t)))
      .filter((e) => !hideViewed || !viewed.isViewed(e.filename))
      .map((e) => {
        const base = toEntry(e);
        const k = patchKey(e.filename);
        if (patches.has(k)) {
          const hunks = patches.get(k)!;
          return { ...base, hunks, unavailable: hunks === null ? 'This diff is too large to display.' : undefined };
        }
        // Whitespace-insensitive diffs come from the per-file endpoint, lazily.
        if (whitespace && !base.binary) return { ...base, hunks: undefined };
        return base;
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps -- patchKey derives from the deps below
  }, [entries, filter, hideViewed, viewed, patches, whitespace, pr.headSha]);

  // Collapsed: viewed files by default; explicit toggles override.
  const [toggled, setToggled] = useState<Map<string, boolean>>(() => new Map());
  const collapsed = useMemo(() => {
    const s = new Set<string>();
    for (const f of files) {
      const t = toggled.get(f.path);
      if (t ?? viewed.isViewed(f.path)) s.add(f.path);
    }
    return s;
  }, [files, toggled, viewed]);

  const threads = useComputed(() => threadsForPull(pr.id), [pr.id]);
  const byFile = useMemo(() => {
    const m = new Map<string, Map<string, ReviewThread[]>>();
    for (const t of threads) {
      let f = m.get(t.path);
      if (!f) m.set(t.path, (f = new Map()));
      const a = anchorOf(t);
      f.set(a, [...(f.get(a) ?? []), t]);
    }
    return m;
  }, [threads]);
  // `end === 0` = a comment on the whole file (subject_type: file).
  const selAnchor = selection ? (selection.end === 0 ? 'file' : `${selection.side === 'LEFT' ? 'L' : 'R'}${selection.end}`) : null;

  const hasPending = !!pendingReview(pr.id);
  const submit = (asReview: boolean) => {
    if (!selection || !draft.trim()) return;
    const loc = selection.end === 0 ? { path: selection.path, subjectType: 'file' as const, commitId: pr.headSha } : {
      path: selection.path,
      line: selection.end,
      side: selection.side,
      startLine: selection.start !== selection.end ? selection.start : undefined,
      startSide: selection.start !== selection.end ? selection.side : undefined,
      commitId: pr.headSha,
    };
    const res = asReview ? addPendingComment(pr, loc, draft.trim()) : addReviewComment(pr, loc, draft.trim());
    const sel = selection;
    const text = draft;
    // The queue rolls back and toasts the server's message; give the text back.
    res.done.catch(() => {
      setSelection((cur) => cur ?? sel);
      setDraft((cur) => cur || text);
    });
    setSelection(null);
    setDraft('');
  };

  const lineSource: LineSource = useCallback(
    (path, start, end) => {
      const f = files.find((x) => x.path === path);
      if (!f?.hunks) return null;
      const out: string[] = [];
      for (const h of f.hunks) for (const l of h.lines) if (l.newNo != null && l.type !== 'del' && l.newNo >= start && l.newNo <= end) out.push(l.text);
      return out.length === end - start + 1 ? out : null;
    },
    [files],
  );

  const annotations: DiffAnnotations = useMemo(
    () => ({
      anchors: (path) => {
        const keys = [...(byFile.get(path)?.keys() ?? [])];
        if (selection && selection.path === path && selAnchor && !keys.includes(selAnchor)) keys.push(selAnchor);
        return keys;
      },
      render: (path, anchor) => (
        <>
          {(byFile.get(path)?.get(anchor) ?? []).map((t) => (
            <ReviewThreadView key={t.id} thread={t} pr={pr} repo={full} defaultCollapsed={t.resolved || t.outdated} showPath={false} />
          ))}
          {selection && selection.path === path && anchor === selAnchor && (
            <div className={styles.composer}>
              <div className={styles.composerLabel}>
                {selection.end === 0 ? 'Comment on this file' : selection.start === selection.end ? `Comment on line ${selection.side === 'LEFT' ? 'L' : 'R'}${selection.end}` : `Comment on lines ${selection.side === 'LEFT' ? 'L' : 'R'}${selection.start} to ${selection.side === 'LEFT' ? 'L' : 'R'}${selection.end}`}
              </div>
              <MarkdownEditor
                value={draft}
                onChange={setDraft}
                repo={full}
                autoFocus
                placeholder="Leave a comment"
                submitLabel={hasPending ? 'Add review comment' : 'Start a review'}
                onSubmit={() => submit(true)}
                onCancel={() => {
                  setSelection(null);
                  setDraft('');
                }}
                extraActions={
                  <>
                    {selection.side === 'RIGHT' && selection.end > 0 && (
                      <Button
                        size="sm"
                        variant="ghost"
                        title="Insert a suggestion"
                        onClick={() => {
                          const lines = lineSource(selection.path, selection.start, selection.end) ?? [];
                          setDraft((d) => `${d}${d && !d.endsWith('\n') ? '\n' : ''}\`\`\`suggestion\n${lines.join('\n')}\n\`\`\`\n`);
                        }}
                      >
                        Suggest
                      </Button>
                    )}
                    {!hasPending && (
                      <Button size="sm" disabled={!draft.trim()} onClick={() => submit(false)}>
                        Add single comment
                      </Button>
                    )}
                  </>
                }
              />
            </div>
          )}
        </>
      ),
      onSelect: (sel) => {
        setSelection(sel);
      },
      selection,
      commentCount: (path) => {
        let n = 0;
        for (const ts of byFile.get(path)?.values() ?? []) for (const t of ts) n += t.comments.length;
        return n;
      },
    }),
    // eslint-disable-next-line react-hooks/exhaustive-deps -- submit/lineSource read current state
    [byFile, selection, selAnchor, draft, hasPending, pr, full, lineSource],
  );

  useShortcuts('Files changed', {
    t: {
      handler: () => {
        filterRef.current?.focus();
      },
      description: 'Filter files',
      group: 'Files changed',
    },
    s: {
      handler: () => setMode(mode === 'split' ? 'unified' : 'split'),
      description: 'Toggle split / unified diff',
      group: 'Files changed',
    },
    w: {
      handler: () => setQuery({ w: whitespace ? null : '1' }),
      description: 'Toggle hide whitespace',
      group: 'Files changed',
    },
    escape: {
      handler: () => {
        if (!selection) return false;
        setSelection(null);
      },
    },
  });

  const setMode = (m: Mode) => {
    writePref('bgh:diff:mode', m);
    setQuery({ diff: m === 'unified' ? null : m });
  };

  if (error && entries.length === 0) {
    const tooLarge = error instanceof ApiError && (error.status === 406 || error.status === 422);
    return <EmptyState icon={AlertIcon} title={tooLarge ? 'This diff is too large to display' : 'Couldn’t load the changed files'} />;
  }

  const toolbar = (
    <div className={styles.toolbar}>
      <Input
        ref={filterRef}
        size="sm"
        className={styles.filter}
        leadingIcon={FilterIcon}
        placeholder="Filter changed files"
        aria-label="Filter changed files"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            setFilter('');
            (e.target as HTMLInputElement).blur();
          } else if (e.key === 'Enter' && files[0]) setJumpTo({ path: files[0].path, nonce: Date.now() });
        }}
      />
      <label className={styles.check}>
        <input type="checkbox" checked={hideViewed} onChange={(e) => setHideViewed(e.target.checked)} /> Hide viewed
      </label>
      <label className={styles.check}>
        <input type="checkbox" checked={whitespace} onChange={(e) => setQuery({ w: e.target.checked ? '1' : null })} /> Hide whitespace
      </label>
      <span className={styles.spacer} />
      <div className={styles.toggle} role="group" aria-label="Diff view">
        <button type="button" aria-pressed={mode === 'unified'} onClick={() => setMode('unified')} title="Unified (s)">
          <RowsIcon size={14} /> Unified
        </button>
        <button type="button" aria-pressed={mode === 'split'} onClick={() => setMode('split')} title="Split (s)">
          <ColumnsIcon size={14} /> Split
        </button>
      </div>
      <ReviewButton pr={pr} repo={full} />
    </div>
  );

  return (
    <LineSourceContext.Provider value={lineSource}>
      <DiffView
        files={files}
        mode={mode}
        keyboard
        header={toolbar}
        collapsed={collapsed}
        onToggleCollapsed={(p) => setToggled((m) => new Map(m).set(p, !collapsed.has(p)))}
        isViewed={viewed.isViewed}
        onToggleViewed={(p) => {
          const now = !viewed.isViewed(p);
          viewed.toggle(p);
          setToggled((m) => new Map(m).set(p, now));
        }}
        onNeedFile={fetchPatch}
        fileActions={(f) => (
          <IconButton
            icon={CommentIcon}
            size="sm"
            label="Comment on this file"
            onClick={() => {
              setSelection({ path: f.path, side: 'RIGHT', start: 0, end: 0 });
              setToggled((m) => new Map(m).set(f.path, false));
            }}
          />
        )}
        annotations={annotations}
        pendingFiles={filter || hideViewed ? 0 : pending}
        onNeedMoreFiles={loadNext}
        jumpTo={jumpTo}
        emptyText={filter ? 'No changed files match the filter.' : 'No files changed.'}
      />
    </LineSourceContext.Provider>
  );
});

/**
 * Per-file extras of the diff viewer (P37), fetched lazily as files scroll
 * into view: syntax highlighting of both sides, context expansion state and
 * lines, the rendered ("rich") toggle and inline check-run annotations.
 */
import { useCallback, useMemo, useRef, useState } from 'react';
import { load, useResource } from '../../api/cache';
import { getBlobLines, listCommitAnnotations } from '../../api/endpoints';
import type { CommitAnnotation } from '../../api/types';
import type { DiffFileEntry } from './DiffView';
import { expandGap, expandHunks, gapControls, gapsOf, type ExpandDir, type Expansion, type Gap, type GapControls, type ShownHunk } from './expand';
import { MAX_HIGHLIGHT_CHANGES, highlightMatches, type FileHighlight } from './highlight';
import type { DiffHunk } from './parseDiff';

/** Where a diff's two sides live (enables highlighting, expansion, image/rich diffs and file actions). */
export interface DiffSource {
  owner: string;
  repo: string;
  /** Old side: a commit SHA or `base...head` (their merge base, as in a pull request). */
  oldRef: string;
  /** New side: a full commit SHA. */
  newRef: string;
  /**
   * Show check-run annotations of `newRef` inline; the value versions the
   * fetch (e.g. the ids of completed check runs), `''` = none to fetch yet.
   */
  annotations?: string;
  /** Branch for "Edit file" (only when the viewer may push to it). */
  editRef?: string | null;
}

interface ExpState {
  hunks: readonly DiffHunk[];
  exp: Expansion;
}

interface CtxLines {
  lines: ReadonlyMap<number, string>;
  total?: number;
}

export interface DiffExtras {
  source: DiffSource;
  highlight(path: string): FileHighlight | undefined;
  /** Start fetching highlighting for a visible file. */
  ensureHighlight(file: DiffFileEntry): void;
  /** Hunks with expanded context, or null when nothing is expanded. */
  shown(file: DiffFileEntry): ShownFile | null;
  expansion(path: string, hunks: readonly DiffHunk[]): Expansion;
  expand(file: DiffFileEntry, gap: number, dir: ExpandDir): void;
  /** A gap is being loaded (`path:gap`). */
  busy: ReadonlySet<string>;
  rich: ReadonlySet<string>;
  toggleRich(path: string): void;
  /** Annotations per file, keyed by end line on the new side. */
  annotations(path: string): ReadonlyMap<number, CommitAnnotation[]> | undefined;
}

const EMPTY: Expansion = new Map();

export interface ShownFile {
  hunks: ShownHunk[];
  tail: Gap | null;
  /** Expander controls per gap index. */
  controls: ReadonlyMap<number, GapControls>;
}

export function useDiffExtras(source: DiffSource | undefined): DiffExtras | undefined {
  const scope = source ? `${source.owner}/${source.repo}:${source.oldRef}:${source.newRef}` : '';
  const [state, setState] = useState<{
    scope: string;
    hl: ReadonlyMap<string, FileHighlight | null>;
    exp: ReadonlyMap<string, ExpState>;
    ctx: ReadonlyMap<string, CtxLines>;
    rich: ReadonlySet<string>;
    busy: ReadonlySet<string>;
  }>(() => ({ scope, hl: new Map(), exp: new Map(), ctx: new Map(), rich: new Set(), busy: new Set() }));
  const cur = state.scope === scope ? state : { scope, hl: new Map(), exp: new Map(), ctx: new Map(), rich: new Set<string>(), busy: new Set<string>() };
  const requested = useRef(new Set<string>());
  const update = useCallback(
    (fn: (s: typeof cur) => Partial<typeof cur>) =>
      setState((s) => {
        const base = s.scope === scope ? s : { scope, hl: new Map(), exp: new Map(), ctx: new Map(), rich: new Set<string>(), busy: new Set<string>() };
        return { ...base, ...fn(base) };
      }),
    [scope],
  );

  const annKey = source?.annotations ? `annotations:${source.owner}/${source.repo}@${source.newRef}:${source.annotations}` : null;
  const { data: annList } = useResource(annKey, () => listCommitAnnotations(source!.owner, source!.repo, source!.newRef));
  const annByPath = useMemo(() => {
    const m = new Map<string, Map<number, CommitAnnotation[]>>();
    for (const a of annList ?? []) {
      let f = m.get(a.path);
      if (!f) m.set(a.path, (f = new Map()));
      f.set(a.end_line, [...(f.get(a.end_line) ?? []), a]);
    }
    return m;
  }, [annList]);

  const ensureHighlight = useCallback(
    (file: DiffFileEntry) => {
      if (!source || !file.hunks?.length || file.binary) return;
      if (file.additions + file.deletions > MAX_HIGHLIGHT_CHANGES) return;
      const key = `${scope}:${file.path}`;
      if (requested.current.has(key)) return;
      requested.current.add(key);
      const hunks = file.hunks;
      const status = file.status === 'removed' ? 'deleted' : file.status;
      const side = (ref: string, path: string) =>
        load(`blob-lines:${source.owner}/${source.repo}@${ref}:${path}:hl`, () => getBlobLines(source.owner, source.repo, ref, path, { hl: true, text: false }), { immutable: true }).then(
          (r) => r,
          () => null,
        );
      const needOld = status !== 'added' && hunks.some((h) => h.lines.some((l) => l.type === 'del' || l.type === 'ctx'));
      const needNew = status !== 'deleted';
      void Promise.all([needOld ? side(source.oldRef, file.oldPath ?? file.path) : Promise.resolve(null), needNew ? side(source.newRef, file.path) : Promise.resolve(null)]).then(([o, n]) => {
        const hl: FileHighlight | null = o?.html || n?.html ? { old: o?.html ?? null, new: n?.html ?? null } : null;
        const ok = hl && highlightMatches(hunks, hl) ? hl : null;
        update((s) => {
          const out: Partial<typeof s> = { hl: new Map(s.hl).set(file.path, ok) };
          // The new side's length tells whether anything is hidden after the last hunk.
          if (n && !n.binary && s.ctx.get(file.path)?.total === undefined) {
            out.ctx = new Map(s.ctx).set(file.path, { lines: s.ctx.get(file.path)?.lines ?? new Map(), total: n.total_lines });
          }
          return out;
        });
      });
    },
    [source, scope, update],
  );

  const expansion = useCallback((path: string, hunks: readonly DiffHunk[]) => {
    const e = cur.exp.get(path);
    return e && e.hunks === hunks ? e.exp : EMPTY;
  }, [cur.exp]);

  const expand = useCallback(
    (file: DiffFileEntry, gapIndex: number, dir: ExpandDir) => {
      if (!source || !file.hunks?.length) return;
      const hunks = file.hunks;
      const ctx = cur.ctx.get(file.path);
      const gap = gapsOf(hunks, ctx?.total)[gapIndex];
      if (!gap) return;
      const exp = expansion(file.path, hunks);
      const step = expandGap(gap, exp.get(gapIndex), dir);
      if (!step) return;
      const apply = (total: number | undefined, lines: ReadonlyMap<number, string>) =>
        update((s) => {
          const prev = s.exp.get(file.path);
          const base = prev && prev.hunks === hunks ? prev.exp : EMPTY;
          // Re-derive against the (possibly now known) file length.
          const g = gapsOf(hunks, total)[gapIndex]!;
          const next = expandGap(g, base.get(gapIndex), dir);
          const busy = new Set(s.busy);
          busy.delete(`${file.path}:${gapIndex}`);
          const merged = new Map(s.ctx.get(file.path)?.lines ?? []);
          for (const [n, t] of lines) merged.set(n, t);
          return {
            busy,
            ctx: new Map(s.ctx).set(file.path, { lines: merged, total: total ?? s.ctx.get(file.path)?.total }),
            exp: next ? new Map(s.exp).set(file.path, { hunks, exp: new Map(base).set(gapIndex, next.state) }) : s.exp,
          };
        });
      const end = Number.isFinite(step.load.end) ? step.load.end : step.load.start + 19;
      let missing = false;
      for (let n = step.load.start; n <= end; n++) if (!ctx?.lines.has(n)) missing = true;
      if (!missing && ctx) {
        apply(ctx.total, ctx.lines);
        return;
      }
      update((s) => ({ busy: new Set(s.busy).add(`${file.path}:${gapIndex}`) }));
      getBlobLines(source.owner, source.repo, source.newRef, file.path, { start: step.load.start, end }).then(
        (r) => {
          const lines = new Map<number, string>();
          (r.lines ?? []).forEach((t, i) => lines.set(r.start + i, t));
          apply(r.total_lines, lines);
        },
        () =>
          update((s) => {
            const busy = new Set(s.busy);
            busy.delete(`${file.path}:${gapIndex}`);
            return { busy };
          }),
      );
    },
    [source, cur.ctx, expansion, update],
  );

  const shown = useCallback(
    (file: DiffFileEntry) => {
      if (!source || !file.hunks?.length || file.binary) return null;
      const exp = expansion(file.path, file.hunks);
      const ctx = cur.ctx.get(file.path);
      const { hunks, tail } = expandHunks(file.hunks, exp, (n) => ctx?.lines.get(n), ctx?.total);
      const controls = new Map<number, GapControls>();
      for (const g of gapsOf(file.hunks, ctx?.total)) controls.set(g.index, gapControls(g, exp.get(g.index), file.hunks.length));
      return { hunks, tail, controls };
    },
    [source, expansion, cur.ctx],
  );

  const toggleRich = useCallback(
    (path: string) =>
      update((s) => {
        const rich = new Set(s.rich);
        if (rich.has(path)) rich.delete(path);
        else rich.add(path);
        return { rich };
      }),
    [update],
  );

  const hl = cur.hl;
  return useMemo(
    () =>
      source && {
        source,
        highlight: (path: string) => hl.get(path) ?? undefined,
        ensureHighlight,
        shown,
        expansion,
        expand,
        busy: cur.busy,
        rich: cur.rich,
        toggleRich,
        annotations: (path: string) => annByPath.get(path),
      },
    [source, hl, ensureHighlight, shown, expansion, expand, cur.busy, cur.rich, toggleRich, annByPath],
  );
}

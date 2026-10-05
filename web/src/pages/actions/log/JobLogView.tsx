/**
 * GitHub Actions job log viewer: live SSE stream, per-step collapsible
 * sections, `##[group]` folding, ANSI colors, permalinks, search and tail
 * follow — over ONE virtualized list of fixed-height rows, so 100k+ lines
 * scroll smoothly.
 *
 * Log state lives outside React (`JobLog`, mutated by the stream) and a
 * version counter bumped at most once per animation frame drives renders.
 */
import { useVirtualizer } from '@tanstack/react-virtual';
import {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useReducer,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent,
  type ReactNode,
} from 'react';
import { jobLogStreamPath, jobLogUrl, type JobStep, type WorkflowJob } from '../../../api/actions';
import { useShortcuts } from '../../../shortcuts/useShortcuts';
import { Kbd } from '../../../ui/Badge';
import { cx, IconButton } from '../../../ui/Button';
import {
  ArrowDownIcon,
  ArrowUpIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  ClockIcon,
  DownloadIcon,
  FoldIcon,
  SearchIcon,
  UnfoldIcon,
} from '../../../ui/icons';
import { Input } from '../../../ui/Input';
import { Spinner } from '../../../ui/Spinner';
import { Tooltip } from '../../../ui/Tooltip';
import { Duration, StatusIcon, visualStatus } from '../shared';
import { markMatches, parseAnsi, type AnsiSpan } from './ansi';
import styles from './JobLog.module.css';
import { JobLog, LogSearch, type LogLine } from './parse';
import { buildLayout, GroupState, VisibleCache, type Row } from './rows';
import { streamJobLog } from './sse';

export interface JobLogViewProps {
  owner: string;
  repo: string;
  /** Live job: re-render with the latest object as status / steps change. */
  job: WorkflowJob;
  className?: string;
}

const LINE_H = 20;
const STEP_H = 36;
/** Gutter + timestamp + padding, in px, added to the widest line for horizontal scroll. */
const GUTTER_PX = 64;
const TS_PX = 96;
const TS_KEY = 'bgh.jobLog.timestamps';
const HASH = /^#step:(\d+):(\d+)(?:-(\d+))?$/;

type Phase = 'connecting' | 'streaming' | 'done' | 'error';

interface Selection {
  step: number;
  /** 1-based, inclusive. */
  from: number;
  to: number;
  anchor: number;
}

function readHash(): Selection | null {
  if (typeof location === 'undefined') return null;
  const m = HASH.exec(location.hash);
  if (!m) return null;
  const step = Number(m[1]);
  const a = Number(m[2]);
  const b = m[3] ? Number(m[3]) : a;
  if (!a || !b) return null;
  return { step, from: Math.min(a, b), to: Math.max(a, b), anchor: a };
}

function readTimestampsPref(): boolean {
  try {
    return localStorage.getItem(TS_KEY) === '1';
  } catch {
    return false;
  }
}

const isFailed = (s: JobStep | undefined) => !!s && visualStatus(s.status, s.conclusion) === 'failure';
const isRunning = (s: JobStep | undefined) => s?.status === 'in_progress';

const pad = (n: number, w = 2) => String(n).padStart(w, '0');
function formatTime(ts: number): string {
  const d = new Date(ts);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`;
}

/** Parsed ANSI spans per line, computed only for rendered rows. */
const spanCache = new WeakMap<LogLine, AnsiSpan[]>();
function spansOf(line: LogLine): AnsiSpan[] {
  let s = spanCache.get(line);
  if (!s) spanCache.set(line, (s = parseAnsi(line.text)));
  return s;
}

const rowKey = (r: Row) => (r.kind === 'line' ? `${r.step}:${r.line}` : `${r.kind}:${r.step}`);

export function JobLogView({ owner, repo, job, className }: JobLogViewProps) {
  const [log] = useState(() => new JobLog());
  const [groups] = useState(() => new GroupState());
  const [visibleCache] = useState(() => new VisibleCache());
  const [search] = useState(() => new LogSearch());
  const [version, bump] = useReducer((v: number) => v + 1, 0);
  const [phase, setPhase] = useState<Phase>('connecting');

  const live = job.status !== 'completed';
  const steps = job.steps;
  const stepByNumber = useMemo(() => new Map(steps.map((s) => [s.number, s])), [steps]);

  // ------------------------------------------------------------ stream
  useEffect(() => {
    const ctrl = new AbortController();
    let raf = 0;
    const schedule = () => {
      if (!raf) {
        raf = requestAnimationFrame(() => {
          raf = 0;
          bump();
        });
      }
    };
    log.reset();
    setPhase('connecting');
    bump();
    void streamJobLog(
      jobLogStreamPath(job.id),
      {
        onReset: () => {
          log.reset();
          setPhase('streaming');
          schedule();
        },
        onLog: (step, text) => {
          log.append(step, text);
          schedule();
        },
        onDone: () => {
          log.finish();
          setPhase('done');
          schedule();
        },
        onError: () => setPhase('error'),
      },
      ctrl.signal,
    );
    return () => {
      ctrl.abort();
      cancelAnimationFrame(raf);
    };
  }, [job.id, log]);

  // ------------------------------------------------------------ sections
  const order = useMemo(() => {
    const nums = new Set(steps.map((s) => s.number));
    for (const n of log.steps.keys()) nums.add(n);
    return [...nums].sort((a, b) => a - b);
    // `version` covers steps that only exist in the log so far.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [steps, log, version]);

  const [sel, setSel] = useState<Selection | null>(readHash);
  const pendingHash = useRef<Selection | null>(sel);
  const [overrides, setOverrides] = useState<ReadonlyMap<number, boolean>>(() => (sel ? new Map([[sel.step, true]]) : new Map()));

  // A step that ran while we watched stays open after it finishes.
  useEffect(() => {
    const running = steps.filter((s) => isRunning(s) && !overrides.has(s.number));
    if (running.length) setOverrides((o) => new Map([...o, ...running.map((s) => [s.number, true] as const)]));
  }, [steps, overrides]);

  const expanded = useMemo(() => {
    const out = new Set<number>();
    for (const n of order) {
      const s = stepByNumber.get(n);
      const def = s ? isFailed(s) || isRunning(s) : steps.length === 0;
      if (overrides.get(n) ?? def) out.add(n);
    }
    return out;
  }, [order, stepByNumber, overrides, steps.length]);

  const layout = useMemo(
    () =>
      buildLayout(
        order.map((n) => ({ step: n, expanded: expanded.has(n), log: log.steps.get(n), waiting: isRunning(stepByNumber.get(n)) })),
        groups,
        visibleCache,
      ),
    // `version` tracks mutations of `log` / `groups`.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [order, expanded, stepByNumber, version, log, groups, visibleCache],
  );

  // ------------------------------------------------------------ virtual list
  const scrollRef = useRef<HTMLDivElement>(null);
  const getItemKey = useCallback((i: number) => rowKey(layout.rowAt(i)), [layout]);
  const estimateSize = useCallback((i: number) => (layout.rowAt(i).kind === 'step' ? STEP_H : LINE_H), [layout]);
  const virtualizer = useVirtualizer({
    count: layout.total,
    getScrollElement: () => scrollRef.current,
    estimateSize,
    getItemKey,
    overscan: 24,
  });

  // ------------------------------------------------------------ follow tail
  const [followPref, setFollowPref] = useState(true);
  const follow = live && followPref;
  const lastTop = useRef(0);
  const onScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const top = el.scrollTop;
    const atBottom = el.scrollHeight - top - el.clientHeight < LINE_H / 2;
    if (top < lastTop.current - 1 && !atBottom) setFollowPref(false);
    else if (top > lastTop.current && atBottom) setFollowPref(true);
    lastTop.current = top;
  };

  // ------------------------------------------------------------ search
  const searchInput = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState('');
  const [debounced, setDebounced] = useState('');
  useEffect(() => {
    const delay = query === '' ? 0 : log.lineCount > 50_000 ? 250 : log.lineCount > 5_000 ? 120 : 40;
    const t = setTimeout(() => setDebounced(query), delay);
    return () => clearTimeout(t);
  }, [query, log]);
  const needle = debounced.toLowerCase();
  const matches = useMemo(() => {
    search.update(log, needle);
    return search.result(log, order);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [search, log, needle, order, version]);
  const [cur, setCur] = useState(-1);
  const current = cur >= 0 ? matches.at(cur) : undefined;

  // ------------------------------------------------------------ reveal + scroll
  const pendingScroll = useRef<{ step: number; line: number } | null>(null);

  const reveal = useCallback(
    (step: number, line: number) => {
      const g = log.steps.get(step)?.lines[line]?.inGroup;
      if (g !== undefined) groups.set(step, g, true);
      setOverrides((o) => (o.get(step) === true ? o : new Map(o).set(step, true)));
      pendingScroll.current = { step, line };
      setFollowPref(false);
      bump();
    },
    [log, groups],
  );

  const goTo = useCallback(
    (i: number) => {
      const m = matches.at(i);
      if (!m) return;
      setCur(i);
      reveal(m.step, m.line);
    },
    [matches, reveal],
  );

  // New query: jump to the first match once results exist.
  const jumpPending = useRef(false);
  useEffect(() => {
    jumpPending.current = needle !== '';
    setCur(-1);
  }, [needle]);
  useEffect(() => {
    if (jumpPending.current && matches.total > 0) {
      jumpPending.current = false;
      goTo(0);
    }
  }, [matches, goTo]);

  const step = (dir: 1 | -1) => {
    if (!matches.total) return;
    goTo(cur < 0 ? (dir === 1 ? 0 : matches.total - 1) : (cur + dir + matches.total) % matches.total);
  };

  useLayoutEffect(() => {
    // Permalink from the URL: wait until its line has streamed in.
    const h = pendingHash.current;
    if (h) {
      const n = log.steps.get(h.step)?.lines.length ?? 0;
      if (n >= h.from) {
        pendingHash.current = null;
        reveal(h.step, h.from - 1);
        return;
      }
      if (phase === 'done' || phase === 'error') pendingHash.current = null;
    }
    const p = pendingScroll.current;
    if (p) {
      const idx = layout.indexOf(p.step, p.line);
      pendingScroll.current = null;
      if (idx >= 0) virtualizer.scrollToIndex(idx, { align: 'center' });
      return;
    }
    if (follow) {
      const el = scrollRef.current;
      if (el) {
        el.scrollTop = el.scrollHeight;
        lastTop.current = el.scrollTop;
      }
    }
  }, [layout, follow, phase, log, reveal, virtualizer]);

  // ------------------------------------------------------------ actions
  const toggleStep = useCallback(
    (n: number) => {
      setOverrides((o) => new Map(o).set(n, !expanded.has(n)));
    },
    [expanded],
  );
  const allExpanded = order.length > 0 && order.every((n) => expanded.has(n));
  const toggleAll = () => setOverrides(new Map(order.map((n) => [n, !allExpanded])));

  const toggleGroup = useCallback(
    (stepNum: number, group: number) => {
      groups.set(stepNum, group, !groups.isOpen(stepNum, group));
      bump();
    },
    [groups],
  );

  const onLineNumber = useCallback(
    (stepNum: number, n: number, e: MouseEvent) => {
      e.preventDefault();
      const range = e.shiftKey && sel && sel.step === stepNum;
      const next: Selection = range
        ? { step: stepNum, from: Math.min(sel.anchor, n), to: Math.max(sel.anchor, n), anchor: sel.anchor }
        : { step: stepNum, from: n, to: n, anchor: n };
      setSel(next);
      const hash = `#step:${stepNum}:${next.from}${next.to !== next.from ? `-${next.to}` : ''}`;
      history.replaceState(history.state, '', `${location.pathname}${location.search}${hash}`);
    },
    [sel],
  );

  const [timestamps, setTimestamps] = useState(readTimestampsPref);
  const toggleTimestamps = () => {
    const v = !timestamps;
    setTimestamps(v);
    try {
      localStorage.setItem(TS_KEY, v ? '1' : '0');
    } catch {
      // storage unavailable: keep the in-memory toggle
    }
  };

  const toggleFollow = () => {
    const v = !followPref;
    setFollowPref(v);
    if (v) {
      const el = scrollRef.current;
      if (el) el.scrollTop = el.scrollHeight;
    }
  };

  useShortcuts('Job log', {
    '/': {
      handler: () => {
        searchInput.current?.focus();
        searchInput.current?.select();
      },
      description: 'Search log',
      group: 'Logs',
    },
    'mod+f': {
      handler: () => {
        const input = searchInput.current;
        // Second press (already in the search box): let the browser's find open.
        if (!input || document.activeElement === input) return false;
        input.focus();
        input.select();
      },
      allowInInput: true,
      description: 'Search log',
      group: 'Logs',
    },
  });

  // ------------------------------------------------------------ render
  const queued = !log.lineCount && (job.status === 'queued' || job.status === 'waiting' || job.status === 'pending' || job.status === 'requested');
  const items = virtualizer.getVirtualItems();
  const scrollTop = virtualizer.scrollOffset ?? 0;

  // Sticky header of the step whose lines are at the top of the viewport.
  let stickyStep: number | undefined;
  const first = items.find((v) => v.end > scrollTop);
  if (first && (first.start < scrollTop || layout.rowAt(first.index).kind !== 'step')) {
    const s = layout.stepAt(first.index);
    if (s !== undefined && expanded.has(s)) stickyStep = s;
  }

  const widthPx = layout.maxWidth ? `calc(${layout.maxWidth + 12}ch + ${GUTTER_PX + (timestamps ? TS_PX : 0)}px)` : undefined;
  const innerStyle: CSSProperties = { height: virtualizer.getTotalSize(), minWidth: '100%', width: widthPx };

  const renderStepHeader = (n: number, sticky = false) => {
    const s = stepByNumber.get(n);
    const open = expanded.has(n);
    const lines = log.steps.get(n)?.lines.length ?? 0;
    return (
      <button
        type="button"
        className={cx(styles.stepHeader, open && styles.stepOpen, sticky && styles.stepSticky)}
        aria-expanded={open}
        onClick={() => {
          toggleStep(n);
          if (sticky) {
            const idx = layout.headerIndex(n);
            if (idx >= 0) virtualizer.scrollToIndex(idx, { align: 'start' });
          }
        }}
      >
        {open ? <ChevronDownIcon size={16} className={styles.chevron} /> : <ChevronRightIcon size={16} className={styles.chevron} />}
        <StatusIcon status={s?.status ?? 'completed'} conclusion={s?.conclusion ?? 'neutral'} size={14} />
        <span className={styles.stepName}>{s?.name ?? `Step ${n}`}</span>
        {open && lines > 0 && <span className={styles.stepCount}>{lines.toLocaleString()} lines</span>}
        {s && <Duration start={s.started_at} end={s.completed_at} running={isRunning(s)} />}
      </button>
    );
  };

  let body: ReactNode;
  if (phase === 'error' && !log.lineCount) {
    body = <div className={styles.message}>Logs for this job are not available.</div>;
  } else if (queued) {
    body = (
      <div className={styles.message}>
        <ClockIcon size={16} />
        This job is waiting for a runner…
      </div>
    );
  } else if (order.length === 0) {
    body =
      phase === 'done' ? (
        <div className={styles.message}>This job has no log output.</div>
      ) : (
        <div className={styles.message}>
          <Spinner size={16} />
          Loading log…
        </div>
      );
  }

  return (
    <section className={cx(styles.root, className)} aria-label={`Log of ${job.name}`}>
      <header className={styles.toolbar}>
        <div className={styles.title}>
          <StatusIcon status={job.status} conclusion={job.conclusion} />
          <h2 className={styles.jobName}>{job.name}</h2>
          <Duration start={job.started_at} end={job.completed_at} running={job.status === 'in_progress'} />
          {phase === 'connecting' && order.length > 0 && <Spinner size={14} label="Connecting" />}
        </div>
        <div className={styles.searchBox}>
          <Input
            ref={searchInput}
            size="sm"
            type="search"
            leadingIcon={SearchIcon}
            placeholder="Search logs"
            aria-label="Search logs"
            className={styles.search}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                step(e.shiftKey ? -1 : 1);
              } else if (e.key === 'Escape') {
                e.preventDefault();
                if (query) setQuery('');
                else e.currentTarget.blur();
              }
            }}
            trailing={
              query ? (
                <span className={styles.matchCount} aria-live="polite">
                  {matches.total ? `${cur >= 0 ? cur + 1 : 0} / ${matches.total.toLocaleString()}` : debounced === query ? 'No results' : ''}
                </span>
              ) : (
                <Kbd>/</Kbd>
              )
            }
          />
          <IconButton icon={ArrowUpIcon} label="Previous match" shortcut="⇧↵" size="sm" disabled={!matches.total} onClick={() => step(-1)} />
          <IconButton icon={ArrowDownIcon} label="Next match" shortcut="↵" size="sm" disabled={!matches.total} onClick={() => step(1)} />
        </div>
        <div className={styles.actions}>
          <IconButton
            icon={ClockIcon}
            label={timestamps ? 'Hide timestamps' : 'Show timestamps'}
            size="sm"
            aria-pressed={timestamps}
            className={cx(timestamps && styles.pressed)}
            onClick={toggleTimestamps}
          />
          <IconButton
            icon={allExpanded ? FoldIcon : UnfoldIcon}
            label={allExpanded ? 'Collapse all steps' : 'Expand all steps'}
            size="sm"
            disabled={order.length === 0}
            onClick={toggleAll}
          />
          <IconButton
            icon={ArrowDownIcon}
            label={follow ? 'Stop following log' : 'Follow log'}
            size="sm"
            aria-pressed={follow}
            disabled={!live}
            className={cx(follow && styles.pressed)}
            onClick={toggleFollow}
          />
          <Tooltip label="Download log">
            <a className={styles.download} href={jobLogUrl(owner, repo, job.id)} download aria-label="Download log">
              <DownloadIcon size={16} />
            </a>
          </Tooltip>
        </div>
      </header>

      {body ?? (
        <div className={styles.viewport}>
          {stickyStep !== undefined && <div className={styles.stickyWrap}>{renderStepHeader(stickyStep, true)}</div>}
          <div ref={scrollRef} className={styles.scroller} onScroll={onScroll}>
            <div className={styles.inner} style={innerStyle} role="log" aria-busy={phase === 'connecting'}>
              {items.map((v) => {
                const r = layout.rowAt(v.index);
                const style: CSSProperties = { transform: `translateY(${v.start}px)`, height: v.size };
                if (r.kind === 'step') {
                  return (
                    <div key={v.key} className={styles.row} style={style}>
                      {renderStepHeader(r.step)}
                    </div>
                  );
                }
                if (r.kind === 'waiting') {
                  return (
                    <div key={v.key} className={cx(styles.row, styles.waiting)} style={style}>
                      Waiting for output…
                    </div>
                  );
                }
                const line = log.steps.get(r.step)!.lines[r.line]!;
                const n = r.line + 1;
                return (
                  <LineRow
                    key={v.key}
                    line={line}
                    step={r.step}
                    n={n}
                    top={v.start}
                    timestamps={timestamps}
                    needle={needle}
                    current={current !== undefined && current.step === r.step && current.line === r.line}
                    selected={!!sel && sel.step === r.step && n >= sel.from && n <= sel.to}
                    groupOpen={line.groupId !== undefined && groups.isOpen(r.step, line.groupId)}
                    onNumber={onLineNumber}
                    onToggleGroup={toggleGroup}
                  />
                );
              })}
            </div>
          </div>
        </div>
      )}
    </section>
  );
}

export default JobLogView;

const PREFIX: Partial<Record<LogLine['kind'], string>> = { error: 'Error:', warning: 'Warning:', notice: 'Notice:' };

interface LineRowProps {
  line: LogLine;
  step: number;
  /** 1-based line number within the step. */
  n: number;
  top: number;
  timestamps: boolean;
  needle: string;
  current: boolean;
  selected: boolean;
  groupOpen: boolean;
  onNumber: (step: number, n: number, e: MouseEvent) => void;
  onToggleGroup: (step: number, group: number) => void;
}

const LineRow = memo(function LineRow({ line, step, n, top, timestamps, needle, current, selected, groupOpen, onNumber, onToggleGroup }: LineRowProps) {
  const spans = markMatches(spansOf(line), needle);
  const isGroup = line.groupId !== undefined;
  const prefix = PREFIX[line.kind];
  const content = (
    <>
      {prefix && <span className={styles.prefix}>{prefix} </span>}
      {spans.map((s, i) => (
        <span key={i} className={cx(s.bold && styles.bold, s.dim && styles.dim, s.italic && styles.italic, s.underline && styles.underline, s.hit && styles.hit)} style={s.fg || s.bg ? { color: s.fg, background: s.bg } : undefined}>
          {s.text}
        </span>
      ))}
    </>
  );
  return (
    <div
      className={cx(styles.row, styles.line, styles[line.kind], line.inGroup !== undefined && styles.inGroup, selected && styles.selected, current && styles.current)}
      style={{ transform: `translateY(${top}px)`, height: LINE_H }}
    >
      <a className={styles.num} href={`#step:${step}:${n}`} onClick={(e) => onNumber(step, n, e)} aria-label={`Line ${n}`}>
        {n}
      </a>
      {timestamps && (
        <span className={styles.ts} title={line.ts != null ? new Date(line.ts).toISOString() : undefined}>
          {line.ts != null ? formatTime(line.ts) : ''}
        </span>
      )}
      {isGroup ? (
        <button type="button" className={styles.groupToggle} aria-expanded={groupOpen} onClick={() => onToggleGroup(step, line.groupId!)}>
          {groupOpen ? <ChevronDownIcon size={14} /> : <ChevronRightIcon size={14} />}
          <span className={styles.text}>{content}</span>
        </button>
      ) : (
        <span className={styles.text}>{content}</span>
      )}
    </div>
  );
});

import { useMemo, useRef, useState, type KeyboardEvent, type RefObject } from 'react';
import { invalidate, load, prefetch, useResource } from '../../api/cache';
import { codeKeys, createBranch } from '../../api/code';
import { browseKeys, getRefs, isSha } from '../../api/endpoints';
import type { BrowseRefs } from '../../api/types';
import { Link } from '../../router';
import { Button, cx } from '../../ui/Button';
import { fuzzyScore } from '../../ui/fuzzy';
import { CheckIcon, ChevronDownIcon, GitBranchIcon, TagIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { Popover, type Placement } from '../../ui/Popover';
import { Spinner } from '../../ui/Spinner';
import { toast } from '../../ui/Toast';
import styles from './RefPicker.module.css';

export type RefKind = 'branch' | 'tag';

export interface RefPickerProps {
  owner: string;
  repo: string;
  /** Current ref (branch, tag or commit SHA). */
  value: string;
  onSelect: (ref: string, kind: RefKind) => void;
  /** Offer "Create branch: <query> from <value>" when nothing matches exactly (needs push access). */
  allowCreate?: boolean;
  /** Called after a branch was created from the picker (defaults to `onSelect`). */
  onCreated?: (branch: string) => void;
  /** Hide the Tags tab (e.g. base-branch pickers). */
  branchesOnly?: boolean;
  /** Text before the ref in the button, e.g. "base:". */
  prefix?: string;
  size?: 'sm' | 'md';
  placement?: Placement;
  /** Controlled open state (for keyboard shortcuts such as `w`). */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  className?: string;
}

export const refsKey = browseKeys.refs;

/** Load the branch + tag list (shared cache key with the code browser). */
export function useRefs(owner: string, repo: string, enabled = true) {
  return useResource<BrowseRefs>(enabled ? browseKeys.refs(owner, repo) : null, () => getRefs(owner, repo));
}

export function prefetchRefs(owner: string, repo: string): void {
  prefetch(browseKeys.refs(owner, repo), () => getRefs(owner, repo));
}

/** Short label for a ref (7-char SHA for commits). */
export function refLabel(ref: string): string {
  return isSha(ref) ? ref.slice(0, 7) : ref;
}

/**
 * Branch/tag switcher: fuzzy filter, Branches/Tags tabs, keyboard
 * navigation and optional "create branch" (POST git/refs) from the current ref.
 */
export function RefPicker(props: RefPickerProps) {
  const { owner, repo, value, prefix, size = 'sm', placement = 'bottom-start', className } = props;
  const anchor = useRef<HTMLButtonElement>(null);
  const [innerOpen, setInnerOpen] = useState(false);
  const open = props.open ?? innerOpen;
  const setOpen = (o: boolean) => {
    setInnerOpen(o);
    props.onOpenChange?.(o);
  };
  return (
    <>
      <Button
        ref={anchor}
        size={size}
        leadingIcon={GitBranchIcon}
        trailingIcon={ChevronDownIcon}
        className={cx(styles.button, className)}
        onClick={() => setOpen(!open)}
        onMouseEnter={() => prefetchRefs(owner, repo)}
        onFocus={() => prefetchRefs(owner, repo)}
        aria-expanded={open}
        aria-haspopup="dialog"
        data-testid="ref-picker"
      >
        {prefix && <span className={styles.prefix}>{prefix}</span>}
        <span className={styles.value}>{refLabel(value)}</span>
      </Button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={anchor} placement={placement} role="dialog" aria-label="Switch branches or tags">
        <RefPanel {...props} onClose={() => setOpen(false)} anchor={anchor} />
      </Popover>
    </>
  );
}

function RefPanel({
  owner,
  repo,
  value,
  onSelect,
  allowCreate,
  onCreated,
  branchesOnly,
  onClose,
}: RefPickerProps & { onClose: () => void; anchor: RefObject<HTMLButtonElement | null> }) {
  const refs = useRefs(owner, repo);
  const [tab, setTab] = useState<RefKind>('branch');
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const [creating, setCreating] = useState(false);
  const listRef = useRef<HTMLDivElement>(null);

  const items = useMemo(() => {
    const data = refs.data;
    if (!data) return [];
    const list = tab === 'branch' ? data.branches : data.tags;
    const q = query.trim();
    const scored = list.map((r) => ({ r, score: fuzzyScore(q, r.name) })).filter((x) => x.score > 0);
    if (q) scored.sort((a, b) => b.score - a.score);
    else if (tab === 'branch') scored.sort((a, b) => (a.r.name === data.default_branch ? -1 : b.r.name === data.default_branch ? 1 : 0));
    return scored.slice(0, 200).map((x) => x.r);
  }, [refs.data, tab, query]);

  const trimmed = query.trim();
  const exact = !!refs.data && [...refs.data.branches, ...refs.data.tags].some((r) => r.name === trimmed);
  const showCreate = !!allowCreate && tab === 'branch' && trimmed.length > 0 && !exact && /^[^\s~^:?*[\\]+$/.test(trimmed);
  const count = items.length + (showCreate ? 1 : 0);
  const cursor = Math.min(active, Math.max(0, count - 1));

  const choose = (i: number) => {
    if (i < items.length) {
      onClose();
      onSelect(items[i]!.name, tab);
    } else if (showCreate) void create();
  };

  const create = async () => {
    if (creating) return;
    setCreating(true);
    try {
      const data = refs.data ?? (await load(browseKeys.refs(owner, repo), () => getRefs(owner, repo)));
      const from = [...data.branches, ...data.tags].find((r) => r.name === value)?.sha ?? (isSha(value) ? value : null);
      if (!from) throw new Error(`Unknown ref ${value}`);
      await createBranch(owner, repo, trimmed, from);
      invalidate(browseKeys.refs(owner, repo));
      invalidate(codeKeys.branchList(owner, repo));
      toast({ title: `Created branch ${trimmed}` });
      onClose();
      (onCreated ?? ((b: string) => onSelect(b, 'branch')))(trimmed);
    } catch (e) {
      toast({ title: 'Could not create branch', description: e instanceof Error ? e.message : String(e), kind: 'error' });
    } finally {
      setCreating(false);
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) setActive((cursor + 1) % Math.max(1, count));
    else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) setActive((cursor - 1 + count) % Math.max(1, count));
    else if (e.key === 'Enter') choose(cursor);
    else if (e.key === 'Tab' && !branchesOnly) setTab((t) => (t === 'branch' ? 'tag' : 'branch'));
    else return;
    e.preventDefault();
    listRef.current?.querySelector(`[data-index="${(e.key === 'ArrowDown' ? cursor + 1 : e.key === 'ArrowUp' ? cursor - 1 : cursor) % Math.max(1, count)}"]`)?.scrollIntoView({ block: 'nearest' });
  };

  return (
    <div className={styles.panel} onKeyDown={onKeyDown}>
      <div className={styles.header}>Switch branches/tags</div>
      <div className={styles.filter}>
        <Input
          size="sm"
          autoFocus
          value={query}
          placeholder={tab === 'branch' ? (allowCreate ? 'Find or create a branch…' : 'Find a branch…') : 'Find a tag…'}
          onChange={(e) => {
            setQuery(e.target.value);
            setActive(0);
          }}
          aria-label="Filter refs"
        />
      </div>
      {!branchesOnly && (
        <div className={styles.tabs} role="tablist">
          {(['branch', 'tag'] as const).map((k) => (
            <button
              key={k}
              type="button"
              role="tab"
              aria-selected={tab === k}
              className={cx(styles.tab, tab === k && styles.tabActive)}
              onClick={() => {
                setTab(k);
                setActive(0);
              }}
            >
              {k === 'branch' ? 'Branches' : 'Tags'}
            </button>
          ))}
        </div>
      )}
      <div ref={listRef} className={styles.list} role="listbox" aria-label={tab === 'branch' ? 'Branches' : 'Tags'}>
        {!refs.data ? (
          <div className={styles.empty}>
            <Spinner size={14} /> Loading…
          </div>
        ) : (
          <>
            {items.map((r, i) => (
              <button
                key={r.name}
                type="button"
                role="option"
                aria-selected={r.name === value}
                data-index={i}
                data-active={i === cursor}
                className={styles.item}
                onPointerMove={() => setActive(i)}
                onClick={() => choose(i)}
              >
                <span className={styles.check}>{r.name === value && <CheckIcon size={16} />}</span>
                {tab === 'tag' && <TagIcon size={14} className={styles.muted} />}
                <span className={styles.name}>{r.name}</span>
                {tab === 'branch' && r.name === refs.data!.default_branch && <span className={styles.badge}>default</span>}
              </button>
            ))}
            {showCreate && (
              <button
                type="button"
                data-index={items.length}
                data-active={cursor === items.length}
                className={cx(styles.item, styles.create)}
                onPointerMove={() => setActive(items.length)}
                onClick={() => void create()}
                disabled={creating}
              >
                {creating ? <Spinner size={14} /> : <GitBranchIcon size={16} />}
                <span className={styles.name}>
                  Create branch <strong>{trimmed}</strong> from <strong>{refLabel(value)}</strong>
                </span>
              </button>
            )}
            {!items.length && !showCreate && <div className={styles.empty}>Nothing to show</div>}
          </>
        )}
      </div>
      {tab === 'branch' && refs.data && (
        <Link className={styles.footer} to={`/${owner}/${repo}/branches`} onClick={onClose}>
          View all branches
        </Link>
      )}
      {tab === 'tag' && refs.data && (
        <Link className={styles.footer} to={`/${owner}/${repo}/tags`} onClick={onClose}>
          View all tags
        </Link>
      )}
    </div>
  );
}

/** Helpers shared by the repository settings sections. */
import { useCallback, useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { invalidate, useResource } from '../../api/cache';
import { listBranchesAll, updateRepo } from '../../api/repoSettings';
import { Banner, ConfirmDialog, errorMessage } from '../../components/settings/kit';
import type { Repo } from '../../sync/models';
import { cx } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { XIcon } from '../../ui/icons';
import { Field, Select } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import styles from './RepoSettings.module.css';

export interface SectionProps {
  repo: Repo;
  /** Path segments after `/settings/<section>`. */
  rest: string[];
  /** `/{owner}/{repo}/settings` */
  base: string;
}

/** Cache key of a settings resource (ends with `/` so prefixes never collide across repos). */
export const repoKey = (repo: Repo, what: string) => `rs:${what}:${repo.id}/`;

/**
 * `useResource` plus a local, optimistic copy: `update(fn)` changes what is
 * shown immediately; the cache entry is dropped on unmount so the next visit
 * refetches the server's view.
 */
export function useLocalResource<T>(key: string | null, loader: () => Promise<T>) {
  const res = useResource<T>(key, loader);
  const [override, setOverride] = useState<{ key: string | null; value: T } | null>(null);
  const latest = useRef<T | undefined>(res.data);
  const mutated = useRef(false);
  const value = override && override.key === key ? override.value : res.data;
  useEffect(() => {
    latest.current = value;
  });
  useEffect(
    () => () => {
      if (mutated.current && key) invalidate(key);
    },
    [key],
  );
  const update = useCallback(
    (fn: (prev: T) => T) => {
      mutated.current = true;
      setOverride((prev) => {
        const cur = prev && prev.key === key ? prev.value : latest.current;
        if (cur === undefined) return prev;
        const next = fn(cur);
        latest.current = next;
        return { key, value: next };
      });
    },
    [key],
  );
  return { data: value, error: res.error, loading: res.loading && value === undefined, update };
}

export function LoadError({ error }: { error: unknown }) {
  return <Banner tone="danger">{errorMessage(error)}</Banner>;
}

export function ListSkeleton({ rows = 3 }: { rows?: number }) {
  return (
    <div className={styles.skeletonList} aria-busy="true" aria-label="Loading">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className={styles.skeletonRow}>
          <Skeleton width={20} height={20} />
          <Skeleton width={`${40 + ((i * 17) % 30)}%`} />
        </div>
      ))}
    </div>
  );
}

// ------------------------------------------------------------------ chips

/**
 * Token input: Enter / comma / Tab adds, Backspace on an empty field
 * removes the last chip. `validate` returns an error message or null.
 */
export function ChipInput({
  label,
  values,
  onChange,
  validate,
  normalize = (s) => s.trim(),
  placeholder,
  max,
  hint,
  disabled,
  suggestions,
  id: idProp,
}: {
  label: string;
  values: string[];
  onChange: (v: string[]) => void;
  validate?: (v: string, all: string[]) => string | null;
  normalize?: (s: string) => string;
  placeholder?: string;
  max?: number;
  hint?: ReactNode;
  disabled?: boolean;
  suggestions?: string[];
  id?: string;
}) {
  const genId = useId();
  const id = idProp ?? genId;
  const [text, setText] = useState('');
  const [error, setError] = useState<string | null>(null);
  const listId = `${id}-suggestions`;
  const add = (raw: string): boolean => {
    const parts = raw.split(',').map(normalize).filter(Boolean);
    if (!parts.length) return false;
    let next = values;
    for (const v of parts) {
      if (next.includes(v)) continue;
      if (max !== undefined && next.length >= max) {
        setError(`You can add at most ${max}.`);
        return false;
      }
      const err = validate?.(v, next) ?? null;
      if (err) {
        setError(err);
        return false;
      }
      next = [...next, v];
    }
    setError(null);
    setText('');
    if (next !== values) onChange(next);
    return true;
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter' || e.key === ',' || (e.key === 'Tab' && text.trim())) {
      if (!text.trim()) return; // Enter on an empty field submits the form
      e.preventDefault();
      add(text);
    } else if (e.key === 'Backspace' && !text && values.length) {
      onChange(values.slice(0, -1));
    }
  };
  return (
    <Field label={label} htmlFor={id} error={error} hint={hint}>
      <div className={cx(styles.chips, error && styles.chipsInvalid, disabled && styles.chipsDisabled)} onClick={(e) => e.currentTarget.querySelector('input')?.focus()}>
        {values.map((v) => (
          <span key={v} className={styles.chip}>
            {v}
            {!disabled && (
              <button type="button" className={styles.chipRemove} aria-label={`Remove ${v}`} onClick={() => onChange(values.filter((x) => x !== v))}>
                <XIcon size={12} />
              </button>
            )}
          </span>
        ))}
        <input
          id={id}
          className={styles.chipText}
          value={text}
          disabled={disabled}
          placeholder={values.length ? '' : placeholder}
          aria-invalid={!!error || undefined}
          list={suggestions?.length ? listId : undefined}
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => {
            const v = e.target.value;
            if (v.endsWith(',')) add(v.slice(0, -1));
            else {
              setText(v);
              if (error) setError(null);
            }
          }}
          onKeyDown={onKeyDown}
          onBlur={() => text.trim() && add(text)}
        />
        {suggestions?.length ? (
          <datalist id={listId}>
            {suggestions
              .filter((s) => !values.includes(s))
              .map((s) => (
                <option key={s} value={s} />
              ))}
          </datalist>
        ) : null}
      </div>
    </Field>
  );
}

// ------------------------------------------------------------------ default branch

export function useBranches(repo: Repo) {
  return useResource(repoKey(repo, 'branches'), () => listBranchesAll(repo.owner, repo.name));
}

/** "Switch default branch" dialog (General and Branches pages). */
export function DefaultBranchDialog({ repo, open, onClose }: { repo: Repo; open: boolean; onClose: () => void }) {
  const branches = useBranches(repo);
  const [value, setValue] = useState(repo.defaultBranch);
  const id = useId();
  useEffect(() => {
    if (open) setValue(repo.defaultBranch);
  }, [open, repo.defaultBranch]);
  return (
    <ConfirmDialog
      open={open}
      onClose={onClose}
      title="Switch default branch"
      confirmLabel="Update"
      danger={false}
      onConfirm={() => {
        if (value === repo.defaultBranch) return;
        // Optimistic: the store updates now; a rejection rolls back and toasts.
        const { done } = updateRepo(repo, { default_branch: value }, `Switch default branch to ${value}`);
        void done.then(() => toast({ kind: 'success', title: `Default branch is now ${value}` }), () => undefined);
      }}
    >
      <p className={styles.muted}>
        The default branch is considered the base branch in your repository, against which all pull requests and code commits are automatically made, unless you
        specify a different branch.
      </p>
      <Field label="Default branch" htmlFor={id}>
        {branches.data ? (
          <Select id={id} value={value} onChange={(e) => setValue(e.target.value)} autoFocus>
            {branches.data.map((b) => (
              <option key={b.name} value={b.name}>
                {b.name}
              </option>
            ))}
          </Select>
        ) : branches.error ? (
          <LoadError error={branches.error} />
        ) : (
          <Skeleton height={32} />
        )}
      </Field>
      {value !== repo.defaultBranch && (
        <Banner tone="warning">
          Changing your default branch can have unintended consequences that can affect new pull requests and clones.
        </Banner>
      )}
    </ConfirmDialog>
  );
}

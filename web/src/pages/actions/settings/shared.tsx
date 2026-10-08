import { useEffect, useRef, useState, type ReactNode, type RefObject } from 'react';
import type { SettingsScope } from '../../../api/actions';
import { refresh, useResource } from '../../../api/cache';
import { api, v3 } from '../../../api/client';
import { errorMessage, isAccessError } from '../../../api/errors';
import { Button } from '../../../ui/Button';
import { Dialog } from '../../../ui/Dialog';
import { EmptyState, Skeleton } from '../../../ui/EmptyState';
import { AlertIcon, LockIcon } from '../../../ui/icons';
import { Field, Input, Select } from '../../../ui/Input';
import { toast } from '../../../ui/Toast';
import styles from './Settings.module.css';

/** A cached REST resource: key + loader (shared by `useResource` and `refresh`). */
export interface Res<T> {
  key: string;
  load: () => Promise<T>;
}

export function scopeKey(s: SettingsScope): string {
  const k = s.kind === 'org' ? `org:${s.org}` : s.kind === 'repo' ? `repo:${s.owner}/${s.repo}` : `env:${s.owner}/${s.repo}:${s.env}`;
  return `actions-settings:${k.toLowerCase()}`;
}

export function useRes<T>(r: Res<T> | null) {
  return useResource<T>(r?.key ?? null, r?.load ?? (() => Promise.reject(new Error('no resource'))));
}

export const reload = <T,>(r: Res<T>): Promise<T> => refresh(r.key, r.load).catch(() => undefined as T);

export { errorMessage, isAccessError } from '../../../api/errors';

export function toastError(title: string, e: unknown): void {
  toast({ kind: 'error', title, description: errorMessage(e) });
}

/** Empty state for API failures (403/404 → no access, others → retry). */
export function ErrorState({ error, onRetry, what }: { error: unknown; onRetry?: () => void; what: string }) {
  if (isAccessError(error)) {
    return (
      <EmptyState icon={LockIcon} title={`You can't manage ${what} here`}>
        These settings are only available to administrators. Ask an owner for admin access.
      </EmptyState>
    );
  }
  return (
    <EmptyState
      icon={AlertIcon}
      title={`Couldn't load ${what}`}
      action={
        onRetry && (
          <Button size="sm" onClick={onRetry}>
            Retry
          </Button>
        )
      }
    >
      {errorMessage(error)}
    </EmptyState>
  );
}

export function ListSkeleton({ rows = 3 }: { rows?: number }) {
  return (
    <div className={styles.list} aria-busy="true">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className={styles.row}>
          <Skeleton width={16} height={16} />
          <Skeleton width={`${30 + ((i * 17) % 30)}%`} />
        </div>
      ))}
    </div>
  );
}

/** Focus `ref` once the surrounding dialog has opened (after showModal's own focus). */
export function useFocusOnOpen(ref: RefObject<HTMLElement | null>, select = false): void {
  useEffect(() => {
    const id = requestAnimationFrame(() => {
      const el = ref.current;
      if (!el) return;
      const input = el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement ? el : el.querySelector<HTMLElement>('input:not([disabled]),textarea:not([disabled]),button:not([disabled])');
      input?.focus();
      if (select && input instanceof HTMLInputElement) input.select();
    });
    return () => cancelAnimationFrame(id);
  }, [ref, select]);
}

/** Cmd/Ctrl+Enter in a textarea submits its form. */
export function submitOnModEnter(e: React.KeyboardEvent<HTMLTextAreaElement>): void {
  if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    e.currentTarget.form?.requestSubmit();
  }
}

export function ConfirmDialog({
  open,
  title,
  children,
  confirmLabel,
  onConfirm,
  onClose,
}: {
  open: boolean;
  title: string;
  children: ReactNode;
  confirmLabel: string;
  onConfirm: () => void;
  onClose: () => void;
}) {
  return (
    <Dialog open={open} onClose={onClose} title={title}>
      {open && <ConfirmBody confirmLabel={confirmLabel} onConfirm={onConfirm} onClose={onClose}>{children}</ConfirmBody>}
    </Dialog>
  );
}

function ConfirmBody({ children, confirmLabel, onConfirm, onClose }: { children: ReactNode; confirmLabel: string; onConfirm: () => void; onClose: () => void }) {
  const ref = useRef<HTMLButtonElement>(null);
  useFocusOnOpen(ref);
  return (
    <div className={styles.dialogForm}>
      <div className={styles.confirmText}>{children}</div>
      <div className={styles.dialogActions}>
        <Button onClick={onClose}>Cancel</Button>
        <Button
          ref={ref}
          variant="danger"
          onClick={() => {
            onConfirm();
            onClose();
          }}
        >
          {confirmLabel}
        </Button>
      </div>
    </div>
  );
}

/** Rows hidden optimistically (pending deletes); restored when the request fails. */
export function useHidden(): [Set<string>, (name: string, run: () => Promise<unknown>, onError: (e: unknown) => void) => void] {
  const [hidden, setHidden] = useState<Set<string>>(() => new Set());
  const hide = (name: string, run: () => Promise<unknown>, onError: (e: unknown) => void) => {
    setHidden((h) => new Set(h).add(name));
    run().then(
      () => setHidden((h) => withoutKey(h, name)),
      (e: unknown) => {
        setHidden((h) => withoutKey(h, name));
        onError(e);
      },
    );
  };
  return [hidden, hide];
}

function withoutKey(s: Set<string>, k: string): Set<string> {
  const n = new Set(s);
  n.delete(k);
  return n;
}

// ------------------------------------------------------------ org visibility

export type Visibility = 'all' | 'private' | 'selected';

export const VISIBILITY_LABEL: Record<Visibility, string> = {
  all: 'All repositories',
  private: 'Private repositories',
  selected: 'Selected repositories',
};

interface OrgRepo {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
}

const orgReposRes = (org: string): Res<OrgRepo[]> => ({
  key: `actions-settings:org:${org.toLowerCase()}:repos`,
  load: () => api.get<OrgRepo[]>(`${v3('orgs', org, 'repos')}?per_page=100`),
});

/** Repositories currently selected for an org secret / variable. */
export const selectedReposRes = (org: string, kind: 'secrets' | 'variables', name: string): Res<number[]> => ({
  key: `actions-settings:org:${org.toLowerCase()}:${kind}:${name.toUpperCase()}:repos`,
  load: () =>
    api
      .get<{ total_count: number; repositories: OrgRepo[] }>(`${v3('orgs', org, 'actions', kind, name, 'repositories')}?per_page=100`)
      .then((r) => r.repositories.map((x) => x.id)),
});

/**
 * Visibility select + repository picker for organization secrets/variables.
 * `selected` is null while the initial selection is unknown (loading).
 */
export function VisibilityFields({
  org,
  idPrefix,
  visibility,
  onVisibility,
  selected,
  onSelected,
}: {
  org: string;
  idPrefix: string;
  visibility: Visibility;
  onVisibility: (v: Visibility) => void;
  selected: Set<number> | null;
  onSelected: (s: Set<number>) => void;
}) {
  return (
    <>
      <Field label="Repository access" htmlFor={`${idPrefix}-vis`}>
        <Select id={`${idPrefix}-vis`} value={visibility} onChange={(e) => onVisibility(e.target.value as Visibility)}>
          <option value="all">All repositories</option>
          <option value="private">Private repositories</option>
          <option value="selected">Selected repositories</option>
        </Select>
      </Field>
      {visibility === 'selected' && <RepoPicker org={org} selected={selected} onSelected={onSelected} />}
    </>
  );
}

function RepoPicker({ org, selected, onSelected }: { org: string; selected: Set<number> | null; onSelected: (s: Set<number>) => void }) {
  const { data, error } = useRes(orgReposRes(org));
  const [filter, setFilter] = useState('');
  const q = filter.trim().toLowerCase();
  const repos = (data ?? []).filter((r) => !q || r.name.toLowerCase().includes(q));
  return (
    <fieldset className={styles.picker}>
      <legend className={styles.pickerLegend}>
        Selected repositories {selected && <span className={styles.muted}>({selected.size})</span>}
      </legend>
      <Input size="sm" placeholder="Filter repositories" value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter repositories" onKeyDown={(e) => e.key === 'Enter' && e.preventDefault()} />
      <div className={styles.pickerList}>
        {error ? (
          <div className={styles.muted}>Couldn't load repositories: {errorMessage(error)}</div>
        ) : !data || !selected ? (
          <Skeleton height={60} />
        ) : repos.length === 0 ? (
          <div className={styles.muted}>No repositories</div>
        ) : (
          repos.map((r) => (
            <label key={r.id} className={styles.pickerItem}>
              <input
                type="checkbox"
                checked={selected.has(r.id)}
                onChange={(e) => {
                  const n = new Set(selected);
                  if (e.target.checked) n.add(r.id);
                  else n.delete(r.id);
                  onSelected(n);
                }}
              />
              {r.private && <LockIcon size={12} className={styles.muted} />}
              {r.name}
            </label>
          ))
        )}
      </div>
    </fieldset>
  );
}

/** Initial repo selection state for a dialog: known for new items, loaded for existing "selected" ones. */
export function useInitialSelection(org: string | null, kind: 'secrets' | 'variables', name: string | null, visibility: Visibility | undefined) {
  const [override, setOverride] = useState<Set<number> | null>(null);
  const needsLoad = !!org && !!name && visibility === 'selected';
  const { data } = useRes(needsLoad ? selectedReposRes(org, kind, name) : null);
  const value = override ?? (needsLoad ? (data ? new Set(data) : null) : new Set<number>());
  return [value, setOverride] as const;
}

export function Section({ title, description, action, children }: { title: ReactNode; description?: ReactNode; action?: ReactNode; children: ReactNode }) {
  return (
    <section className={styles.section}>
      <div className={styles.sectionHeader}>
        <div>
          <h2 className={styles.sectionTitle}>{title}</h2>
          {description && <p className={styles.sectionDesc}>{description}</p>}
        </div>
        {action}
      </div>
      {children}
    </section>
  );
}

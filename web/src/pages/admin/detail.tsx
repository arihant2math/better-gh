/** Pieces shared by the admin user, organization and repository detail pages. */
import { useState, type ReactNode } from 'react';
import styles from '../../components/admin/admin.module.css';
import { Meter, type Severity } from '../../components/admin/charts';
import { formatKb } from '../../components/admin/format';
import { Panel, StatusPill, errorMessage } from '../../components/admin/kit';
import { Link, navigate } from '../../router';
import { Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import d from './AdminDetail.module.css';
import { deleteQuota, getQuota, setQuota, type MaintenanceOp, type Quota, type RepoBrief } from './api';

// ------------------------------------------------------------------ shared helpers (also used by the org / repo pages)

const enc = encodeURIComponent;

/** Same rule as the server (`bgh_accounts::validate::is_valid_login`). */
export const isValidLogin = (s: string) => /^[a-z\d](?:[a-z\d]|-(?=[a-z\d])){0,38}$/i.test(s);
export const LOGIN_RULE = 'Letters, digits and single hyphens (not at the start or end); up to 39 characters.';

/** `true` while any modal dialog is open: page shortcuts stand down. */
export const modalOpen = () => typeof document !== 'undefined' && !!document.querySelector('dialog[open]');

export { isNotFound } from '../../api/client';

export function NotFound({ what, name, back }: { what: string; name: string; back: { to: string; label: string } }) {
  return (
    <div className={styles.page}>
      <EmptyState icon={AlertIcon} title={`${what} not found`} action={<Button onClick={() => navigate(back.to)}>{back.label}</Button>}>
        There is no {what.toLowerCase()} named <strong>{name}</strong>. It may have been renamed or deleted.
      </EmptyState>
    </div>
  );
}

export function DetailSkeleton() {
  return (
    <div className={styles.page} aria-busy="true">
      <div className={styles.pageHeader}>
        <Skeleton width={48} height={48} />
        <div className={styles.pageHeaderText}>
          <Skeleton width={220} height={22} />
          <Skeleton width={320} style={{ marginTop: 8 }} />
        </div>
      </div>
      <div className={d.columns}>
        <div className={styles.stack}>
          {[0, 1, 2].map((i) => (
            <div key={i} className={styles.panel}>
              <div className={styles.panelBody}>
                <Skeleton width="40%" />
                <Skeleton style={{ marginTop: 10 }} />
                <Skeleton width="80%" style={{ marginTop: 10 }} />
              </div>
            </div>
          ))}
        </div>
        <div className={styles.stack}>
          <div className={styles.panel}>
            <div className={styles.panelBody}>
              <Skeleton width="50%" />
              <Skeleton style={{ marginTop: 10 }} />
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

/** Small list wrapper with an empty line. */
export function ItemList({ children, empty, scroll }: { children: ReactNode[]; empty: string; scroll?: boolean }) {
  if (children.length === 0) return <div className={d.emptyRow}>{empty}</div>;
  return (
    <div className={scroll ? d.scrollList : undefined}>
      <ul className={styles.list}>{children}</ul>
    </div>
  );
}

const VIS_STATUS = { public: 'neutral', private: 'warning', internal: 'info' } as const;

export function VisibilityPill({ visibility }: { visibility: keyof typeof VIS_STATUS }) {
  return <StatusPill status={VIS_STATUS[visibility] ?? 'neutral'}>{visibility[0]!.toUpperCase() + visibility.slice(1)}</StatusPill>;
}

/** Repositories owned by an account (links into the admin repo page). */
export function RepoBriefList({ owner, repos }: { owner: string; repos: RepoBrief[] }) {
  return (
    <ItemList empty="No repositories." scroll>
      {repos.map((r) => (
        <li key={r.id} className={styles.listItem}>
          <span className={styles.listMain}>
            <Link to={`/site-admin/repos/${enc(owner)}/${enc(r.name)}`} className={d.listLink}>
              {r.name}
            </Link>
          </span>
          <span className={d.rowMeta}>
            {r.archived && <StatusPill status="warning">Archived</StatusPill>}
            {r.fork && <Tag>Fork</Tag>}
            <VisibilityPill visibility={r.visibility} />
            <span title="Size on disk">{formatKb(r.size)}</span>
            <span title="Last push">{r.pushed_at ? <RelativeTime date={r.pushed_at} short /> : 'never pushed'}</span>
          </span>
        </li>
      ))}
    </ItemList>
  );
}

// ------------------------------------------------------------------ prompt dialog

export interface PromptOptions {
  title: string;
  label: string;
  initial?: string;
  body?: ReactNode;
  hint?: ReactNode;
  submitLabel: string;
  danger?: boolean;
  /** Message when the value is invalid, else null. */
  validate?: (v: string) => string | null;
  onSubmit: (value: string) => Promise<unknown>;
}

function PromptDialog({ open, onClose, options }: { open: boolean; onClose: () => void; options: PromptOptions | null }) {
  const [value, setValue] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shownFor, setShownFor] = useState<PromptOptions | null>(null);
  if (options !== shownFor) {
    setShownFor(options);
    setValue(options?.initial ?? '');
    setError(null);
    setBusy(false);
  }
  if (!options) return null;
  const trimmed = value.trim();
  const invalid = trimmed ? options.validate?.(trimmed) ?? null : null;
  const blocked = !trimmed || !!invalid || trimmed === (options.initial ?? '');
  const submit = async () => {
    if (blocked || busy) return;
    setBusy(true);
    setError(null);
    try {
      await options.onSubmit(trimmed);
      onClose();
    } catch (err) {
      setError(errorMessage(err));
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={options.title}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant={options.danger ? 'danger' : 'primary'} disabled={blocked} loading={busy} onClick={() => void submit()}>
            {options.submitLabel}
          </Button>
        </>
      }
    >
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        {options.body && <div className={styles.confirmBody}>{options.body}</div>}
        <Field label={options.label} htmlFor="prompt-value" error={invalid} hint={options.hint}>
          <Input id="prompt-value" value={value} onChange={(e) => setValue(e.target.value)} autoFocus autoComplete="off" spellCheck={false} invalid={!!invalid} />
        </Field>
        {error && (
          <div className={styles.formError} role="alert">
            <AlertIcon size={14} /> {error}
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}

/** `const prompt = usePrompt(); prompt({...})` + render `prompt.dialog`. */
export function usePrompt() {
  const [options, setOptions] = useState<PromptOptions | null>(null);
  const [open, setOpen] = useState(false);
  const ask = (o: PromptOptions) => {
    setOptions(o);
    setOpen(true);
  };
  return Object.assign(ask, { dialog: <PromptDialog open={open} onClose={() => setOpen(false)} options={options} /> });
}

/**
 * Uncontrolled text field rendered inside a confirm dialog's `extra`: it owns
 * its state and reports changes into a ref the confirm handler reads.
 */
export function RefField({ id, label, hint, into, placeholder }: { id: string; label: string; hint?: ReactNode; into: { current: string }; placeholder?: string }) {
  const [v, setV] = useState('');
  return (
    <Field label={label} htmlFor={id} hint={hint}>
      <Input
        id={id}
        value={v}
        placeholder={placeholder}
        autoComplete="off"
        spellCheck={false}
        onChange={(e) => {
          setV(e.target.value);
          into.current = e.target.value.trim();
        }}
      />
    </Field>
  );
}

// ------------------------------------------------------------------ quota

function severity(used: number, limit: number): Severity {
  const r = limit > 0 ? used / limit : 0;
  return r >= 0.9 ? 'critical' : r >= 0.75 ? 'warning' : 'ok';
}

const parseMb = (s: string): number | null | 'invalid' => {
  const t = s.trim();
  if (!t) return null;
  const n = Number(t);
  return Number.isInteger(n) && n > 0 ? n : 'invalid';
};

/** Storage usage against the effective limits + per-account override form. */
export function QuotaPanel({ login, quota, onChange }: { login: string; quota: Quota; onChange: (q: Quota) => void }) {
  const [repoMb, setRepoMb] = useState(quota.max_repo_size_mb?.toString() ?? '');
  const [totalMb, setTotalMb] = useState(quota.max_total_size_mb?.toString() ?? '');
  const [shownFor, setShownFor] = useState(quota);
  const [busy, setBusy] = useState<'save' | 'reset' | null>(null);
  const [error, setError] = useState<string | null>(null);
  if (shownFor !== quota) {
    setShownFor(quota);
    setRepoMb(quota.max_repo_size_mb?.toString() ?? '');
    setTotalMb(quota.max_total_size_mb?.toString() ?? '');
  }
  const repoVal = parseMb(repoMb);
  const totalVal = parseMb(totalMb);
  const dirty = repoVal !== quota.max_repo_size_mb || totalVal !== quota.max_total_size_mb;
  const invalid = repoVal === 'invalid' || totalVal === 'invalid';
  const hasOverride = quota.max_repo_size_mb !== null || quota.max_total_size_mb !== null;
  const totalLimitKb = quota.effective_max_total_size_mb != null ? quota.effective_max_total_size_mb * 1024 : null;
  const repoLimitKb = quota.effective_max_repo_size_mb != null ? quota.effective_max_repo_size_mb * 1024 : null;

  const save = async () => {
    if (invalid || !dirty) return;
    setBusy('save');
    setError(null);
    try {
      const q = await setQuota(login, { max_repo_size_mb: repoVal, max_total_size_mb: totalVal });
      onChange(q);
      toast({ kind: 'success', title: 'Storage quota saved' });
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(null);
    }
  };
  const reset = async () => {
    setBusy('reset');
    setError(null);
    try {
      await deleteQuota(login);
      onChange(await getQuota(login));
      toast({ kind: 'success', title: 'Storage quota reset to site defaults' });
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(null);
    }
  };

  return (
    <Panel title="Storage quota">
      <div className={d.quotaMeters}>
        {totalLimitKb ? (
          <Meter
            label="Total usage"
            value={quota.used_kb}
            max={totalLimitKb}
            severity={severity(quota.used_kb, totalLimitKb)}
            detail={`${formatKb(quota.used_kb)} of ${formatKb(totalLimitKb)}`}
          />
        ) : (
          <div className={d.usageLine}>
            <span>Total usage</span>
            <span>
              {formatKb(quota.used_kb)} <span className={styles.subtle}>· no limit</span>
            </span>
          </div>
        )}
        {repoLimitKb ? (
          <Meter
            label="Largest repository"
            value={quota.largest_repo_kb}
            max={repoLimitKb}
            severity={severity(quota.largest_repo_kb, repoLimitKb)}
            detail={`${formatKb(quota.largest_repo_kb)} of ${formatKb(repoLimitKb)} per repository`}
          />
        ) : (
          <div className={d.usageLine}>
            <span>Largest repository</span>
            <span>
              {formatKb(quota.largest_repo_kb)} <span className={styles.subtle}>· no limit</span>
            </span>
          </div>
        )}
      </div>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <div className={d.quotaForm}>
          <Field
            label="Per-repo limit (MB)"
            htmlFor={`q-repo-${login}`}
            error={repoVal === 'invalid' ? 'A positive whole number.' : null}
            hint="Empty: site default."
          >
            <Input
              id={`q-repo-${login}`}
              size="sm"
              inputMode="numeric"
              value={repoMb}
              placeholder={quota.effective_max_repo_size_mb != null && quota.max_repo_size_mb == null ? String(quota.effective_max_repo_size_mb) : 'No limit'}
              onChange={(e) => setRepoMb(e.target.value)}
              invalid={repoVal === 'invalid'}
            />
          </Field>
          <Field label="Total limit (MB)" htmlFor={`q-total-${login}`} error={totalVal === 'invalid' ? 'A positive whole number.' : null} hint="Empty: no limit.">
            <Input
              id={`q-total-${login}`}
              size="sm"
              inputMode="numeric"
              value={totalMb}
              placeholder="No limit"
              onChange={(e) => setTotalMb(e.target.value)}
              invalid={totalVal === 'invalid'}
            />
          </Field>
        </div>
        {error && (
          <div className={styles.formError} role="alert" style={{ marginTop: 10 }}>
            <AlertIcon size={14} /> {error}
          </div>
        )}
        <div className={d.quotaActions}>
          <Button size="sm" variant="ghost" disabled={!hasOverride} loading={busy === 'reset'} onClick={() => void reset()}>
            Reset to defaults
          </Button>
          <Button size="sm" variant="primary" type="submit" disabled={!dirty || invalid} loading={busy === 'save'}>
            Save quota
          </Button>
        </div>
      </form>
    </Panel>
  );
}


export const MAINTENANCE_OPS: { id: MaintenanceOp; label: string; description: string }[] = [
  {
    id: 'gc',
    label: 'Garbage collect',
    description: 'Fork-aware gc: pack objects; unreachable ones are pruned only after the grace period, and never in repositories forks borrow from.',
  },
  { id: 'repack', label: 'Repack', description: 'Rewrite all objects into a single pack, keeping unreachable ones.' },
  { id: 'fsck', label: 'Check integrity', description: 'git fsck: verify connectivity and validity of objects.' },
  { id: 'recalculate_size', label: 'Recalculate size', description: 'Measure the repository on disk and update its recorded size.' },
  { id: 'recalculate_languages', label: 'Recalculate languages', description: 'Re-detect the language breakdown of the default branch.' },
];

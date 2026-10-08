import { useEffect, useState } from 'react';
import { mutate, refresh, useResource } from '../../api/cache';
import { StatTile } from '../../components/admin/charts';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount, formatDateTime } from '../../components/admin/format';
import { PageHeader, Panel, StatusPill, Switch, attempt, errorMessage, type PillStatus } from '../../components/admin/kit';
import { usePagedList } from '../../api/usePagedList';
import { setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { PlayIcon, RepoForkedIcon, SyncIcon, ToolsIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import {
  GIT_MAINTENANCE_KEY,
  getGitMaintenance,
  gitMaintenanceReposPath,
  patchSettings,
  runGitMaintenanceNow,
  type GitMaintenanceOverview,
  type GitMaintenanceSettings,
  type GitMaintenanceState,
  type GitMaintenanceStatus,
} from '../../api/admin';
import g from './gitMaintenance.module.css';

const STATES: {
  id: Exclude<GitMaintenanceState, 'pending'>;
  label: string;
  pill: PillStatus;
}[] = [
  { id: 'succeeded', label: 'Succeeded', pill: 'ok' },
  { id: 'failed', label: 'Failed', pill: 'error' },
  { id: 'skipped', label: 'Skipped', pill: 'neutral' },
];

function StatePill({ state }: { state: GitMaintenanceState }) {
  const s = STATES.find((x) => x.id === state);
  return <StatusPill status={s?.pill ?? 'unknown'}>{s?.label ?? state}</StatusPill>;
}

/** Fork-network role badge: what maintenance may do to the repository. */
export function NetworkRole({ hasAlternates, hasDependents }: { hasAlternates: boolean; hasDependents: boolean }) {
  if (hasDependents)
    return (
      <span title="Other repositories borrow objects from this one: unreachable objects are kept, never pruned.">
        <StatusPill status="info">{hasAlternates ? 'Fork with forks' : 'Fork parent'}</StatusPill>
      </span>
    );
  if (hasAlternates)
    return (
      <span title="Borrows objects from its parent through alternates: local repacks only, no bitmaps.">
        <StatusPill status="neutral">Fork</StatusPill>
      </span>
    );
  return <span className={styles.subtle}>Standalone</span>;
}

const COLUMNS: Column<GitMaintenanceStatus>[] = [
  {
    id: 'repo',
    header: 'Repository',
    width: 'minmax(200px, 2fr)',
    render: (x) => (
      <span className={styles.cellMain}>
        <span className={styles.mono}>{x.full_name}</span>
        {x.error && (
          <span className={g.error} title={x.error}>
            {x.error.split('\n', 1)[0]}
          </span>
        )}
      </span>
    ),
  },
  {
    id: 'status',
    header: 'Status',
    width: '104px',
    render: (x) => <StatePill state={x.status} />,
  },
  {
    id: 'role',
    header: 'Network',
    width: '128px',
    hideBelow: 700,
    render: (x) => <NetworkRole hasAlternates={x.has_alternates} hasDependents={x.has_dependents} />,
  },
  {
    id: 'packs',
    header: 'Packs',
    width: '64px',
    align: 'end',
    hideBelow: 820,
    render: (x) => formatCount(x.pack_count),
  },
  {
    id: 'loose',
    header: 'Loose',
    width: '72px',
    align: 'end',
    hideBelow: 820,
    render: (x) => formatCount(x.loose_count),
  },
  {
    id: 'last',
    header: 'Last run',
    width: '104px',
    align: 'end',
    render: (x) => (x.last_run_at ? <RelativeTime date={x.last_run_at} /> : <span className={styles.subtle}>—</span>),
  },
  {
    id: 'full',
    header: 'Last full',
    width: '104px',
    align: 'end',
    hideBelow: 960,
    render: (x) =>
      x.last_full_at ? (
        <span title={formatDateTime(x.last_full_at)}>
          <RelativeTime date={x.last_full_at} />
        </span>
      ) : (
        <span className={styles.subtle}>—</span>
      ),
  },
];

type NumberField = Exclude<keyof GitMaintenanceSettings, 'enabled'>;

const FIELDS: { id: NumberField; label: string; hint: string; min: number }[] = [
  {
    id: 'prune_grace_days',
    label: 'Prune grace period (days)',
    hint: 'Unreachable objects younger than this are never deleted, so in-flight pushes stay safe.',
    min: 1,
  },
  {
    id: 'interval_hours',
    label: 'Incremental interval (hours)',
    hint: 'Geometric repack of a pushed repository at most this often.',
    min: 1,
  },
  {
    id: 'full_interval_days',
    label: 'Full repack every (days)',
    hint: 'Single-pack repack; fork parents keep unreachable objects.',
    min: 1,
  },
  {
    id: 'loose_objects_threshold',
    label: 'Loose objects threshold',
    hint: 'Repack early once this many loose objects pile up.',
    min: 1,
  },
  {
    id: 'pack_count_threshold',
    label: 'Pack count threshold',
    hint: 'Repack early once this many packs pile up.',
    min: 2,
  },
  {
    id: 'max_repos_per_pass',
    label: 'Repositories per pass',
    hint: 'Passes run once a minute.',
    min: 1,
  },
  {
    id: 'archive_cache_max_age_days',
    label: 'Archive cache max age (days)',
    hint: 'Cached source archives unused this long are removed.',
    min: 1,
  },
  {
    id: 'archive_cache_max_size_mb',
    label: 'Archive cache max size (MB)',
    hint: 'Oldest archives are removed beyond this size.',
    min: 0,
  },
];

type Draft = { enabled: boolean } & Record<NumberField, string>;

const toDraft = (s: GitMaintenanceSettings): Draft => ({
  enabled: s.enabled,
  ...(Object.fromEntries(FIELDS.map((f) => [f.id, String(s[f.id])])) as Record<NumberField, string>),
});

function fieldError(f: (typeof FIELDS)[number], v: string): string | null {
  if (!/^\d+$/.test(v.trim())) return 'Enter a whole number';
  return Number(v) < f.min ? `At least ${f.min}` : null;
}

function ScheduleForm({ settings, onSaved }: { settings: GitMaintenanceSettings; onSaved: (s: GitMaintenanceSettings) => void }) {
  const [draft, setDraft] = useState<Draft>(() => toDraft(settings));
  const [saving, setSaving] = useState(false);
  useEffect(() => setDraft(toDraft(settings)), [settings]);
  const errors = Object.fromEntries(FIELDS.map((f) => [f.id, fieldError(f, draft[f.id])])) as Record<NumberField, string | null>;
  const invalid = Object.values(errors).some(Boolean);
  const dirty = JSON.stringify(draft) !== JSON.stringify(toDraft(settings));
  const save = async () => {
    if (invalid || !dirty) return;
    setSaving(true);
    const patch: Partial<GitMaintenanceSettings> = { enabled: draft.enabled };
    for (const f of FIELDS) patch[f.id] = Number(draft[f.id]);
    await attempt(
      'Could not save the schedule',
      async () => {
        const s = await patchSettings({ git_maintenance: patch });
        onSaved(s.git_maintenance);
      },
      'Maintenance schedule saved',
    );
    setSaving(false);
  };
  return (
    <form
      className={g.form}
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <Switch
        checked={draft.enabled}
        onChange={(enabled) => setDraft((d) => ({ ...d, enabled }))}
        label="Scheduled maintenance"
        description="Commit-graphs, geometric and full repacks aware of fork networks, and archive-cache pruning."
      />
      <div className={g.grid}>
        {FIELDS.map((f) => (
          <Field key={f.id} label={f.label} htmlFor={`gm-${f.id}`} hint={f.hint} error={errors[f.id]}>
            <Input id={`gm-${f.id}`} inputMode="numeric" value={draft[f.id]} invalid={!!errors[f.id]} onChange={(e) => setDraft((d) => ({ ...d, [f.id]: e.target.value }))} />
          </Field>
        ))}
      </div>
      <div className={g.actions}>
        <Button type="button" disabled={!dirty || saving} onClick={() => setDraft(toDraft(settings))}>
          Reset
        </Button>
        <Button type="submit" variant="primary" disabled={!dirty || invalid} loading={saving}>
          Save schedule
        </Button>
      </div>
    </form>
  );
}

export default function GitMaintenancePage() {
  const params = useQuery();
  const stateParam = params.get('status') ?? '';
  const state = STATES.find((s) => s.id === stateParam)?.id ?? '';
  const overview = useResource(GIT_MAINTENANCE_KEY, getGitMaintenance, {
    ttlMs: 10_000,
  });
  const list = usePagedList<GitMaintenanceStatus>(gitMaintenanceReposPath(state || undefined));
  const [running, setRunning] = useState(false);

  const refreshAll = () => {
    void refresh(GIT_MAINTENANCE_KEY, getGitMaintenance).catch(() => undefined);
    void list.reload();
  };
  useShortcuts('Git maintenance', {
    r: {
      handler: refreshAll,
      description: 'Refresh maintenance status',
      group: 'Git maintenance',
    },
  });

  const runNow = async () => {
    setRunning(true);
    await attempt('Could not start maintenance', runGitMaintenanceNow, 'Maintenance pass queued');
    setRunning(false);
    // The pass runs as a background job; pick up its results shortly.
    setTimeout(refreshAll, 3000);
  };

  const o = overview.data;
  const count = (n: number | undefined) => (n == null ? '—' : formatCount(n));
  return (
    <div className={styles.fill}>
      <PageHeader
        title="Git maintenance"
        description="Scheduled, fork-safe repository housekeeping. Repositories that forks borrow objects from are never pruned."
        actions={
          <>
            <Button leadingIcon={SyncIcon} onClick={refreshAll} kbd="r">
              Refresh
            </Button>
            <Button leadingIcon={PlayIcon} variant="primary" loading={running} onClick={() => void runNow()}>
              Run now
            </Button>
          </>
        }
      />
      <div className={g.top}>
        <div className={g.tiles}>
          <StatTile label="Repositories" value={count(o?.repositories)} sub={o ? `${formatCount(o.never_run)} not yet maintained` : undefined} />
          <StatTile label="Failed" value={count(o?.failed)} sub={o?.failed ? <span className={g.failed}>needs attention</span> : undefined} />
          <StatTile label="Fork parents" value={count(o?.with_dependents)} sub="never pruned" />
          <StatTile label="Last pass" value={o?.last_run_at ? <RelativeTime date={o.last_run_at} /> : '—'} sub={o && !o.settings.enabled ? 'schedule disabled' : undefined} />
        </div>
        <Panel title="Schedule">
          {o ? (
            <ScheduleForm
              settings={o.settings}
              onSaved={(s) => mutate<GitMaintenanceOverview | undefined>(GIT_MAINTENANCE_KEY, (prev) => (prev ? { ...prev, settings: s } : prev))}
            />
          ) : overview.error ? (
            <span className={g.failed}>{errorMessage(overview.error)}</span>
          ) : (
            <span className={styles.subtle}>Loading…</span>
          )}
        </Panel>
      </div>
      <div className={styles.toolbar}>
        <Tabs
          size="sm"
          items={[
            { id: '', label: 'All' },
            ...STATES.map((s) => ({
              id: s.id,
              label: s.label,
              count: o ? formatCount(o[s.id]) : undefined,
            })),
          ]}
          value={state}
          onChange={(id) => setQuery({ status: id })}
        />
      </div>
      <DataTable
        aria-label="Repository maintenance status"
        rows={list.items}
        columns={COLUMNS}
        getKey={(x) => x.repository_id}
        href={(x) => `/site-admin/repos/${x.full_name}`}
        loading={list.loading && list.items.length === 0}
        hasMore={!!list.next}
        onEndReached={() => void list.loadMore()}
        rowHeight={48}
        empty={
          list.error ? (
            <EmptyState icon={ToolsIcon} title="Could not load maintenance status" action={<Button onClick={() => void list.reload()}>Try again</Button>}>
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={RepoForkedIcon} title={state ? 'No matching repositories' : 'No maintenance has run yet'}>
              {state ? 'Try another status.' : 'The scheduler picks up repositories within a minute of starting, or use Run now.'}
            </EmptyState>
          )
        }
      />
    </div>
  );
}

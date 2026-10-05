import { useEffect, useRef, useState } from 'react';
import { useResource } from '../../api/cache';
import type { RegistrationToken } from '../../api/actions';
import {
  addSiteGroupRunner,
  createSiteGroup,
  deleteAdminRunner,
  deleteSiteGroup,
  listAdminRunners,
  listAllOrgs,
  listQueue,
  listSiteGroupOrgs,
  listSiteGroupRunners,
  listSiteGroups,
  removeSiteGroupRunner,
  setSiteGroupOrgs,
  siteJitConfig,
  siteRegistrationToken,
  updateSiteGroup,
  type AdminRunner,
  type JitConfig,
  type QueuedJob,
  type RunnerGroup,
  type RunnerStatusFilter,
} from '../../api/runners';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount, plural } from '../../components/admin/format';
import { ErrorState, PageHeader, SearchInput, StatusPill, attempt, errorMessage, useConfirm } from '../../components/admin/kit';
import { Link, setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Tag } from '../../ui/Badge';
import { Button, IconButton } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import { GlobeIcon, LinkExternalIcon, OrganizationIcon, PencilIcon, PlusIcon, RepoIcon, ServerIcon, StackIcon, SyncIcon, TrashIcon, WorkflowIcon } from '../../ui/icons';
import { Field, Input, Select } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { Tooltip } from '../../ui/Tooltip';
import { AddRunnerRow, CommandBlock, GroupDialog, LabelChips, NameChips, RunnerStatus, osArch, runnerStyles as s, visibilityText, type GroupValues } from '../actions/settings/runnerKit';

type Tab = 'runners' | 'queue' | 'groups';
const STATUSES: { id: RunnerStatusFilter; label: string }[] = [
  { id: '', label: 'All' },
  { id: 'online', label: 'Online' },
  { id: 'offline', label: 'Offline' },
  { id: 'busy', label: 'Busy' },
];
const REFRESH_MS = 10_000;
const GROUPS_KEY = 'admin:runner-groups';

function ScopeCell({ r }: { r: AdminRunner }) {
  if (r.scope === 'site')
    return (
      <span className={s.scope}>
        <GlobeIcon size={14} /> Site
      </span>
    );
  if (r.scope === 'org')
    return (
      <span className={s.scope}>
        <OrganizationIcon size={14} />
        <Link to={`/organizations/${encodeURIComponent(r.owner ?? '')}/settings/actions/runners`} className={s.ellipsis}>
          {r.owner}
        </Link>
      </span>
    );
  return (
    <span className={s.scope}>
      <RepoIcon size={14} />
      <Link to={`/${r.repository ?? ''}/settings/actions/runners`} className={s.ellipsis}>
        {r.repository}
      </Link>
    </span>
  );
}

export default function RunnersPage() {
  const params = useQuery();
  const tabParam = params.get('tab');
  const tab: Tab = tabParam === 'queue' || tabParam === 'groups' ? tabParam : 'runners';
  const [adding, setAdding] = useState(false);
  const [version, setVersion] = useState(0);
  const bump = () => setVersion((v) => v + 1);
  const queue = useResource(`admin:runner-queue:${version}`, () => listQueue(), { ttlMs: REFRESH_MS });
  const groups = useResource(`${GROUPS_KEY}:${version}`, listSiteGroups);

  useShortcuts('Runners', {
    n: { handler: () => setAdding(true), description: 'New runner', group: 'Runners' },
    r: { handler: bump, description: 'Refresh', group: 'Runners' },
  });

  const queued = queue.data?.jobs.filter((j) => j.status === 'queued').length;
  return (
    <div className={styles.fill}>
      <PageHeader
        title="Runners"
        description="Self-hosted runners of every scope on this instance, the job queue and site-wide runner groups."
        actions={
          <>
            <Button leadingIcon={SyncIcon} onClick={bump} kbd="r">
              Refresh
            </Button>
            <Button leadingIcon={PlusIcon} variant="primary" onClick={() => setAdding(true)} kbd="n">
              New runner
            </Button>
          </>
        }
      />
      <div className={styles.toolbar}>
        <Tabs
          items={[
            { id: 'runners', label: 'Runners', icon: ServerIcon },
            { id: 'queue', label: 'Queue', icon: StackIcon, count: queued != null ? formatCount(queued) : undefined },
            { id: 'groups', label: 'Runner groups', icon: WorkflowIcon, count: groups.data ? formatCount(groups.data.length) : undefined },
          ]}
          value={tab}
          onChange={(id) => setQuery({ tab: id === 'runners' ? null : id })}
        />
      </div>
      {tab === 'runners' && <RunnersTab version={version} onChanged={bump} />}
      {tab === 'queue' && <QueueTab jobs={queue.data?.jobs} error={queue.error} onRetry={bump} />}
      {tab === 'groups' && <GroupsTab version={version} onChanged={bump} groups={groups.data} error={groups.error} />}
      <Dialog open={adding} onClose={() => setAdding(false)} title="New runner">
        {adding && <NewRunner groups={groups.data ?? []} onClose={() => setAdding(false)} onCreated={bump} />}
      </Dialog>
    </div>
  );
}

// ------------------------------------------------------------------ runners

function RunnersTab({ version, onChanged }: { version: number; onChanged: () => void }) {
  const params = useQuery();
  const statusParam = params.get('status') ?? '';
  const status = STATUSES.some((x) => x.id === statusParam) ? (statusParam as RunnerStatusFilter) : '';
  const q = params.get('q') ?? '';
  const key = `admin:runners:${status}:${q}:${version}`;
  const res = useResource(key, () => listAdminRunners({ status, q }), { ttlMs: REFRESH_MS });
  const [hidden, setHidden] = useState<Set<number>>(new Set());
  const confirm = useConfirm();
  const rows = (res.data?.runners ?? []).filter((r) => !hidden.has(r.id));

  // Status changes on its own: poll while visible.
  const reload = useRef(onChanged);
  useEffect(() => {
    reload.current = onChanged;
  });
  useEffect(() => {
    const t = setInterval(() => document.visibilityState === 'visible' && reload.current(), 30_000);
    return () => clearInterval(t);
  }, []);

  const remove = (r: AdminRunner) =>
    confirm({
      title: `Remove runner ${r.name}?`,
      body: (
        <>
          <strong>{r.name}</strong> stops receiving jobs and is unregistered from {r.scope === 'site' ? 'the site' : (r.repository ?? r.owner)}. Run{' '}
          <code className={styles.mono}>bgh-runner remove</code> on the machine to clean up its configuration.
        </>
      ),
      confirmLabel: 'Remove runner',
      danger: true,
      onConfirm: async () => {
        await deleteAdminRunner(r.id);
        setHidden((h) => new Set(h).add(r.id));
        toast({ kind: 'success', title: `Removed runner ${r.name}` });
        onChanged();
      },
    });

  const columns: Column<AdminRunner>[] = [
    {
      id: 'name',
      header: 'Runner',
      width: 'minmax(170px, 1.4fr)',
      render: (r) => (
        <span className={s.name}>
          <span className={s.ellipsis} title={r.name}>
            {r.name}
          </span>
          {r.builtin && <Tag>Built-in</Tag>}
          {r.ephemeral && <Tag>Ephemeral</Tag>}
        </span>
      ),
    },
    { id: 'scope', header: 'Scope', width: 'minmax(130px, 1fr)', render: (r) => <ScopeCell r={r} /> },
    { id: 'status', header: 'Status', width: '92px', render: (r) => <RunnerStatus runner={r} /> },
    { id: 'labels', header: 'Labels', width: 'minmax(180px, 2fr)', hideBelow: 760, render: (r) => <LabelChips labels={r.labels} /> },
    { id: 'os', header: 'OS / arch', width: '112px', hideBelow: 980, render: (r) => <span className={styles.subtle}>{osArch(r)}</span> },
    { id: 'group', header: 'Group', width: 'minmax(90px, 0.8fr)', hideBelow: 1100, render: (r) => <span className={`${styles.subtle} ${s.ellipsis}`}>{r.runner_group_name ?? '—'}</span> },
    {
      id: 'seen',
      header: 'Last seen',
      width: '96px',
      align: 'end',
      hideBelow: 880,
      render: (r) => (r.status === 'online' ? <span className={styles.subtle}>now</span> : r.last_seen_at ? <RelativeTime date={r.last_seen_at} /> : <span className={styles.subtle}>never</span>),
    },
    {
      id: 'actions',
      header: <span className="visually-hidden">Actions</span>,
      width: '44px',
      align: 'end',
      render: (r) =>
        r.builtin ? null : r.busy ? (
          <Tooltip label="Busy: wait for its job to finish before removing it">
            <span>
              <IconButton icon={TrashIcon} size="sm" label={`Remove runner ${r.name}`} tooltip={false} disabled />
            </span>
          </Tooltip>
        ) : (
          <IconButton icon={TrashIcon} size="sm" label={`Remove runner ${r.name}`} onClick={() => remove(r)} />
        ),
    },
  ];

  const filtered = !!(status || q);
  return (
    <>
      <div className={styles.toolbar}>
        <Tabs size="sm" items={STATUSES.map((x) => ({ id: x.id, label: x.label }))} value={status} onChange={(id) => setQuery({ status: id || null })} />
        <SearchInput label="Search runners" placeholder="Name, label, owner or group" value={q} onChange={(v) => setQuery({ q: v || null })} />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {res.data ? plural(rows.length, 'runner') : ''}
        </span>
      </div>
      <DataTable
        aria-label="Runners"
        rows={rows}
        columns={columns}
        getKey={(r) => r.id}
        loading={!res.data && !res.error}
        rowHeight={48}
        keyboard="Runners table"
        empty={
          res.error ? (
            <ErrorState error={res.error} onRetry={onChanged} title="Could not load runners" />
          ) : (
            <EmptyState icon={ServerIcon} title={filtered ? 'No matching runners' : 'No runners registered'}>
              {filtered ? 'Try another status or search.' : 'Register a machine with bgh-runner, or enable the built-in runner on the server.'}
            </EmptyState>
          )
        }
      />
      {confirm.dialog}
    </>
  );
}

// ------------------------------------------------------------------ queue

function waiting(j: QueuedJob) {
  return j.status === 'in_progress' && j.started_at ? (
    <span className={styles.subtle}>
      started <RelativeTime date={j.started_at} />
    </span>
  ) : (
    <span className={styles.subtle}>
      waiting since <RelativeTime date={j.created_at} />
    </span>
  );
}

const QUEUE_COLUMNS: Column<QueuedJob>[] = [
  {
    id: 'job',
    header: 'Job',
    width: 'minmax(180px, 1.4fr)',
    render: (j) => (
      <span className={s.name}>
        <span className={s.ellipsis} title={`${j.workflow_name} / ${j.name}`}>
          <span className={styles.subtle}>{j.workflow_name} / </span>
          {j.name}
        </span>
      </span>
    ),
  },
  {
    id: 'repo',
    header: 'Repository',
    width: 'minmax(130px, 1fr)',
    render: (j) => (
      <span className={s.scope}>
        <RepoIcon size={14} />
        <Link to={`/${j.repository}`} className={s.ellipsis}>
          {j.repository}
        </Link>
      </span>
    ),
  },
  { id: 'status', header: 'Status', width: '104px', render: (j) => (j.status === 'queued' ? <StatusPill status="warning">Queued</StatusPill> : <StatusPill status="info">In progress</StatusPill>) },
  { id: 'labels', header: 'Requested labels', width: 'minmax(180px, 1.6fr)', hideBelow: 760, render: (j) => <NameChips names={j.labels} /> },
  { id: 'time', header: 'Time', width: '190px', hideBelow: 900, render: waiting },
  { id: 'runner', header: 'Runner', width: 'minmax(100px, 0.8fr)', hideBelow: 1040, render: (j) => <span className={`${styles.subtle} ${s.ellipsis}`}>{j.runner_name ?? '—'}</span> },
  {
    id: 'open',
    header: <span className="visually-hidden">Open</span>,
    width: '44px',
    align: 'end',
    render: (j) => (
      <Link to={j.html_url} aria-label={`Open ${j.name} in ${j.repository}`} title="Open job">
        <LinkExternalIcon size={14} />
      </Link>
    ),
  },
];

function QueueTab({ jobs, error, onRetry }: { jobs: QueuedJob[] | undefined; error: unknown; onRetry: () => void }) {
  const rows = [...(jobs ?? [])].sort((a, b) => (a.status === b.status ? a.created_at.localeCompare(b.created_at) : a.status === 'in_progress' ? -1 : 1));
  return (
    <>
      <div className={styles.toolbar}>
        <span className={styles.meta}>
          {jobs ? `${plural(jobs.filter((j) => j.status === 'queued').length, 'queued job')} · ${jobs.filter((j) => j.status === 'in_progress').length} in progress` : ''}
        </span>
      </div>
      <DataTable
        aria-label="Job queue"
        rows={rows}
        columns={QUEUE_COLUMNS}
        getKey={(j) => j.id}
        href={(j) => j.html_url}
        loading={!jobs && !error}
        keyboard="Queue table"
        empty={
          error ? (
            <ErrorState error={error} onRetry={onRetry} title="Could not load the queue" />
          ) : (
            <EmptyState icon={StackIcon} title="No queued jobs">
              Jobs waiting for a self-hosted runner, and jobs running on one, show up here.
            </EmptyState>
          )
        }
      />
    </>
  );
}

// ------------------------------------------------------------------ new runner

function NewRunner({ groups, onClose, onCreated }: { groups: RunnerGroup[]; onClose: () => void; onCreated: () => void }) {
  const [mode, setMode] = useState<'token' | 'jit'>('token');
  return (
    <div className={s.stack}>
      <Tabs
        size="sm"
        items={[
          { id: 'token', label: 'Registration token' },
          { id: 'jit', label: 'Just-in-time config' },
        ]}
        value={mode}
        onChange={(id) => setMode(id as 'token' | 'jit')}
      />
      {mode === 'token' ? <TokenFlow /> : <JitFlow groups={groups} onCreated={onCreated} />}
      <div className={s.actions}>
        <Button variant="primary" onClick={onClose}>
          Done
        </Button>
      </div>
    </div>
  );
}

function TokenFlow() {
  const [state, setState] = useState<{ token?: RegistrationToken; error?: unknown }>({});
  const [labels, setLabels] = useState('');
  const started = useRef(false);
  const mint = () => {
    setState({});
    siteRegistrationToken().then(
      (token) => setState({ token }),
      (error: unknown) => setState({ error }),
    );
  };
  useEffect(() => {
    if (started.current) return;
    started.current = true;
    mint();
  }, []);
  if (state.error) return <ErrorState error={state.error} onRetry={mint} title="Could not create a registration token" />;
  if (!state.token)
    return (
      <p className={styles.subtle}>
        <Spinner /> Creating a registration token…
      </p>
    );
  const origin = window.location.origin;
  const list = labels
    .split(/[,\s]+/)
    .map((x) => x.trim())
    .filter(Boolean)
    .join(',');
  const register = `bgh-runner register --url ${origin} --token ${state.token.token}${list ? ` --labels ${list}` : ''}`;
  return (
    <>
      <CommandBlock
        label="Registration token"
        text={state.token.token}
        hint={
          <>
            Expires <RelativeTime date={state.token.expires_at} />. Registers a site-wide runner (Default group), available to every organization and repository.
          </>
        }
      />
      <Field label="Custom labels (optional)" htmlFor="runner-labels" hint="Comma-separated; added to the command below.">
        <Input id="runner-labels" value={labels} placeholder="gpu, big-disk" onChange={(e) => setLabels(e.target.value)} />
      </Field>
      <CommandBlock label="Register the runner" text={register} />
      <CommandBlock label="Start it" text="bgh-runner run" hint="Add --ephemeral to the register command for a runner that takes a single job and unregisters." />
    </>
  );
}

function JitFlow({ groups, onCreated }: { groups: RunnerGroup[]; onCreated: () => void }) {
  const [name, setName] = useState('');
  const [labels, setLabels] = useState('self-hosted, linux, x64');
  const [group, setGroup] = useState<number>(groups.find((g) => g.default)?.id ?? groups[0]?.id ?? 1);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<JitConfig | null>(null);
  if (result) {
    return (
      <>
        <p className={styles.subtle} style={{ margin: 0 }}>
          Created ephemeral runner <strong>{result.runner.name}</strong>. The config is shown once and registers exactly one runner that takes a single job.
        </p>
        <CommandBlock label="Start the runner" text={`bgh-runner run --jitconfig ${result.encoded_jit_config}`} />
        <Button size="sm" onClick={() => setResult(null)} style={{ alignSelf: 'flex-start' }}>
          Generate another
        </Button>
      </>
    );
  }
  const submit = async () => {
    const list = labels
      .split(/[,\s]+/)
      .map((x) => x.trim())
      .filter(Boolean);
    if (!name.trim()) return setError('Name is required.');
    if (!list.length) return setError('Add at least one label.');
    setBusy(true);
    setError(null);
    try {
      setResult(await siteJitConfig({ name: name.trim(), labels: list, runner_group_id: group }));
      onCreated();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className={s.stack}
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <Field label="Runner name" htmlFor="jit-name">
        <Input id="jit-name" value={name} placeholder="ci-ephemeral-01" onChange={(e) => setName(e.target.value)} autoFocus />
      </Field>
      <Field label="Labels" htmlFor="jit-labels" hint="Comma-separated. Jobs whose runs-on lists only these labels can run on it.">
        <Input id="jit-labels" value={labels} onChange={(e) => setLabels(e.target.value)} />
      </Field>
      <Field label="Runner group" htmlFor="jit-group">
        <Select id="jit-group" value={group} onChange={(e) => setGroup(Number(e.target.value))}>
          {groups.map((g) => (
            <option key={g.id} value={g.id}>
              {g.name}
            </option>
          ))}
        </Select>
      </Field>
      {error && (
        <div className={s.formError} role="alert">
          {error}
        </div>
      )}
      <Button type="submit" variant="primary" loading={busy} style={{ alignSelf: 'flex-start' }}>
        Generate JIT config
      </Button>
    </form>
  );
}

// ------------------------------------------------------------------ groups

function GroupsTab({ version, onChanged, groups, error }: { version: number; onChanged: () => void; groups: RunnerGroup[] | undefined; error: unknown }) {
  const [editing, setEditing] = useState<RunnerGroup | 'new' | null>(null);
  const orgs = useResource(editing ? 'admin:runner-orgs' : null, listAllOrgs);
  const editingGroup = editing && editing !== 'new' ? editing : null;
  const selected = useResource(editingGroup?.visibility === 'selected' ? `admin:runner-group:${editingGroup.id}:orgs:${version}` : null, () => listSiteGroupOrgs(editingGroup!.id));
  const runners = useResource(`admin:runners:::${version}`, () => listAdminRunners({}));
  const confirm = useConfirm();

  const save = async (v: GroupValues) => {
    const body = { name: v.name, visibility: v.visibility, allows_public_repositories: v.allows_public_repositories, restricted_to_workflows: v.restricted_to_workflows, selected_workflows: v.selected_workflows };
    if (editingGroup) {
      await updateSiteGroup(editingGroup.id, editingGroup.default ? { ...body, name: undefined } : body);
      if (v.visibility === 'selected') await setSiteGroupOrgs(editingGroup.id, v.selected);
      toast({ kind: 'success', title: `Saved ${v.name}` });
    } else {
      await createSiteGroup({ ...body, selected_organization_ids: v.visibility === 'selected' ? v.selected : undefined });
      toast({ kind: 'success', title: `Created runner group ${v.name}` });
    }
    onChanged();
  };

  const remove = (g: RunnerGroup) =>
    confirm({
      title: `Delete runner group ${g.name}?`,
      body: 'Its runners move to the Default group. Organizations lose access granted through this group.',
      confirmLabel: 'Delete group',
      danger: true,
      onConfirm: async () => {
        await deleteSiteGroup(g.id);
        toast({ kind: 'success', title: `Deleted ${g.name}` });
        onChanged();
      },
    });

  const siteRunners = (runners.data?.runners ?? []).filter((r) => r.scope === 'site');
  return (
    <div className={`${s.tabBody} ${s.scroll}`}>
      <div className={styles.toolbar} style={{ padding: '0 0 12px' }}>
        <span className={styles.subtle}>Site groups control which organizations may use site-wide runners. Organization groups are managed in each organization’s settings.</span>
        <span className={styles.toolbarSpacer} />
        <Button size="sm" leadingIcon={PlusIcon} onClick={() => setEditing('new')}>
          New group
        </Button>
      </div>
      {error ? (
        <ErrorState error={error} onRetry={onChanged} title="Could not load runner groups" />
      ) : !groups ? (
        <p className={styles.subtle}>Loading…</p>
      ) : (
        <div className={s.cardList}>
          {groups.map((g) => (
            <SiteGroupCard
              key={g.id}
              group={g}
              version={version}
              siteRunners={siteRunners}
              onEdit={() => setEditing(g)}
              onDelete={() => remove(g)}
              onChanged={onChanged}
            />
          ))}
        </div>
      )}
      <GroupDialog
        open={!!editing}
        onClose={() => setEditing(null)}
        kind="site"
        group={editingGroup}
        targets={orgs.data?.map((o) => ({ id: o.id, label: o.login }))}
        targetsError={orgs.error}
        initialSelected={editingGroup?.visibility === 'selected' ? (selected.data?.map((o) => o.id) ?? null) : []}
        onSubmit={save}
      />
      {confirm.dialog}
    </div>
  );
}

function SiteGroupCard({
  group: g,
  version,
  siteRunners,
  onEdit,
  onDelete,
  onChanged,
}: {
  group: RunnerGroup;
  version: number;
  siteRunners: AdminRunner[];
  onEdit: () => void;
  onDelete: () => void;
  onChanged: () => void;
}) {
  const members = useResource(`admin:runner-group:${g.id}:runners:${version}`, () => listSiteGroupRunners(g.id));
  const orgs = useResource(g.visibility === 'selected' ? `admin:runner-group:${g.id}:orgs:${version}` : null, () => listSiteGroupOrgs(g.id));
  const inGroup = new Set((members.data ?? []).map((r) => r.id));
  const candidates = siteRunners.filter((r) => !inGroup.has(r.id)).map((r) => ({ ...r, groupName: r.runner_group_name }));
  return (
    <section className={s.groupCard} aria-label={`Runner group ${g.name}`}>
      <div className={s.groupHeader}>
        <h3 className={s.groupTitle}>
          {g.name} {g.default && <Tag>Default</Tag>}
        </h3>
        <IconButton icon={PencilIcon} size="sm" label={`Edit ${g.name}`} onClick={onEdit} />
        {!g.default && <IconButton icon={TrashIcon} size="sm" label={`Delete ${g.name}`} onClick={onDelete} />}
      </div>
      <div className={s.groupMeta}>
        <span>
          <OrganizationIcon size={14} />
          {visibilityText(g, 'site')}
          {g.visibility === 'selected' && orgs.data && `: ${orgs.data.map((o) => o.login).join(', ') || 'none'}`}
        </span>
        <span>{g.allows_public_repositories ? 'Public repositories allowed' : 'Private repositories only'}</span>
        <span title={g.selected_workflows.join('\n')}>{g.restricted_to_workflows ? `Restricted to ${plural(g.selected_workflows.length, 'workflow')}` : 'Any workflow'}</span>
      </div>
      {members.error ? (
        <div className={s.empty}>Couldn’t load runners: {errorMessage(members.error)}</div>
      ) : !members.data ? (
        <div className={s.empty}>Loading runners…</div>
      ) : members.data.length === 0 ? (
        <div className={s.empty}>No runners in this group.</div>
      ) : (
        members.data.map((r) => (
          <div key={r.id} className={s.memberRow}>
            <div className={s.memberMain}>
              <span className={s.name}>{r.name}</span>
              <RunnerStatus runner={r} />
              <span className={styles.subtle}>{osArch(r)}</span>
              <LabelChips labels={r.labels} />
            </div>
            {!g.default && (
              <Button
                size="sm"
                variant="ghost"
                onClick={() => void attempt(`Couldn’t move ${r.name}`, () => removeSiteGroupRunner(g.id, r.id), `Moved ${r.name} to the Default group`).then((ok) => ok && onChanged())}
              >
                Remove
              </Button>
            )}
          </div>
        ))
      )}
      {members.data && (
        <AddRunnerRow
          candidates={candidates}
          groupName={g.name}
          onAdd={async (id) => {
            const r = siteRunners.find((x) => x.id === id);
            if (await attempt('Couldn’t move the runner', () => addSiteGroupRunner(g.id, id), `Moved ${r?.name ?? 'runner'} to ${g.name}`)) onChanged();
          }}
        />
      )}
    </section>
  );
}

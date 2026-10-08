import { observer } from 'mobx-react-lite';
import { useEffect, useMemo, useRef, useState } from 'react';
import { setWorkflowEnabled, type RunFilters, type Workflow, type WorkflowRun } from '../../api/actions';
import { load, refresh, useResource } from '../../api/cache';
import { listBranches } from '../../api/endpoints';
import { useCommands } from '../../app/commands';
import { Link, navigate, setQuery, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import { Button, IconButton, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { ChevronDownIcon, KebabHorizontalIcon, PlayIcon, WorkflowIcon, XIcon } from '../../ui/icons';
import { Menu, SelectPanel, type MenuEntry, type SelectItem } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import { VirtualList } from '../../ui/VirtualList';
import { loadRuns, loadWorkflows, runsKey, workflowsKey } from './data';
import { BadgeDialog } from './BadgeDialog';
import { DispatchButton } from './DispatchPanel';
import { runs as liveRuns, useOnNewRun, usePolling } from './live';
import { RunRow } from './RunRow';
import { EVENTS, STATUS_FILTERS, workflowFile } from './shared';
import { WorkflowsSidebar } from './WorkflowsSidebar';
import styles from './Runs.module.css';
import { canPush } from '../../sync/selectors';
import { useRouteRepo } from '../repo/useRouteRepo';

const PER_PAGE = 50;

type FilterName = 'event' | 'status' | 'branch' | 'actor';

export default observer(function RunsPage() {
  const { owner, repo: name, workflow: wfParam } = useParams<{ owner: string; repo: string; workflow?: string }>();
  const query = useQuery();
  const repo = useRouteRepo();
  const base = `/${owner}/${name}`;
  const canAdmin = store().get('viewerRepo', repo.id)?.permission === 'admin';
  const canWrite = canPush(repo.id);

  const workflows = useResource(workflowsKey(owner, name), loadWorkflows(owner, name));
  const workflow: Workflow | undefined = wfParam ? workflows.data?.find((w) => workflowFile(w.path) === wfParam || String(w.id) === wfParam) : undefined;
  const wfMissing = !!wfParam && !!workflows.data && !workflow;

  const filters: RunFilters = {
    workflowId: workflow?.id,
    event: query.get('event') ?? undefined,
    status: query.get('status') ?? undefined,
    branch: query.get('branch') ?? undefined,
    actor: query.get('actor') ?? undefined,
    perPage: PER_PAGE,
  };
  const ready = !wfParam || !!workflow;
  const firstKey = ready ? runsKey(owner, name, { ...filters, page: 1 }) : null;
  const first = useResource(firstKey, loadRuns(owner, name, { ...filters, page: 1 }));

  // Further pages (infinite scroll), keyed by the filter set.
  const filterId = firstKey ?? '';
  const [more, setMore] = useState<{ id: string; pages: WorkflowRun[][]; loading: boolean }>({ id: '', pages: [], loading: false });
  const extra = more.id === filterId ? more : { id: filterId, pages: [], loading: false };
  const items = useMemo(() => {
    const seen = new Set<number>();
    const out: WorkflowRun[] = [];
    for (const r of [...(first.data?.workflow_runs ?? []), ...extra.pages.flat()]) {
      if (!seen.has(r.id)) {
        seen.add(r.id);
        out.push(r);
      }
    }
    return out;
  }, [first.data, extra.pages]);
  const total = first.data?.total_count ?? 0;
  const hasMore = items.length < total;

  const pending = useRef<string | null>(null);
  const loadMore = () => {
    if (extra.loading || !hasMore || !firstKey) return;
    const page = extra.pages.length + 2;
    const token = `${filterId}#${page}`;
    if (pending.current === token) return;
    pending.current = token;
    setMore({ ...extra, loading: true });
    const f = { ...filters, page };
    load(runsKey(owner, name, f), loadRuns(owner, name, f)).then(
      (res) => setMore((m) => (m.id === filterId ? { id: filterId, pages: [...m.pages, res.workflow_runs], loading: false } : m)),
      () => setMore((m) => ({ ...m, loading: false })),
    );
  };

  // Live: a run we haven't seen → refetch page 1 once (debounced).
  const refetchFirst = () => firstKey && void refresh(firstKey, loadRuns(owner, name, { ...filters, page: 1 })).catch(() => undefined);
  useOnNewRun(repo.id, refetchFirst);
  const anyRunning = items.some((r) => (liveRuns.get(r.id) ?? r).status !== 'completed');
  usePolling(anyRunning, refetchFirst);

  // Keyboard cursor.
  const [cursor, setCursor] = useState(0);
  const cur = Math.min(cursor, Math.max(0, items.length - 1));
  useShortcuts('Workflow runs', {
    j: { handler: () => setCursor(Math.min(items.length - 1, cur + 1)), description: 'Next run', group: 'Lists' },
    k: { handler: () => setCursor(Math.max(0, cur - 1)), description: 'Previous run', group: 'Lists' },
    arrowdown: { handler: () => setCursor(Math.min(items.length - 1, cur + 1)), hidden: true },
    arrowup: { handler: () => setCursor(Math.max(0, cur - 1)), hidden: true },
    enter: { handler: () => (items[cur] ? navigate(`${base}/actions/runs/${items[cur].id}`) : false), description: 'Open run', group: 'Lists' },
    o: { handler: () => (items[cur] ? navigate(`${base}/actions/runs/${items[cur].id}`) : false), hidden: true },
  });
  useEffect(() => {
    if (hasMore && cur >= items.length - 5) loadMore();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- page in as the cursor nears the end
  }, [cur, items.length, hasMore]);

  const wfName = (id: number) => workflows.data?.find((w) => w.id === id)?.name ?? '';
  const activeFilters = (['event', 'status', 'branch', 'actor'] as const).filter((f) => query.get(f));

  useCommands(
    [
      { id: 'actions.all', title: 'Actions: all workflows', group: 'Actions', run: () => navigate(`${base}/actions`) },
      ...(workflows.data ?? []).map((w) => ({
        id: `actions.wf.${w.id}`,
        title: `Actions: ${w.name}`,
        group: 'Actions',
        run: () => navigate(`${base}/actions/workflows/${encodeURIComponent(workflowFile(w.path))}`),
      })),
    ],
    [base, workflows.data],
  );

  return (
    <div className={styles.page}>
      <WorkflowsSidebar base={base} workflows={workflows.data} current={workflow ? workflowFile(workflow.path) : null} canAdmin={canAdmin} />
      <section className={styles.main}>
        <header className={styles.header}>
          <div className={styles.headTitle}>
            <h2 className={styles.h2}>{workflow ? workflow.name : wfParam ? wfParam : 'All workflows'}</h2>
            {workflow && (
              <Link className={styles.path} to={new URL(workflow.html_url, window.location.origin).pathname}>
                {workflow.path}
              </Link>
            )}
            {!workflow && <span className={styles.subtle}>Showing runs from all workflows</span>}
          </div>
          <div className={styles.headActions}>
            {workflow && canWrite && <DispatchButton owner={owner} repo={name} workflow={workflow} defaultBranch={repo.defaultBranch} />}
            {workflow && <WorkflowMenu owner={owner} repo={name} workflow={workflow} defaultBranch={repo.defaultBranch} canAdmin={canAdmin} />}
          </div>
        </header>
        {workflow && workflow.state !== 'active' && (
          <div className={styles.banner}>This workflow was disabled manually. Enable it from the menu to run it again.</div>
        )}
        <div className={styles.filterBar}>
          <span className={styles.count}>{first.data ? `${total.toLocaleString()} workflow run${total === 1 ? '' : 's'}` : ' '}</span>
          <div className={styles.filters}>
            <FilterButton name="event" label="Event" items={EVENTS.map((e) => ({ id: e, text: e }))} />
            <FilterButton name="status" label="Status" items={STATUS_FILTERS.map((s) => ({ id: s, text: s.replace('_', ' ') }))} />
            <BranchFilter owner={owner} repo={name} />
            <ActorFilter runs={items} />
            {activeFilters.length > 0 && (
              <Button size="sm" variant="ghost" leadingIcon={XIcon} onClick={() => setQuery({ event: null, status: null, branch: null, actor: null })}>
                Clear
              </Button>
            )}
          </div>
        </div>
        {wfMissing ? (
          <EmptyState icon={WorkflowIcon} title="Workflow not found">
            No workflow named {wfParam} exists in this repository.
          </EmptyState>
        ) : !first.data && (first.loading || !ready) ? (
          <div className={styles.skeletons}>
            {Array.from({ length: 6 }, (_, i) => (
              <div key={i} className={styles.skeletonRow}>
                <Skeleton width={16} height={16} />
                <div style={{ flex: 1 }}>
                  <Skeleton width="45%" height={14} />
                  <Skeleton width="30%" height={11} style={{ marginTop: 6 }} />
                </div>
              </div>
            ))}
          </div>
        ) : first.error && !first.data ? (
          <EmptyState icon={PlayIcon} title="Could not load workflow runs">
            {(first.error as Error).message}
          </EmptyState>
        ) : items.length === 0 ? (
          <EmptyState icon={PlayIcon} title={activeFilters.length ? 'No runs match these filters' : 'No workflow runs yet'}>
            {activeFilters.length
              ? 'Try removing some filters.'
              : workflows.data?.length
                ? 'Runs appear here as soon as a workflow is triggered.'
                : 'Add a YAML file under .github/workflows/ and push it to get started.'}
          </EmptyState>
        ) : (
          <VirtualList
            className={styles.list}
            items={items}
            estimateSize={61}
            activeIndex={cur}
            getKey={(r) => r.id}
            aria-label="Workflow runs"
            renderItem={(run, index) => {
              if (index >= items.length - 10 && hasMore) queueMicrotask(loadMore);
              return (
                <RunRow run={run} base={base} active={index === cur} workflowName={wfName(run.workflow_id) || run.name} showWorkflow={!workflow} onActivate={() => setCursor(index)} />
              );
            }}
          />
        )}
      </section>
    </div>
  );
});

function FilterButton({ name, label, items }: { name: FilterName; label: string; items: { id: string; text: string }[] }) {
  const query = useQuery();
  const value = query.get(name);
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const list: SelectItem[] = items.map((i) => ({ ...i, selected: i.id === value }));
  return (
    <>
      <Button ref={ref} size="sm" variant="ghost" trailingIcon={ChevronDownIcon} aria-expanded={open} className={cx(value && styles.filterOn)} onClick={() => setOpen(true)}>
        {value ? `${label}: ${value}` : label}
      </Button>
      <SelectPanel
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        title={`Filter by ${label.toLowerCase()}`}
        items={list}
        multiple={false}
        onToggle={(id) => {
          setQuery({ [name]: id === value ? null : String(id) });
          setOpen(false);
        }}
      />
    </>
  );
}

function BranchFilter({ owner, repo }: { owner: string; repo: string }) {
  const branches = useResource(`branches:${owner}/${repo}`, () => listBranches(owner, repo));
  return <FilterButton name="branch" label="Branch" items={(branches.data ?? []).map((b) => ({ id: b.name, text: b.name }))} />;
}

function ActorFilter({ runs }: { runs: WorkflowRun[] }) {
  const query = useQuery();
  const logins = new Set<string>();
  const current = query.get('actor');
  if (current) logins.add(current);
  for (const r of runs) {
    if (r.actor) logins.add(r.actor.login);
    if (r.triggering_actor) logins.add(r.triggering_actor.login);
  }
  return <FilterButton name="actor" label="Actor" items={[...logins].sort().map((l) => ({ id: l, text: l }))} />;
}

function WorkflowMenu({ owner, repo, workflow, defaultBranch, canAdmin }: { owner: string; repo: string; workflow: Workflow; defaultBranch: string; canAdmin: boolean }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [badge, setBadge] = useState(false);
  const enabled = workflow.state === 'active';
  const items: MenuEntry[] = [{ id: 'badge', label: 'Create status badge', onSelect: () => setBadge(true) }];
  if (canAdmin) {
    items.push({
      id: 'toggle',
      label: enabled ? 'Disable workflow' : 'Enable workflow',
      danger: enabled,
      onSelect: () => {
        setWorkflowEnabled(owner, repo, workflow.id, !enabled).then(
          () => {
            toast({ kind: 'success', title: `${workflow.name} ${enabled ? 'disabled' : 'enabled'}` });
            void refresh(workflowsKey(owner, repo), loadWorkflows(owner, repo));
          },
          (e: Error) => toast({ kind: 'error', title: e.message }),
        );
      },
    });
  }
  return (
    <>
      <IconButton ref={ref} icon={KebabHorizontalIcon} label="Workflow options" aria-expanded={open} onClick={() => setOpen(true)} />
      <Menu open={open} onClose={() => setOpen(false)} anchor={ref} items={items} />
      <BadgeDialog owner={owner} repo={repo} workflow={workflow} defaultBranch={defaultBranch} open={badge} onClose={() => setBadge(false)} />
    </>
  );
}

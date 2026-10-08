import { useEffect, useRef, useState } from 'react';
import { invalidate, mutate, refresh, useResource } from '../../api/cache';
import styles from '../../components/admin/admin.module.css';
import { StatTile } from '../../components/admin/charts';
import { formatCount, formatDateTime, formatDuration, formatKb } from '../../components/admin/format';
import { CopyButton, ErrorState, KeyValue, PageHeader, Panel, RadioCards, StatusPill, attempt, errorMessage, useConfirm, type PillStatus } from '../../components/admin/kit';
import { invalidateLists, updateLists } from '../../api/usePagedList';
import { Link, navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { Skeleton } from '../../ui/EmptyState';
import {
  AlertIcon,
  ArrowRightIcon,
  ChevronRightIcon,
  CircleSlashIcon,
  EyeIcon,
  KebabHorizontalIcon,
  LinkExternalIcon,
  LockIcon,
  PencilIcon,
  RepoForkedIcon,
  RepoIcon,
  SyncIcon,
  ToolsIcon,
  TrashIcon,
} from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { Menu, type MenuEntry } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import d from './AdminDetail.module.css';
import { DetailSkeleton, MAINTENANCE_OPS, NotFound, VisibilityPill, isNotFound, isValidLogin, modalOpen, usePrompt } from './detail';
import { NetworkRole } from './GitMaintenancePage';
import {
  deleteRepo,
  detachFork,
  getRepo,
  listMaintenance,
  pruneNow,
  runMaintenance,
  transferRepo,
  updateRepo,
  type AdminRepo,
  type MaintenanceOp,
  type MaintenanceRun,
  type RepoDetail,
  type Visibility,
} from '../../api/admin';

const enc = encodeURIComponent;
const repoKey = (owner: string, repo: string) => `admin:repo:${owner}/${repo}`;
const runsKey = (owner: string, repo: string) => `admin:repo-runs:${owner}/${repo}`;

/** Same rule as the server (`bgh_repos::create::is_valid_repo_name`). */
const isValidRepoName = (n: string) => /^[A-Za-z0-9._-]{1,100}$/.test(n) && n !== '.' && n !== '..' && !n.toLowerCase().endsWith('.git');
const REPO_NAME_RULE = 'Letters, digits, “.”, “-” and “_”; up to 100 characters; must not end in “.git”.';

const POLL_MS = 3000;

function syncRepoLists(r: AdminRepo) {
  updateLists<AdminRepo>('/_bgh/admin/repos', (items) => items.map((x) => (x.id === r.id ? r : x)));
  invalidateLists('/_bgh/admin/repos');
}

/** Owner pages embed their repositories: let them refetch. */
function invalidateOwner(login: string) {
  invalidate(`admin:user:${login}`);
  invalidate(`admin:org:${login}`);
}

export default function RepoDetailPage() {
  const { owner = '', repo = '' } = useParams<{ owner: string; repo: string }>();
  const key = repoKey(owner, repo);
  const { data, error, loading } = useResource(key, () => getRepo(owner, repo));
  if (!data) {
    if (error) {
      if (isNotFound(error))
        return <NotFound what="Repository" name={`${owner}/${repo}`} back={{ to: '/site-admin/repos', label: 'Back to repositories' }} />;
      return (
        <div className={styles.page}>
          <ErrorState error={error} onRetry={() => void refresh(key, () => getRepo(owner, repo)).catch(() => undefined)} />
        </div>
      );
    }
    if (loading) return <DetailSkeleton />;
    return null;
  }
  return <RepoDetailView key={data.repository.id} owner={owner} name={repo} data={data} />;
}

function RepoDetailView({ owner, name, data }: { owner: string; name: string; data: RepoDetail }) {
  const key = repoKey(owner, name);
  const r = data.repository;
  const confirm = useConfirm();
  const prompt = usePrompt();
  const [menuOpen, setMenuOpen] = useState(false);
  const [visibilityOpen, setVisibilityOpen] = useState(false);
  const [transferOpen, setTransferOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const ownerAdminUrl = r.owner.type === 'Organization' ? `/site-admin/orgs/${enc(r.owner.login)}` : `/site-admin/users/${enc(r.owner.login)}`;

  const set = (fn: (prev: RepoDetail) => RepoDetail) => mutate<RepoDetail>(key, (prev) => fn(prev ?? data));

  /** PATCH with an optimistic update of the given fields (rolled back on failure). */
  const patch = async (body: Parameters<typeof updateRepo>[2], optimistic: Partial<AdminRepo>) => {
    const before = r;
    set((p) => ({ ...p, repository: { ...p.repository, ...optimistic } }));
    try {
      const updated = await updateRepo(owner, name, body);
      set((p) => ({ ...p, repository: updated }));
      syncRepoLists(updated);
      invalidateOwner(owner);
      return updated;
    } catch (err) {
      set((p) => ({ ...p, repository: before }));
      throw err;
    }
  };

  /** Move the cached detail to a new URL after a rename / transfer. */
  const moved = (updated: AdminRepo) => {
    mutate<RepoDetail>(repoKey(updated.owner.login, updated.name), () => ({ ...data, repository: updated }));
    invalidate(key);
    invalidate(runsKey(owner, name));
    syncRepoLists(updated);
    invalidateOwner(owner);
    invalidateOwner(updated.owner.login);
    navigate(`/site-admin/repos/${enc(updated.owner.login)}/${enc(updated.name)}`, { replace: true });
  };

  const rename = () =>
    prompt({
      title: `Rename ${r.full_name}`,
      label: 'New repository name',
      initial: r.name,
      body: 'Web URLs redirect only until the old name is reused; update git remotes that use the old name.',
      validate: (v) => (isValidRepoName(v) ? null : REPO_NAME_RULE),
      submitLabel: 'Rename repository',
      onSubmit: async (next) => {
        const updated = await updateRepo(owner, name, { name: next });
        toast({ kind: 'success', title: `Renamed to ${updated.full_name}` });
        moved(updated);
      },
    });

  const toggleArchive = () =>
    confirm({
      title: r.archived ? `Unarchive ${r.full_name}?` : `Archive ${r.full_name}?`,
      body: r.archived
        ? 'Pushes, issues, pull requests and comments are allowed again.'
        : 'The repository becomes read-only: no pushes, issues, pull requests or comments. It stays visible and can be unarchived later.',
      confirmLabel: r.archived ? 'Unarchive' : 'Archive repository',
      danger: !r.archived,
      onConfirm: () => patch({ archived: !r.archived }, { archived: !r.archived }),
    });

  const toggleDisabled = () =>
    confirm({
      title: r.disabled ? `Re-enable ${r.full_name}?` : `Disable access to ${r.full_name}?`,
      body: r.disabled ? (
        'Collaborators and members regain access through the web, git and the API.'
      ) : (
        <>
          Blocks <strong>all access</strong> — web, git clone/push and API — for everyone except site administrators, who see “Repository access blocked”. Use this
          for legal holds, abuse or compromised content. Nothing is deleted; re-enable it at any time.
        </>
      ),
      confirmLabel: r.disabled ? 'Enable access' : 'Disable access',
      danger: !r.disabled,
      onConfirm: () => patch({ disabled: !r.disabled }, { disabled: !r.disabled }),
    });

  const remove = () =>
    confirm({
      title: `Delete ${r.full_name}?`,
      body: (
        <>
          This permanently deletes the repository, its git data, issues, pull requests, wiki and webhooks. Forks are not deleted. <strong>This can’t be undone.</strong>
        </>
      ),
      confirmLabel: 'Delete this repository',
      danger: true,
      confirmText: r.full_name,
      onConfirm: async () => {
        await deleteRepo(owner, name);
        updateLists<AdminRepo>('/_bgh/admin/repos', (items) => items.filter((x) => x.id !== r.id));
        invalidateLists('/_bgh/admin/repos');
        invalidateOwner(owner);
        invalidate(key);
        toast({ kind: 'success', title: `Deleted ${r.full_name}` });
        navigate('/site-admin/repos');
      },
    });

  const menu: MenuEntry[] = [
    { id: 'visibility', label: 'Change visibility…', icon: EyeIcon, trailing: 'V', onSelect: () => setVisibilityOpen(true) },
    { id: 'rename', label: 'Rename…', icon: PencilIcon, trailing: 'R', onSelect: rename },
    { id: 'transfer', label: 'Transfer…', icon: ArrowRightIcon, trailing: 'T', onSelect: () => setTransferOpen(true) },
    { id: 'archive', label: r.archived ? 'Unarchive…' : 'Archive…', icon: LockIcon, trailing: 'A', onSelect: toggleArchive },
    { id: 'disable', label: r.disabled ? 'Enable access…' : 'Disable access…', icon: CircleSlashIcon, onSelect: toggleDisabled },
    { id: 'sep', separator: true },
    { id: 'delete', label: 'Delete repository…', icon: TrashIcon, danger: true, onSelect: remove },
  ];

  const guard = (fn: () => void) => () => {
    if (modalOpen()) return false;
    fn();
  };
  useShortcuts('Repository admin', {
    '.': { handler: guard(() => setMenuOpen(true)), description: 'Repository actions menu', group: 'Repository admin' },
    v: { handler: guard(() => setVisibilityOpen(true)), description: 'Change visibility', group: 'Repository admin' },
    r: { handler: guard(rename), description: 'Rename repository', group: 'Repository admin' },
    t: { handler: guard(() => setTransferOpen(true)), description: 'Transfer repository', group: 'Repository admin' },
    a: { handler: guard(toggleArchive), description: 'Archive / unarchive repository', group: 'Repository admin' },
  });

  const disk = data.storage.disk_usage_kb;

  return (
    <div className={styles.page}>
      <PageHeader
        leading={<RepoIcon size={24} />}
        title={
          <span className={d.titleRow}>
            <span>
              <Link to={ownerAdminUrl} className={d.listLink}>
                {r.owner.login}
              </Link>
              {' / '}
              {r.name}
            </span>
            <span className={d.pills}>
              <VisibilityPill visibility={r.visibility} />
              {r.archived && <StatusPill status="warning">Archived</StatusPill>}
              {r.disabled && <StatusPill status="error">Disabled</StatusPill>}
              {r.fork && <Tag>Fork</Tag>}
            </span>
          </span>
        }
        description={r.description || 'No description'}
        actions={
          <>
            <Button size="sm" leadingIcon={LinkExternalIcon} onClick={() => navigate(`/${enc(r.owner.login)}/${enc(r.name)}`)}>
              Open repository
            </Button>
            <Button ref={menuRef} size="sm" leadingIcon={KebabHorizontalIcon} kbd="." aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => setMenuOpen((o) => !o)}>
              Actions
            </Button>
            <Menu open={menuOpen} onClose={() => setMenuOpen(false)} anchor={menuRef} items={menu} placement="bottom-end" aria-label="Repository actions" />
          </>
        }
      />

      {r.disabled && (
        <div className={`${d.callout} ${d.calloutDanger}`} role="status">
          <CircleSlashIcon size={16} />
          <div>
            <strong>Access disabled.</strong> Only site administrators can reach this repository through the web, git or the API.
          </div>
        </div>
      )}

      <div className={d.columns}>
        <div className={styles.stack}>
          <div className={d.counts}>
            <StatTile label="Issues" value={formatCount(data.issues_count)} />
            <StatTile label="Pull requests" value={formatCount(data.pulls_count)} />
            <StatTile label="Collaborators" value={formatCount(data.collaborators_count)} />
            <StatTile label="Teams" value={formatCount(data.teams_count)} />
            <StatTile label="Webhooks" value={formatCount(data.hooks_count)} />
            <StatTile label="Stars" value={formatCount(r.stargazers_count)} sub={`${formatCount(r.forks_count)} forks`} />
          </div>

          <Panel title="Summary">
            <KeyValue
              items={[
                [
                  'Owner',
                  <Link to={ownerAdminUrl} className={d.listLink}>
                    {r.owner.login}
                  </Link>,
                ],
                ['ID', <span className={styles.mono}>{r.id}</span>],
                ['Visibility', <VisibilityPill visibility={r.visibility} />],
                ['Default branch', <span className={styles.mono}>{r.default_branch}</span>],
                ['Language', r.language ?? <span className={styles.subtle}>Not detected</span>],
                ['Recorded size', <span title="Size recorded in the database (used for quotas)">{formatKb(r.size)}</span>],
                [
                  'Size on disk',
                  disk == null ? (
                    <span className={styles.subtle}>Not on disk</span>
                  ) : (
                    <span className={d.inline}>
                      {formatKb(disk)}
                      {Math.abs(disk - r.size) > Math.max(1024, r.size * 0.2) && <StatusPill status="warning">Differs — recalculate size</StatusPill>}
                    </span>
                  ),
                ],
                [
                  'Fork of',
                  data.parent ? (
                    <Link to={`/site-admin/repos/${data.parent.split('/').map(enc).join('/')}`} className={d.listLink}>
                      {data.parent}
                    </Link>
                  ) : (
                    <span className={styles.subtle}>Not a fork</span>
                  ),
                ],
                [
                  'Storage path',
                  <span className={d.inline}>
                    <span className={styles.mono}>{data.storage.path}</span>
                    <CopyButton text={data.storage.path} label="Copy storage path" />
                    {!data.storage.exists && <StatusPill status="error">Missing</StatusPill>}
                  </span>,
                ],
                ['Open issues', formatCount(r.open_issues_count)],
                ['Created', formatDateTime(r.created_at)],
                ['Updated', formatDateTime(r.updated_at)],
                ['Last push', r.pushed_at ? <RelativeTime date={r.pushed_at} /> : <span className={styles.subtle}>Never</span>],
              ]}
            />
          </Panel>

          <MaintenancePanel owner={owner} name={name} detail={data} onFinished={() => void refresh(key, () => getRepo(owner, name)).catch(() => undefined)} />
        </div>

        <div className={styles.stack}>
          <Panel title="Admin actions" padded={false}>
            <div className={styles.dangerRow}>
              <div>
                <strong>Visibility</strong>
                <span className={styles.subtle}>Currently {r.visibility}.</span>
              </div>
              <Button size="sm" kbd="V" onClick={() => setVisibilityOpen(true)}>
                Change
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>{r.archived ? 'Unarchive' : 'Archive'}</strong>
                <span className={styles.subtle}>{r.archived ? 'Make it writable again.' : 'Make the repository read-only.'}</span>
              </div>
              <Button size="sm" kbd="A" onClick={toggleArchive}>
                {r.archived ? 'Unarchive' : 'Archive'}
              </Button>
            </div>
          </Panel>

          <Panel title="Danger zone" danger padded={false}>
            <div className={styles.dangerRow}>
              <div>
                <strong>Rename</strong>
                <span className={styles.subtle}>Git remotes using the old name stop working.</span>
              </div>
              <Button size="sm" variant="danger" onClick={rename}>
                Rename
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>Transfer ownership</strong>
                <span className={styles.subtle}>Move to another user or organization. Team grants are dropped.</span>
              </div>
              <Button size="sm" variant="danger" onClick={() => setTransferOpen(true)}>
                Transfer
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>{r.disabled ? 'Enable access' : 'Disable access'}</strong>
                <span className={styles.subtle}>{r.disabled ? 'Restore access for collaborators.' : 'Block everyone except site admins.'}</span>
              </div>
              <Button size="sm" variant={r.disabled ? 'secondary' : 'danger'} onClick={toggleDisabled}>
                {r.disabled ? 'Enable' : 'Disable'}
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>Delete repository</strong>
                <span className={styles.subtle}>Permanently delete it and its git data.</span>
              </div>
              <Button size="sm" variant="danger" onClick={remove}>
                Delete
              </Button>
            </div>
          </Panel>
        </div>
      </div>

      <VisibilityDialog
        open={visibilityOpen}
        repo={r}
        onClose={() => setVisibilityOpen(false)}
        onSubmit={async (visibility) => {
          await patch({ visibility }, { visibility, private: visibility !== 'public' });
          toast({ kind: 'success', title: `${r.full_name} is now ${visibility}` });
        }}
      />
      <TransferDialog
        open={transferOpen}
        repo={r}
        onClose={() => setTransferOpen(false)}
        onSubmit={async (newOwner, newName) => {
          const updated = await transferRepo(owner, name, { new_owner: newOwner, new_name: newName || undefined });
          toast({ kind: 'success', title: `Transferred to ${updated.full_name}` });
          moved(updated);
        }}
      />
      {confirm.dialog}
      {prompt.dialog}
    </div>
  );
}

// ------------------------------------------------------------------ visibility

const VIS_OPTIONS: { value: Visibility; label: string; description: string }[] = [
  { value: 'public', label: 'Public', description: 'Anyone who can reach this instance can see it.' },
  { value: 'internal', label: 'Internal', description: 'Every signed-in user can see it. Organization repositories only.' },
  { value: 'private', label: 'Private', description: 'Only people with explicit access can see it.' },
];

function VisibilityDialog({ open, repo, onClose, onSubmit }: { open: boolean; repo: AdminRepo; onClose: () => void; onSubmit: (v: Visibility) => Promise<void> }) {
  const [value, setValue] = useState<Visibility>(repo.visibility);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [wasOpen, setWasOpen] = useState(open);
  if (wasOpen !== open) {
    setWasOpen(open);
    if (open) {
      setValue(repo.visibility);
      setError(null);
    }
  }
  const options = repo.owner.type === 'Organization' || repo.visibility === 'internal' ? VIS_OPTIONS : VIS_OPTIONS.filter((o) => o.value !== 'internal');
  const submit = async () => {
    if (value === repo.visibility || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onSubmit(value);
      onClose();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  const exposing = value === 'public' && repo.visibility !== 'public';
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={`Change visibility of ${repo.full_name}`}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant={exposing ? 'danger' : 'primary'} disabled={value === repo.visibility} loading={busy} onClick={() => void submit()}>
            Make {value}
          </Button>
        </>
      }
    >
      <div className={styles.form}>
        <RadioCards name="repo-visibility" label="Visibility" value={value} onChange={setValue} options={options} />
        {exposing && (
          <div className={d.callout} style={{ margin: 0 }}>
            <AlertIcon size={16} />
            <span>All code, issues and history become visible to anyone who can reach this instance, including anonymous visitors if allowed.</span>
          </div>
        )}
        {value === 'private' && repo.visibility !== 'private' && (
          <p className={styles.subtle}>Stars and watchers from people without access are removed; forks keep their current visibility.</p>
        )}
        {error && (
          <div className={styles.formError} role="alert">
            <AlertIcon size={14} /> {error}
          </div>
        )}
      </div>
    </Dialog>
  );
}

// ------------------------------------------------------------------ transfer

function TransferDialog({
  open,
  repo,
  onClose,
  onSubmit,
}: {
  open: boolean;
  repo: AdminRepo;
  onClose: () => void;
  onSubmit: (newOwner: string, newName: string) => Promise<void>;
}) {
  const [newOwner, setNewOwner] = useState('');
  const [newName, setNewName] = useState('');
  const [typed, setTyped] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [wasOpen, setWasOpen] = useState(open);
  if (wasOpen !== open) {
    setWasOpen(open);
    if (open) {
      setNewOwner('');
      setNewName('');
      setTyped('');
      setError(null);
    }
  }
  const ownerOk = isValidLogin(newOwner);
  const nameOk = !newName || isValidRepoName(newName);
  const same = newOwner.toLowerCase() === repo.owner.login.toLowerCase() && (!newName || newName === repo.name);
  const blocked = !ownerOk || !nameOk || same || typed !== repo.full_name;
  const submit = async () => {
    if (blocked || busy) return;
    setBusy(true);
    setError(null);
    try {
      await onSubmit(newOwner, newName);
      onClose();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={`Transfer ${repo.full_name}`}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="danger" disabled={blocked} loading={busy} onClick={() => void submit()}>
            Transfer repository
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
        <p className={styles.confirmBody}>
          The repository moves immediately, without asking the new owner. Team access is removed; internal repositories moved to a user become private.
        </p>
        <div className={styles.formRow}>
          <Field label="New owner" htmlFor="tr-owner" error={newOwner && !ownerOk ? 'Enter a valid user or organization login.' : null}>
            <Input id="tr-owner" value={newOwner} onChange={(e) => setNewOwner(e.target.value.trim())} autoFocus autoComplete="off" spellCheck={false} invalid={!!newOwner && !ownerOk} />
          </Field>
          <Field label="New name (optional)" htmlFor="tr-name" error={nameOk ? null : REPO_NAME_RULE}>
            <Input id="tr-name" value={newName} placeholder={repo.name} onChange={(e) => setNewName(e.target.value.trim())} autoComplete="off" spellCheck={false} invalid={!nameOk} />
          </Field>
        </div>
        {same && newOwner && <span className={styles.subtle}>That is the current owner and name.</span>}
        <Field label={`Type ${repo.full_name} to confirm`} htmlFor="tr-confirm">
          <Input id="tr-confirm" value={typed} onChange={(e) => setTyped(e.target.value)} autoComplete="off" spellCheck={false} />
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

// ------------------------------------------------------------------ maintenance

const RUN_STATUS: Record<string, { status: PillStatus; label: string }> = {
  queued: { status: 'neutral', label: 'Queued' },
  running: { status: 'info', label: 'Running' },
  succeeded: { status: 'ok', label: 'Succeeded' },
  failed: { status: 'error', label: 'Failed' },
};

const isActive = (run: MaintenanceRun) => run.status === 'queued' || run.status === 'running';

function runDuration(run: MaintenanceRun): string {
  if (!run.started_at) return '—';
  const end = run.finished_at ? new Date(run.finished_at).getTime() : Date.now();
  return formatDuration((end - new Date(run.started_at).getTime()) / 1000);
}

function MaintenancePanel({ owner, name, detail, onFinished }: { owner: string; name: string; detail: RepoDetail; onFinished: () => void }) {
  const key = runsKey(owner, name);
  const loader = () => listMaintenance(owner, name);
  const { data: runs, error, loading } = useResource(key, loader, { ttlMs: POLL_MS });
  const [pending, setPending] = useState<MaintenanceOp | null>(null);
  const active = !!runs?.some(isActive);

  // Poll while a run is queued or running; when the last one settles,
  // refresh the repository (size / language may have changed).
  const finishedRef = useRef(onFinished);
  useEffect(() => {
    finishedRef.current = onFinished;
  });
  const wasActive = useRef(active);
  useEffect(() => {
    if (wasActive.current && !active) finishedRef.current();
    wasActive.current = active;
    if (!active) return;
    const id = setInterval(() => void refresh(key, () => listMaintenance(owner, name)).catch(() => undefined), POLL_MS);
    return () => clearInterval(id);
  }, [active, key, owner, name]);

  const confirm = useConfirm();
  const network = detail.network ?? { has_alternates: false, has_dependents: false };
  const status = detail.git_maintenance ?? null;
  const inNetwork = !!detail.parent || network.has_alternates;
  const prepend = (run: MaintenanceRun) => mutate<MaintenanceRun[]>(key, (prev) => [run, ...(prev ?? []).filter((x) => x.id !== run.id)]);

  const prune = () =>
    confirm({
      title: 'Prune unreachable objects now?',
      body: (
        <>
          Deletes every object no ref of <strong>{`${owner}/${name}`}</strong> reaches, skipping the grace period. Objects of a push in progress can be lost. The run is recorded
          in the audit log.
        </>
      ),
      confirmText: name,
      confirmLabel: 'Prune now',
      danger: true,
      onConfirm: async () => {
        prepend(await pruneNow(owner, name));
        toast({ kind: 'success', title: 'Scheduled prune' });
      },
    });

  const detach = () =>
    confirm({
      title: 'Leave the fork network?',
      body: (
        <>
          <strong>{`${owner}/${name}`}</strong> stops being a fork{detail.parent ? <> of {detail.parent}</> : null}: it copies every object it borrows and no longer shares
          storage. Its own forks follow it. This can’t be undone.
        </>
      ),
      confirmLabel: 'Leave fork network',
      danger: true,
      onConfirm: async () => {
        prepend(await detachFork(owner, name));
        toast({ kind: 'success', title: 'Leaving the fork network' });
      },
    });

  const start = async (op: MaintenanceOp) => {
    setPending(op);
    await attempt(
      'Could not schedule maintenance',
      async () => {
        const run = await runMaintenance(owner, name, op);
        mutate<MaintenanceRun[]>(key, (prev) => [run, ...(prev ?? []).filter((x) => x.id !== run.id)]);
      },
      `Scheduled ${op.replace(/_/g, ' ')}`,
    );
    setPending(null);
  };

  return (
    <Panel
      title="Maintenance"
      padded={false}
      actions={
        <Button size="sm" variant="ghost" leadingIcon={SyncIcon} onClick={() => void refresh(key, loader).catch(() => undefined)}>
          Refresh
        </Button>
      }
    >
      <div className={d.opStatus}>
        <NetworkRole hasAlternates={network.has_alternates} hasDependents={network.has_dependents} />
        {status ? (
          <span className={styles.subtle} title={status.error ?? undefined}>
            Scheduled: {status.status}
            {status.last_run_at && (
              <>
                {' '}
                <RelativeTime date={status.last_run_at} />
              </>
            )}
            {' · '}
            {formatCount(status.pack_count)} packs · {formatCount(status.loose_count)} loose
          </span>
        ) : (
          <span className={styles.subtle}>Not yet picked up by scheduled maintenance</span>
        )}
      </div>
      <div className={d.opButtons}>
        {MAINTENANCE_OPS.map((op) => (
          <Button key={op.id} size="sm" leadingIcon={ToolsIcon} title={op.description} loading={pending === op.id} disabled={pending !== null} onClick={() => void start(op.id)}>
            {op.label}
          </Button>
        ))}
        <Button
          size="sm"
          variant="danger"
          leadingIcon={TrashIcon}
          disabled={network.has_dependents || pending !== null}
          title={network.has_dependents ? 'Forks borrow objects from this repository; pruning is not allowed.' : 'Delete unreachable objects now, without the grace period.'}
          onClick={prune}
        >
          Prune now…
        </Button>
        {inNetwork && (
          <Button size="sm" leadingIcon={RepoForkedIcon} disabled={pending !== null} onClick={detach} title="Make this repository self-contained and detach it from its parent.">
            Leave fork network…
          </Button>
        )}
      </div>
      {confirm.dialog}
      {!runs ? (
        loading ? (
          <div className={d.emptyRow}>
            <Skeleton width="60%" />
          </div>
        ) : (
          <div className={d.emptyRow}>{error ? `Could not load runs: ${errorMessage(error)}` : 'No runs yet.'}</div>
        )
      ) : runs.length === 0 ? (
        <div className={d.emptyRow}>No maintenance has run on this repository yet.</div>
      ) : (
        <div className={d.scrollList} aria-live="polite">
          {runs.map((run) => {
            const s = RUN_STATUS[run.status] ?? { status: 'unknown' as const, label: run.status };
            return (
              <details key={run.id} className={d.run}>
                <summary>
                  <ChevronRightIcon size={14} />
                  <StatusPill status={s.status}>{s.label}</StatusPill>
                  <span className={d.runOp}>{run.operation}</span>
                  <span className={d.rowMeta}>
                    <span title={`Requested ${formatDateTime(run.created_at)}`}>
                      <RelativeTime date={run.created_at} />
                    </span>
                    <span title="Duration">{runDuration(run)}</span>
                    <span title="Finished">{run.finished_at ? formatDateTime(run.finished_at) : isActive(run) ? '…' : '—'}</span>
                  </span>
                </summary>
                {run.output ? <pre className={d.runOutput}>{run.output}</pre> : <p className={d.runEmptyOutput}>{isActive(run) ? 'Output appears when the run finishes.' : 'No output.'}</p>}
              </details>
            );
          })}
        </div>
      )}
    </Panel>
  );
}

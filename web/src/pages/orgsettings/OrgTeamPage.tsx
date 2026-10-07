import { useMemo, useState } from 'react';
import { invalidate, mutate, refresh, useResource } from '../../api/cache';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { plural } from '../../components/admin/format';
import { ErrorState, PageHeader, Panel, SearchInput, StatusPill, errorMessage, useConfirm } from '../../components/admin/kit';
import { usePagedList } from '../../components/admin/usePagedList';
import { Link, navigate, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Tag } from '../../ui/Badge';
import { Button, IconButton } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { ArrowLeftIcon, CheckIcon, LockIcon, PeopleIcon, PersonAddIcon, PlusIcon, RepoIcon, TrashIcon } from '../../ui/icons';
import { Field, Input, Select, Textarea } from '../../ui/Input';
import { TabNav } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import {
  allTeamsKey,
  childTeamsPath,
  deleteTeam,
  fieldErrors,
  getTeam,
  listOrgRepos,
  orgReposKey,
  removeTeamMembership,
  removeTeamRepo,
  setTeamMembership,
  setTeamRepo,
  teamKey,
  teamMembersPath,
  teamRepoPermission,
  teamReposPath,
  updateTeam,
  type SimpleUser,
  type Team,
  type TeamFull,
  type TeamRepo,
  type TeamRepoPermission,
  type TeamRole,
} from './api';
import { RowMenu, TeamDialog, TeamPrivacyChoice, descendants, privacyPill, teamPath, teamTree, useAllTeams, useLoadAll, useOrgAccess, userCell } from './common';
import { orgSettingsPath } from './OrgSettingsLayout';
import local from './OrgSettings.module.css';

const REPO_PERMISSIONS: { value: TeamRepoPermission; label: string; description: string }[] = [
  { value: 'pull', label: 'Read', description: 'Can read and clone, open and comment on issues and pull requests.' },
  { value: 'triage', label: 'Triage', description: 'Read, plus manage issues and pull requests.' },
  { value: 'push', label: 'Write', description: 'Triage, plus push to the repository.' },
  { value: 'maintain', label: 'Maintain', description: 'Write, plus manage the repository without destructive actions.' },
  { value: 'admin', label: 'Admin', description: 'Full access, including sensitive and destructive actions.' },
];

const PERMISSION_LABEL = Object.fromEntries(REPO_PERMISSIONS.map((p) => [p.value, p.label])) as Record<TeamRepoPermission, string>;

type Tab = 'members' | 'teams' | 'repos' | 'settings';

export default function OrgTeamPage() {
  const { org = '', team: slug = '' } = useParams<{ org: string; team: string }>();
  const query = useQuery();
  const tab = (query.get('tab') as Tab | null) ?? 'members';
  const res = useResource(teamKey(org, slug), () => getTeam(org, slug));
  const access = useOrgAccess(org);
  const maintainers = usePagedList<SimpleUser>(teamMembersPath(org, slug, 'maintainer'));
  useLoadAll(maintainers);
  const canManage = access.isOwner || maintainers.items.some((u) => u.login === access.me);
  const team = res.data;
  const base = teamPath(org, slug);

  if (res.error && !team) {
    return (
      <div className={styles.page}>
        <PageHeader title={slug} leading={<BackLink org={org} />} />
        <ErrorState error={res.error} title="Could not load this team" onRetry={() => void refresh(teamKey(org, slug), () => getTeam(org, slug))} />
      </div>
    );
  }

  const tabs = [
    { id: 'members', label: 'Members', icon: PeopleIcon, count: team?.members_count, href: base },
    { id: 'teams', label: 'Child teams', icon: PeopleIcon, href: `${base}?tab=teams` },
    { id: 'repos', label: 'Repositories', icon: RepoIcon, count: team?.repos_count, href: `${base}?tab=repos` },
    ...(canManage ? [{ id: 'settings', label: 'Settings', href: `${base}?tab=settings` }] : []),
  ];

  return (
    <div className={styles.fill}>
      <PageHeader
        leading={<BackLink org={org} />}
        title={
          <span className={local.titleRow}>
            {team?.name ?? slug} {team && privacyPill(team.privacy)}
          </span>
        }
        description={
          team ? (
            <>
              {team.parent && (
                <>
                  Child of{' '}
                  <Link to={teamPath(org, team.parent.slug)}>
                    <strong>{team.parent.name}</strong>
                  </Link>
                  {team.description ? ' · ' : ''}
                </>
              )}
              {team.description ?? (team.parent ? '' : <span className={styles.subtle}>No description</span>)}
            </>
          ) : (
            <Skeleton width={240} />
          )
        }
      />
      <TabNav items={tabs} current={tab} aria-label="Team sections" className={local.tabNav} />
      {tab === 'members' && <MembersTab org={org} slug={slug} canManage={canManage} me={access.me} onChange={() => void refresh(teamKey(org, slug), () => getTeam(org, slug))} />}
      {tab === 'teams' && <ChildTeamsTab org={org} slug={slug} team={team} canManage={access.isOwner} />}
      {tab === 'repos' && <ReposTab org={org} slug={slug} canManage={canManage} onChange={() => void refresh(teamKey(org, slug), () => getTeam(org, slug))} />}
      {tab === 'settings' && team && (canManage ? <SettingsTab org={org} team={team} /> : <EmptyState icon={LockIcon} title="Only owners and team maintainers can edit this team" />)}
    </div>
  );
}

function BackLink({ org }: { org: string }) {
  return (
    <Link to={orgSettingsPath(org, 'teams')} aria-label="All teams" className={local.back}>
      <ArrowLeftIcon size={16} />
    </Link>
  );
}

// ------------------------------------------------------------------ members

interface MemberRow {
  user: SimpleUser;
  role: TeamRole | 'child';
}

function MembersTab({ org, slug, canManage, me, onChange }: { org: string; slug: string; canManage: boolean; me: string | null; onChange: () => void }) {
  const all = usePagedList<SimpleUser>(teamMembersPath(org, slug, 'all'));
  const maintainers = usePagedList<SimpleUser>(teamMembersPath(org, slug, 'maintainer'));
  const direct = usePagedList<SimpleUser>(teamMembersPath(org, slug, 'member'));
  useLoadAll(all);
  useLoadAll(maintainers);
  useLoadAll(direct);
  const [q, setQ] = useState('');
  const [adding, setAdding] = useState(false);
  const confirm = useConfirm();

  const rows = useMemo(() => {
    const m = new Set(maintainers.items.map((u) => u.id));
    const d = new Set(direct.items.map((u) => u.id));
    const needle = q.trim().toLowerCase();
    return all.items
      .filter((u) => !needle || u.login.toLowerCase().includes(needle))
      .map((user): MemberRow => ({ user, role: m.has(user.id) ? 'maintainer' : d.has(user.id) ? 'member' : 'child' }))
      .sort((a, b) => a.user.login.localeCompare(b.user.login, undefined, { sensitivity: 'base' }));
  }, [all.items, maintainers.items, direct.items, q]);

  const reload = () => {
    void all.reload();
    void maintainers.reload();
    void direct.reload();
    onChange();
  };

  const changeRole = async (row: MemberRow, role: TeamRole) => {
    if (row.role === role) return;
    // Optimistic: move between the role lists.
    const [from, to] = role === 'maintainer' ? [direct, maintainers] : [maintainers, direct];
    from.update((items) => items.filter((u) => u.id !== row.user.id));
    to.update((items) => [...items, row.user]);
    try {
      await setTeamMembership(org, slug, row.user.login, role);
      toast({ kind: 'success', title: `${row.user.login} is now a ${role}` });
    } catch (err) {
      to.update((items) => items.filter((u) => u.id !== row.user.id));
      if (row.role !== 'child') from.update((items) => [...items, row.user]);
      toast({ kind: 'error', title: `Could not change the role of ${row.user.login}`, description: errorMessage(err) });
    }
  };

  const askRemove = (row: MemberRow) =>
    confirm({
      title: `Remove ${row.user.login} from this team?`,
      body: (
        <>
          <strong>{row.user.login}</strong> loses the repository access granted through this team (and its parent teams). They stay a member of {org}.
        </>
      ),
      confirmLabel: 'Remove from team',
      danger: true,
      onConfirm: async () => {
        await removeTeamMembership(org, slug, row.user.login);
        for (const l of [all, maintainers, direct]) l.update((items) => items.filter((u) => u.id !== row.user.id));
        onChange();
        toast({ kind: 'success', title: `Removed ${row.user.login} from the team` });
      },
    });

  const columns: Column<MemberRow>[] = [
    { id: 'user', header: 'Member', width: 'minmax(220px, 3fr)', render: (r) => userCell(r.user, undefined, me) },
    {
      id: 'role',
      header: 'Role',
      width: '140px',
      render: (r) =>
        r.role === 'maintainer' ? <StatusPill status="info">Maintainer</StatusPill> : r.role === 'member' ? <span className={styles.muted}>Member</span> : <span className={styles.subtle}>Via child team</span>,
    },
    {
      id: 'actions',
      header: <span className="visually-hidden">Actions</span>,
      width: '44px',
      align: 'end',
      render: (r) =>
        canManage && r.role !== 'child' ? (
          <RowMenu
            label={`Actions for ${r.user.login}`}
            items={[
              { header: 'Role', id: 'h' },
              { id: 'member', label: 'Member', trailing: r.role === 'member' ? <CheckIcon size={14} /> : undefined, onSelect: () => void changeRole(r, 'member') },
              {
                id: 'maintainer',
                label: 'Maintainer',
                description: 'Can manage members and settings of this team.',
                trailing: r.role === 'maintainer' ? <CheckIcon size={14} /> : undefined,
                onSelect: () => void changeRole(r, 'maintainer'),
              },
              { separator: true, id: 's' },
              { id: 'remove', label: 'Remove from team…', icon: TrashIcon, danger: true, onSelect: () => askRemove(r) },
            ]}
          />
        ) : null,
    },
  ];

  useShortcuts('Team members', { a: { handler: () => canManage && setAdding(true), description: 'Add a member', group: 'Team' } });

  return (
    <>
      <div className={styles.toolbar}>
        <SearchInput label="Find a member" placeholder="Find a member…" value={q} onChange={setQ} />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta}>{all.done ? `${plural(all.items.length, 'member')}` : ''}</span>
        {canManage && (
          <Button size="sm" variant="primary" leadingIcon={PersonAddIcon} kbd="A" onClick={() => setAdding(true)}>
            Add a member
          </Button>
        )}
      </div>
      <DataTable
        aria-label="Team members"
        rows={rows}
        columns={columns}
        getKey={(r) => r.user.id}
        onOpen={(r) => navigate(`/${encodeURIComponent(r.user.login)}`)}
        loading={all.loading && rows.length === 0}
        empty={
          all.error ? (
            <EmptyState icon={PeopleIcon} title="Could not load members">
              {errorMessage(all.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={PeopleIcon} title={q ? 'No members match' : 'This team has no members'}>
              {canManage && !q ? 'Add organization members to give them the team’s repository access.' : undefined}
            </EmptyState>
          )
        }
      />
      <AddMemberDialog org={org} slug={slug} open={adding} onClose={() => setAdding(false)} onAdded={reload} />
      {confirm.dialog}
    </>
  );
}

function AddMemberDialog({ org, slug, open, onClose, onAdded }: { org: string; slug: string; open: boolean; onClose: () => void; onAdded: () => void }) {
  const [login, setLogin] = useState('');
  const [role, setRole] = useState<TeamRole>('member');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shownFor, setShownFor] = useState(false);
  if (open !== shownFor) {
    setShownFor(open);
    if (open) {
      setLogin('');
      setRole('member');
      setError(null);
    }
  }
  const value = login.trim().replace(/^@/, '');
  const ok = /^[a-z\d](?:[a-z\d]|-(?=[a-z\d])){0,38}$/i.test(value);
  const submit = async () => {
    if (!ok || busy) return;
    setBusy(true);
    setError(null);
    try {
      const m = await setTeamMembership(org, slug, value, role);
      toast({ kind: 'success', title: m.state === 'pending' ? `Invited ${value}` : `Added ${value} to the team`, description: m.state === 'pending' ? `${value} joins the team once they accept the organization invitation.` : undefined });
      onAdded();
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
      title="Add a team member"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!ok} onClick={() => void submit()}>
            Add member
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
        <Field label="Username" htmlFor="tm-login" error={error}>
          <Input id="tm-login" value={login} onChange={(e) => setLogin(e.target.value)} autoFocus autoComplete="off" spellCheck={false} invalid={!!error} />
        </Field>
        <Field label="Role" htmlFor="tm-role" hint="Maintainers can add and remove members and edit the team.">
          <Select id="tm-role" value={role} onChange={(e) => setRole(e.target.value as TeamRole)}>
            <option value="member">Member</option>
            <option value="maintainer">Maintainer</option>
          </Select>
        </Field>
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}

// ------------------------------------------------------------------ child teams

function ChildTeamsTab({ org, slug, team, canManage }: { org: string; slug: string; team: TeamFull | undefined; canManage: boolean }) {
  const children = usePagedList<Team>(childTeamsPath(org, slug));
  useLoadAll(children);
  const allTeams = useAllTeams(org);
  const [creating, setCreating] = useState(false);
  // Grandchildren come from the full team list when it is loaded.
  const rows = useMemo(() => {
    if (!team || !allTeams.data) return children.items.map((t) => ({ team: t, depth: 0, path: t.name }));
    const sub = descendants(allTeams.data, team.id);
    sub.delete(team.id);
    const tree = teamTree(allTeams.data.filter((t) => sub.has(t.id)).map((t) => (t.parent?.id === team.id ? { ...t, parent: null } : t)));
    return tree.length ? tree : children.items.map((t) => ({ team: t, depth: 0, path: t.name }));
  }, [team, allTeams.data, children.items]);

  return (
    <>
      <div className={styles.toolbar}>
        <span className={styles.meta}>Child teams inherit the repository access of {team?.name ?? 'this team'}; their members are listed as members of this team.</span>
        <span className={styles.toolbarSpacer} />
        {canManage && team?.privacy === 'closed' && (
          <Button size="sm" leadingIcon={PlusIcon} onClick={() => setCreating(true)}>
            New child team
          </Button>
        )}
      </div>
      <DataTable
        aria-label="Child teams"
        rows={rows}
        columns={[
          {
            id: 'team',
            header: 'Team',
            width: 'minmax(220px, 3fr)',
            render: (r) => (
              <span className={local.treeCell} style={{ paddingLeft: r.depth * 20 }}>
                {r.depth > 0 && <span className={local.treeElbow} aria-hidden />}
                <PeopleIcon size={16} className={styles.subtle} />
                <span className={styles.cellMain}>
                  <strong>{r.team.name}</strong>
                  <span className={styles.subtle}>{r.team.description || ' '}</span>
                </span>
              </span>
            ),
          },
          { id: 'privacy', header: 'Visibility', width: '104px', render: (r) => privacyPill(r.team.privacy) },
        ]}
        getKey={(r) => r.team.id}
        href={(r) => teamPath(org, r.team.slug)}
        loading={children.loading && rows.length === 0}
        empty={
          children.error ? (
            <EmptyState icon={PeopleIcon} title="Could not load child teams">
              {errorMessage(children.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={PeopleIcon} title="No child teams">
              {team?.privacy === 'secret' ? 'Secret teams can’t have child teams.' : 'Nest teams to mirror your organization’s structure.'}
            </EmptyState>
          )
        }
      />
      <TeamDialog org={org} open={creating} onClose={() => setCreating(false)} teams={allTeams.data ?? []} parentId={team?.id} />
    </>
  );
}

// ------------------------------------------------------------------ repositories

function ReposTab({ org, slug, canManage, onChange }: { org: string; slug: string; canManage: boolean; onChange: () => void }) {
  const list = usePagedList<TeamRepo>(teamReposPath(org, slug));
  useLoadAll(list);
  const [q, setQ] = useState('');
  const [adding, setAdding] = useState(false);
  const confirm = useConfirm();
  const rows = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return needle ? list.items.filter((r) => r.full_name.toLowerCase().includes(needle)) : list.items;
  }, [list.items, q]);

  const changePermission = async (repo: TeamRepo, permission: TeamRepoPermission) => {
    const prev = repo.role_name;
    const roleName = { pull: 'read', triage: 'triage', push: 'write', maintain: 'maintain', admin: 'admin' }[permission];
    list.update((items) => items.map((r) => (r.id === repo.id ? { ...r, role_name: roleName } : r)));
    try {
      await setTeamRepo(org, slug, repo.owner.login, repo.name, permission);
      toast({ kind: 'success', title: `${repo.full_name}: ${PERMISSION_LABEL[permission]} access` });
    } catch (err) {
      list.update((items) => items.map((r) => (r.id === repo.id ? { ...r, role_name: prev } : r)));
      toast({ kind: 'error', title: `Could not change access to ${repo.full_name}`, description: errorMessage(err) });
    }
  };

  const askRemove = (repo: TeamRepo) =>
    confirm({
      title: `Remove ${repo.full_name} from this team?`,
      body: 'Members of the team (and its child teams) lose the access granted through the team. Access granted another way is kept.',
      confirmLabel: 'Remove repository',
      danger: true,
      onConfirm: async () => {
        await removeTeamRepo(org, slug, repo.owner.login, repo.name);
        list.update((items) => items.filter((r) => r.id !== repo.id));
        onChange();
        toast({ kind: 'success', title: `Removed ${repo.full_name}` });
      },
    });

  const columns: Column<TeamRepo>[] = [
    {
      id: 'repo',
      header: 'Repository',
      width: 'minmax(220px, 3fr)',
      render: (r) => (
        <>
          <RepoIcon size={16} className={styles.subtle} />
          <span className={styles.cellMain}>
            <strong>
              {r.full_name} {r.private && <Tag>Private</Tag>} {r.archived && <Tag>Archived</Tag>}
            </strong>
            <span className={styles.subtle}>{r.description || ' '}</span>
          </span>
        </>
      ),
    },
    {
      id: 'permission',
      header: 'Access',
      width: '140px',
      render: (r) => {
        const p = teamRepoPermission(r);
        return canManage ? (
          <span onClick={(e) => e.stopPropagation()} className={local.cellControl}>
            <Select aria-label={`Access to ${r.full_name}`} value={p} onChange={(e) => void changePermission(r, e.target.value as TeamRepoPermission)} className={local.compactSelect}>
              {REPO_PERMISSIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {o.label}
                </option>
              ))}
            </Select>
          </span>
        ) : (
          PERMISSION_LABEL[p]
        );
      },
    },
    {
      id: 'actions',
      header: <span className="visually-hidden">Actions</span>,
      width: '44px',
      align: 'end',
      render: (r) =>
        canManage ? (
          <span onClick={(e) => e.stopPropagation()}>
            <IconButton icon={TrashIcon} label={`Remove ${r.full_name}`} size="sm" onClick={() => askRemove(r)} />
          </span>
        ) : null,
    },
  ];

  useShortcuts('Team repositories', { a: { handler: () => canManage && setAdding(true), description: 'Add a repository', group: 'Team' } });

  return (
    <>
      <div className={styles.toolbar}>
        <SearchInput label="Find a repository" placeholder="Find a repository…" value={q} onChange={setQ} />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta}>{list.done ? `${plural(list.items.length, 'repository')}` : ''}</span>
        {canManage && (
          <Button size="sm" variant="primary" leadingIcon={PlusIcon} kbd="A" onClick={() => setAdding(true)}>
            Add repository
          </Button>
        )}
      </div>
      <DataTable
        aria-label="Team repositories"
        rows={rows}
        columns={columns}
        getKey={(r) => r.id}
        onOpen={(r) => navigate(`/${r.full_name}`)}
        loading={list.loading && rows.length === 0}
        empty={
          list.error ? (
            <EmptyState icon={RepoIcon} title="Could not load repositories">
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={RepoIcon} title={q ? 'No repositories match' : 'No repositories'}>
              {!q && canManage ? 'Give the team access to repositories of the organization.' : undefined}
            </EmptyState>
          )
        }
      />
      <AddRepoDialog
        org={org}
        slug={slug}
        open={adding}
        onClose={() => setAdding(false)}
        existing={list.items.map((r) => r.full_name)}
        onAdded={() => {
          void list.reload();
          onChange();
        }}
      />
      {confirm.dialog}
    </>
  );
}

function AddRepoDialog({ org, slug, open, onClose, existing, onAdded }: { org: string; slug: string; open: boolean; onClose: () => void; existing: string[]; onAdded: () => void }) {
  const [name, setName] = useState('');
  const [permission, setPermission] = useState<TeamRepoPermission>('pull');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shownFor, setShownFor] = useState(false);
  if (open !== shownFor) {
    setShownFor(open);
    if (open) {
      setName('');
      setPermission('pull');
      setError(null);
    }
  }
  // Suggestions (best effort: the list endpoint may be unavailable).
  const repos = useResource(open ? orgReposKey(org) : null, () => listOrgRepos(org));
  const raw = name.trim();
  const [owner, repo] = raw.includes('/') ? (raw.split('/', 2) as [string, string]) : [org, raw];
  const ok = /^[\w.-]+$/.test(repo ?? '') && /^[\w.-]+$/.test(owner);
  const already = existing.includes(`${owner}/${repo}`);
  const submit = async () => {
    if (!ok || busy) return;
    setBusy(true);
    setError(null);
    try {
      await setTeamRepo(org, slug, owner, repo, permission);
      toast({ kind: 'success', title: `Added ${owner}/${repo}` });
      onAdded();
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
      title="Add a repository"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!ok} onClick={() => void submit()}>
            {already ? 'Update access' : 'Add repository'}
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
        <Field label="Repository" htmlFor="tr-name" error={error} hint={already ? 'The team already has access; this changes its permission.' : `A repository of ${org}.`}>
          <Input id="tr-name" list="tr-repos" value={name} onChange={(e) => setName(e.target.value)} autoFocus autoComplete="off" spellCheck={false} placeholder="repository-name" invalid={!!error} />
        </Field>
        <datalist id="tr-repos">
          {(repos.data ?? []).map((r) => (
            <option key={r.id} value={r.name}>
              {r.description ?? ''}
            </option>
          ))}
        </datalist>
        <Field label="Permission" htmlFor="tr-perm" hint={REPO_PERMISSIONS.find((p) => p.value === permission)?.description}>
          <Select id="tr-perm" value={permission} onChange={(e) => setPermission(e.target.value as TeamRepoPermission)}>
            {REPO_PERMISSIONS.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </Select>
        </Field>
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}

// ------------------------------------------------------------------ settings

function SettingsTab({ org, team }: { org: string; team: TeamFull }) {
  const allTeams = useAllTeams(org);
  const initial = { name: team.name, description: team.description ?? '', privacy: team.privacy, parent: team.parent ? String(team.parent.id) : '' };
  const [form, setForm] = useState(initial);
  const [baseFor, setBaseFor] = useState(team);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const confirm = useConfirm();
  const dirty = form.name !== initial.name || form.description !== initial.description || form.privacy !== initial.privacy || form.parent !== initial.parent;
  if (team !== baseFor && !dirty) {
    setBaseFor(team);
    setForm(initial);
  }
  const set = (p: Partial<typeof form>) => setForm((f) => ({ ...f, ...p }));

  const teams = allTeams.data ?? [];
  const blocked = descendants(teams, team.id);
  const hasChildren = teams.some((t) => t.parent?.id === team.id);
  const parents = teamTree(teams.filter((t) => t.privacy !== 'secret' && !blocked.has(t.id)));
  const nested = !!form.parent || hasChildren;
  const subteams = blocked.size - 1;

  const save = async () => {
    if (!dirty || busy || !form.name.trim()) return;
    setBusy(true);
    setErrors({});
    setError(null);
    try {
      const patch: Parameters<typeof updateTeam>[2] = {};
      if (form.name !== initial.name) patch.name = form.name.trim();
      if (form.description !== initial.description) patch.description = form.description.trim() || null;
      if (form.privacy !== initial.privacy) patch.privacy = form.privacy;
      if (form.parent !== initial.parent) patch.parent_team_id = form.parent ? Number(form.parent) : null;
      const updated = await updateTeam(org, team.slug, patch);
      mutate<TeamFull>(teamKey(org, updated.slug), () => updated);
      mutate<Team[]>(allTeamsKey(org), (prev) => (prev ?? []).map((t) => (t.id === updated.id ? updated : t)));
      toast({ kind: 'success', title: 'Team updated' });
      if (updated.slug !== team.slug) {
        invalidate(teamKey(org, team.slug));
        navigate(`${teamPath(org, updated.slug)}?tab=settings`, { replace: true });
      }
    } catch (err) {
      const fe = fieldErrors(err);
      if (fe.parent_team_id) fe.parent = fe.parent_team_id;
      setErrors(fe);
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  useShortcuts('Team settings', { 'mod+s': { handler: () => void save(), description: 'Save team', group: 'Team' } });

  const askDelete = () =>
    confirm({
      title: `Delete ${team.name}?`,
      body: (
        <>
          Members lose the repository access granted through <strong>{team.name}</strong>.
          {subteams > 0 && (
            <>
              {' '}
              Its <strong>{subteams} child team{subteams === 1 ? '' : 's'}</strong> will be deleted too.
            </>
          )}{' '}
          This can’t be undone.
        </>
      ),
      confirmLabel: 'Delete team',
      danger: true,
      confirmText: team.slug,
      onConfirm: async () => {
        await deleteTeam(org, team.slug);
        mutate<Team[]>(allTeamsKey(org), (prev) => (prev ?? []).filter((t) => !blocked.has(t.id)));
        invalidate(teamKey(org, team.slug));
        toast({ kind: 'success', title: `Deleted ${team.name}` });
        navigate(orgSettingsPath(org, 'teams'));
      },
    });

  return (
    <div className={local.scroll}>
      <div className={styles.page}>
        <form
          className={styles.stack}
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <Panel title="Team settings">
            <div className={styles.form}>
              <Field label="Team name" htmlFor="ts-name" error={errors.name ?? (form.name.trim() ? null : 'Name is required.')} hint="Renaming changes the team’s URL and @mention.">
                <Input id="ts-name" value={form.name} onChange={(e) => set({ name: e.target.value })} invalid={!!errors.name || !form.name.trim()} />
              </Field>
              <Field label="Description" htmlFor="ts-desc" error={errors.description ?? null}>
                <Textarea id="ts-desc" rows={2} value={form.description} onChange={(e) => set({ description: e.target.value })} />
              </Field>
              <Field label="Parent team" htmlFor="ts-parent" error={errors.parent ?? null} hint="Secret teams, this team and its children can’t be selected.">
                <Select id="ts-parent" value={form.parent} onChange={(e) => set({ parent: e.target.value, privacy: e.target.value ? 'closed' : form.privacy })}>
                  <option value="">No parent team</option>
                  {parents.map((r) => (
                    <option key={r.team.id} value={r.team.id}>
                      {'  '.repeat(r.depth)}
                      {r.team.name}
                    </option>
                  ))}
                </Select>
              </Field>
              <TeamPrivacyChoice
                value={nested ? 'closed' : form.privacy}
                onChange={(v) => set({ privacy: v })}
                secretDisabled={nested}
                secretReason={hasChildren ? 'Teams with child teams can’t be secret.' : 'Nested teams can’t be secret.'}
              />
              {error && (
                <div className={styles.formError} role="alert">
                  {error}
                </div>
              )}
            </div>
          </Panel>
          {dirty && (
            <div className={styles.saveBar} role="region" aria-label="Unsaved changes">
              <span>Unsaved changes</span>
              <Button onClick={() => setForm(initial)} disabled={busy}>
                Discard
              </Button>
              <Button type="submit" variant="primary" loading={busy} disabled={!form.name.trim()} kbd="⌘S">
                Save changes
              </Button>
            </div>
          )}
          <Panel title="Danger zone" danger padded={false}>
            <div className={styles.dangerRow}>
              <div>
                <strong>Delete this team</strong>
                <span className={styles.muted}>{subteams > 0 ? `Also deletes ${subteams} child team${subteams === 1 ? '' : 's'}.` : 'Once deleted, it can’t be restored.'}</span>
              </div>
              <Button variant="danger" leadingIcon={TrashIcon} onClick={askDelete}>
                Delete team
              </Button>
            </div>
          </Panel>
        </form>
      </div>
      {confirm.dialog}
    </div>
  );
}

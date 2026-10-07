import { useMemo, useState } from 'react';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount, plural } from '../../components/admin/format';
import { Drawer, PageHeader, RadioCards, SearchInput, StatusPill, errorMessage, useConfirm } from '../../components/admin/kit';
import { invalidateLists, usePagedList } from '../../components/admin/usePagedList';
import { Link, setQuery, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { CheckIcon, LinkExternalIcon, PersonAddIcon, PersonIcon, TrashIcon } from '../../ui/icons';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { collaboratorsPrefix, convertToOutsideCollaborator, membersPath, removeMember, setMembership, type OrgRole } from './api';
import { InviteDialog, RowMenu, useLoadAll, useOrgAccess, userCell } from './common';
import type { SimpleUser } from '../../api/types';

interface Row {
  user: SimpleUser;
  role: OrgRole;
}

const ROLE_TABS = [
  { id: '', label: 'All' },
  { id: 'admin', label: 'Owners' },
  { id: 'member', label: 'Members' },
];

const ROLE_OPTIONS: { value: OrgRole; label: string; description: string }[] = [
  { value: 'member', label: 'Member', description: 'Can see every member and be granted access to repositories.' },
  { value: 'admin', label: 'Owner', description: 'Full administrative rights to the organization and every repository.' },
];

export default function OrgMembersPage() {
  const { org = '' } = useParams<{ org: string }>();
  const query = useQuery();
  const q = query.get('q') ?? '';
  const roleFilter = query.get('role') ?? '';
  const access = useOrgAccess(org);
  const admins = usePagedList<SimpleUser>(membersPath(org, 'admin'));
  const members = usePagedList<SimpleUser>(membersPath(org, 'member'));
  useLoadAll(admins);
  useLoadAll(members);
  const [inviting, setInviting] = useState(false);
  const [open, setOpen] = useState<Row | null>(null);
  const confirm = useConfirm();

  const rows = useMemo(() => {
    const all: Row[] = [...admins.items.map((user) => ({ user, role: 'admin' as const })), ...members.items.map((user) => ({ user, role: 'member' as const }))];
    const needle = q.trim().toLowerCase();
    return all
      .filter((r) => !roleFilter || r.role === roleFilter)
      .filter((r) => !needle || r.user.login.toLowerCase().includes(needle))
      .sort((a, b) => a.user.login.localeCompare(b.user.login, undefined, { sensitivity: 'base' }));
  }, [admins.items, members.items, q, roleFilter]);

  const loading = admins.loading || members.loading;
  const error = admins.error ?? members.error;
  const total = admins.items.length + members.items.length;

  const dropRow = (login: string) => {
    admins.update((items) => items.filter((u) => u.login !== login));
    members.update((items) => items.filter((u) => u.login !== login));
  };

  const changeRole = async (row: Row, role: OrgRole) => {
    if (row.role === role) return;
    const from = row.role === 'admin' ? admins : members;
    const to = role === 'admin' ? admins : members;
    from.update((items) => items.filter((u) => u.id !== row.user.id));
    to.update((items) => [...items, row.user]);
    setOpen((o) => (o && o.user.id === row.user.id ? { ...o, role } : o));
    try {
      await setMembership(org, row.user.login, role);
      toast({ kind: 'success', title: `${row.user.login} is now ${role === 'admin' ? 'an owner' : 'a member'}` });
    } catch (err) {
      to.update((items) => items.filter((u) => u.id !== row.user.id));
      from.update((items) => [...items, row.user]);
      setOpen((o) => (o && o.user.id === row.user.id ? { ...o, role: row.role } : o));
      toast({ kind: 'error', title: `Could not change the role of ${row.user.login}`, description: errorMessage(err) });
    }
  };

  const askRemove = (row: Row) =>
    confirm({
      title: `Remove ${row.user.login} from ${org}?`,
      body: (
        <>
          <strong>{row.user.login}</strong> loses access to every private repository of {org} and is removed from all of its teams. Their forks of private
          repositories are deleted.
        </>
      ),
      confirmLabel: 'Remove member',
      danger: true,
      confirmText: row.user.login,
      onConfirm: async () => {
        await removeMember(org, row.user.login);
        dropRow(row.user.login);
        setOpen(null);
        toast({ kind: 'success', title: `Removed ${row.user.login}` });
      },
    });

  const askConvert = (row: Row) =>
    confirm({
      title: `Convert ${row.user.login} to an outside collaborator?`,
      body: (
        <>
          <strong>{row.user.login}</strong> stops being a member of {org}. Repository access they had through teams is kept as direct collaborator access;
          everything else they could see as a member is lost.
        </>
      ),
      confirmLabel: 'Convert to outside collaborator',
      danger: true,
      onConfirm: async () => {
        await convertToOutsideCollaborator(org, row.user.login);
        dropRow(row.user.login);
        invalidateLists(collaboratorsPrefix(org));
        setOpen(null);
        toast({ kind: 'success', title: `${row.user.login} is now an outside collaborator` });
      },
    });

  const menu = (row: Row) =>
    access.isOwner ? (
      <RowMenu
        label={`Actions for ${row.user.login}`}
        items={[
          { header: 'Role', id: 'h-role' },
          ...ROLE_OPTIONS.map((o) => ({
            id: `role-${o.value}`,
            label: o.label,
            description: o.description,
            trailing: row.role === o.value ? <CheckIcon size={14} /> : undefined,
            onSelect: () => void changeRole(row, o.value),
          })),
          { separator: true, id: 's1' },
          { id: 'convert', label: 'Convert to outside collaborator…', disabled: row.role === 'admin', onSelect: () => askConvert(row) },
          { id: 'remove', label: 'Remove from organization…', danger: true, icon: TrashIcon, onSelect: () => askRemove(row) },
        ]}
      />
    ) : null;

  const columns: Column<Row>[] = [
    { id: 'user', header: 'Member', width: 'minmax(220px, 3fr)', render: (r) => userCell(r.user, undefined, access.me) },
    {
      id: 'role',
      header: 'Role',
      width: '120px',
      render: (r) => (r.role === 'admin' ? <StatusPill status="info">Owner</StatusPill> : <span className={styles.muted}>Member</span>),
    },
    { id: 'type', header: 'Account', width: '96px', hideBelow: 720, render: (r) => (r.user.site_admin ? <StatusPill status="neutral">Site admin</StatusPill> : <span className={styles.subtle}>{r.user.type}</span>) },
    { id: 'actions', header: <span className="visually-hidden">Actions</span>, width: '44px', align: 'end', render: menu },
  ];

  useShortcuts('Organization members', {
    i: { handler: () => access.isOwner && setInviting(true), description: 'Invite member', group: 'Organization' },
  });

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Members"
        description={`People who belong to ${org}. Owners have full administrative access.`}
        actions={
          access.isOwner && (
            <Button variant="primary" leadingIcon={PersonAddIcon} kbd="I" onClick={() => setInviting(true)}>
              Invite member
            </Button>
          )
        }
      />
      <div className={styles.toolbar}>
        <SearchInput label="Find a member" placeholder="Find a member…" value={q} onChange={(v) => setQuery({ q: v || null })} />
        <Tabs
          size="sm"
          items={ROLE_TABS.map((t) => ({ ...t, count: t.id === 'admin' ? admins.items.length : t.id === 'member' ? members.items.length : total }))}
          value={roleFilter}
          onChange={(id) => setQuery({ role: id || null })}
        />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {loading ? 'Loading…' : q ? `${formatCount(rows.length)} of ${formatCount(total)}` : `${plural(total, 'member')}`}
        </span>
      </div>
      <DataTable
        aria-label="Members"
        rows={rows}
        columns={columns}
        getKey={(r) => r.user.id}
        onOpen={(r) => setOpen(r)}
        loading={loading && rows.length === 0}
        empty={
          error ? (
            <EmptyState icon={PersonIcon} title="Could not load members">
              {errorMessage(error)}
            </EmptyState>
          ) : (
            <EmptyState icon={PersonIcon} title={q ? 'No members match' : 'No members'}>
              {q ? 'Try a different search.' : 'Invite people to collaborate in this organization.'}
            </EmptyState>
          )
        }
      />
      <Drawer
        open={!!open}
        onClose={() => setOpen(null)}
        title={open ? open.user.login : 'Member'}
        footer={
          open &&
          access.isOwner && (
            <>
              <Button disabled={open.role === 'admin'} onClick={() => askConvert(open)}>
                Convert to outside collaborator
              </Button>
              <Button variant="danger" leadingIcon={TrashIcon} onClick={() => askRemove(open)}>
                Remove
              </Button>
            </>
          )
        }
      >
        {open && (
          <div className={styles.stack}>
            <div className={styles.listItem} style={{ padding: 0, border: 0 }}>
              <Avatar user={{ login: open.user.login, avatarUrl: open.user.avatar_url }} size={48} />
              <div className={styles.listMain}>
                <strong>{open.user.login}</strong>
                <div className={styles.subtle}>{open.role === 'admin' ? 'Owner' : 'Member'}</div>
              </div>
              <Link to={`/${encodeURIComponent(open.user.login)}`} className={styles.subtle}>
                <LinkExternalIcon size={14} /> Profile
              </Link>
            </div>
            {access.isOwner ? (
              <RadioCards name="member-role" label="Role" value={open.role} onChange={(v) => void changeRole(open, v)} options={ROLE_OPTIONS} />
            ) : (
              <p className={styles.muted}>Only owners can change roles.</p>
            )}
          </div>
        )}
      </Drawer>
      <InviteDialog org={org} open={inviting} onClose={() => setInviting(false)} />
      {confirm.dialog}
    </div>
  );
}


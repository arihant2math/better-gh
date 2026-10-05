import { useMemo, useState } from 'react';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount } from '../../components/admin/format';
import { PageHeader, SearchInput, errorMessage, useConfirm } from '../../components/admin/kit';
import { usePagedList } from '../../components/admin/usePagedList';
import { navigate, setQuery, useParams, useQuery } from '../../router';
import { EmptyState } from '../../ui/EmptyState';
import { InfoIcon, LockIcon, PersonAddIcon, PersonIcon, TrashIcon } from '../../ui/icons';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { collaboratorsPath, isNotAllowed, removeOutsideCollaborator, type SimpleUser } from './api';
import { InviteDialog, RowMenu, useLoadAll, useOrgAccess, userCell } from './common';
import local from './OrgSettings.module.css';

const FILTERS = [
  { id: '', label: 'All' },
  { id: '2fa_disabled', label: '2FA disabled' },
];

export default function OrgCollaboratorsPage() {
  const { org = '' } = useParams<{ org: string }>();
  const query = useQuery();
  const q = query.get('q') ?? '';
  const filter = query.get('filter') ?? '';
  const access = useOrgAccess(org);
  const list = usePagedList<SimpleUser>(collaboratorsPath(org, filter));
  useLoadAll(list);
  const [inviting, setInviting] = useState<string | null>(null);
  const confirm = useConfirm();

  const rows = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return list.items.filter((u) => !needle || u.login.toLowerCase().includes(needle)).sort((a, b) => a.login.localeCompare(b.login, undefined, { sensitivity: 'base' }));
  }, [list.items, q]);

  const askRemove = (u: SimpleUser) =>
    confirm({
      title: `Remove ${u.login} from every repository of ${org}?`,
      body: (
        <>
          <strong>{u.login}</strong> loses access to all of {org}’s repositories, and their pending repository invitations are cancelled.
        </>
      ),
      confirmLabel: 'Remove outside collaborator',
      danger: true,
      confirmText: u.login,
      onConfirm: async () => {
        await removeOutsideCollaborator(org, u.login);
        list.update((items) => items.filter((x) => x.id !== u.id));
        toast({ kind: 'success', title: `Removed ${u.login}` });
      },
    });

  const columns: Column<SimpleUser>[] = [
    { id: 'user', header: 'Outside collaborator', width: 'minmax(220px, 3fr)', render: (u) => userCell(u, u.type === 'Bot' ? 'Bot' : undefined) },
    {
      id: 'actions',
      header: <span className="visually-hidden">Actions</span>,
      width: '44px',
      align: 'end',
      render: (u) =>
        access.isOwner ? (
          <RowMenu
            label={`Actions for ${u.login}`}
            items={[
              { id: 'invite', label: 'Invite to the organization…', icon: PersonAddIcon, description: 'Make them a member instead.', onSelect: () => setInviting(u.login) },
              { separator: true, id: 's' },
              { id: 'remove', label: 'Remove from all repositories…', icon: TrashIcon, danger: true, onSelect: () => askRemove(u) },
            ]}
          />
        ) : null,
    },
  ];

  if (isNotAllowed(list.error)) {
    return (
      <div className={styles.page}>
        <PageHeader title="Outside collaborators" />
        <EmptyState icon={LockIcon} title="You must be a member">
          Only members of <strong>{org}</strong> can see its outside collaborators.
        </EmptyState>
      </div>
    );
  }

  return (
    <div className={styles.fill}>
      <PageHeader title="Outside collaborators" description={`People with access to one or more of ${org}’s repositories who aren’t members of the organization.`} />
      <div className={local.callout}>
        <InfoIcon size={16} />
        <span>
          To make someone an outside collaborator, open a member’s actions on the Members page and choose <strong>Convert to outside collaborator</strong> — their
          team access is kept as direct repository access. Collaborators are added per repository in its settings.
        </span>
      </div>
      <div className={styles.toolbar}>
        <SearchInput label="Find a collaborator" placeholder="Find a collaborator…" value={q} onChange={(v) => setQuery({ q: v || null })} />
        <Tabs size="sm" items={FILTERS} value={filter} onChange={(id) => setQuery({ filter: id || null })} />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {list.done ? `${formatCount(rows.length)} collaborator${rows.length === 1 ? '' : 's'}` : ''}
        </span>
      </div>
      <DataTable
        aria-label="Outside collaborators"
        rows={rows}
        columns={columns}
        getKey={(u) => u.id}
        onOpen={(u) => navigate(`/${encodeURIComponent(u.login)}`)}
        loading={list.loading && rows.length === 0}
        empty={
          list.error ? (
            <EmptyState icon={PersonIcon} title="Could not load outside collaborators">
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={PersonIcon} title={q ? 'No collaborators match' : 'No outside collaborators'}>
              {q ? 'Try a different search.' : `Nobody outside ${org} has access to its repositories.`}
            </EmptyState>
          )
        }
      />
      <InviteDialog org={org} open={!!inviting} onClose={() => setInviting(null)} initialLogin={inviting ?? ''} />
      {confirm.dialog}
    </div>
  );
}

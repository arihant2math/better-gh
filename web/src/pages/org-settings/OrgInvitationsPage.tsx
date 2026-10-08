import { useState } from 'react';
import { useResource } from '../../api/cache';
import { isAccessError } from '../../api/errors';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount, formatDateTime } from '../../components/admin/format';
import { Drawer, KeyValue, PageHeader, StatusPill, errorMessage, useConfirm } from '../../components/admin/kit';
import { usePagedList } from '../../api/usePagedList';
import { Link, setQuery, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Avatar } from '../../ui/Badge';
import { Button, IconButton } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { MailIcon, PersonAddIcon, TrashIcon, XIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { cancelInvitation, failedInvitationsPath, invitationTeams, invitationsPath, type OrgInvitation } from '../../api/orgSettings';
import { INVITE_ROLE_LABEL, InviteDialog, OwnerRequired, teamPath, useLoadAll, useOrgAccess } from './common';

const ROLE_FILTERS = [
  { id: '', label: 'All roles' },
  { id: 'admin', label: 'Owners' },
  { id: 'direct_member', label: 'Members' },
  { id: 'billing_manager', label: 'Billing managers' },
];

const invitee = (i: OrgInvitation) => i.login ?? i.email ?? 'Unknown';

const inviteeCell = (i: OrgInvitation) => (
  <>
    {i.login ? <Avatar user={{ login: i.login, avatarUrl: '' }} size={24} /> : <MailIcon size={16} className={styles.subtle} />}
    <span className={styles.cellMain}>
      <strong>{invitee(i)}</strong>
      <span className={styles.subtle}>{i.login && i.email ? i.email : i.login ? 'Username invitation' : 'Email invitation'}</span>
    </span>
  </>
);

export default function OrgInvitationsPage() {
  const { org = '' } = useParams<{ org: string }>();
  const query = useQuery();
  const tab = query.get('tab') === 'failed' ? 'failed' : 'pending';
  const role = query.get('role') ?? '';
  const access = useOrgAccess(org);
  const pending = usePagedList<OrgInvitation>(tab === 'pending' ? invitationsPath(org, role) : null);
  const failed = usePagedList<OrgInvitation>(tab === 'failed' ? failedInvitationsPath(org) : null);
  const list = tab === 'pending' ? pending : failed;
  useLoadAll(list);
  const [inviting, setInviting] = useState(false);
  const [open, setOpen] = useState<OrgInvitation | null>(null);
  const confirm = useConfirm();

  const askCancel = (i: OrgInvitation) =>
    confirm({
      title: `Cancel the invitation for ${invitee(i)}?`,
      body: 'The invitation link stops working. You can invite them again later.',
      confirmLabel: 'Cancel invitation',
      danger: true,
      onConfirm: async () => {
        await cancelInvitation(org, i.id);
        list.update((items) => items.filter((x) => x.id !== i.id));
        setOpen(null);
        toast({ kind: 'success', title: `Cancelled the invitation for ${invitee(i)}` });
      },
    });

  const columns: Column<OrgInvitation>[] = [
    { id: 'invitee', header: 'Invitee', width: 'minmax(220px, 3fr)', render: inviteeCell },
    {
      id: 'role',
      header: 'Role',
      width: '128px',
      render: (i) => (i.role === 'admin' ? <StatusPill status="info">Owner</StatusPill> : <span className={styles.muted}>{INVITE_ROLE_LABEL[i.role] ?? i.role}</span>),
    },
    {
      id: 'inviter',
      header: 'Invited by',
      width: 'minmax(120px, 1fr)',
      hideBelow: 760,
      render: (i) => (
        <>
          <Avatar user={{ login: i.inviter.login, avatarUrl: i.inviter.avatar_url }} size={18} />
          {i.inviter.login}
        </>
      ),
    },
    { id: 'teams', header: 'Teams', width: '64px', align: 'end', render: (i) => (i.team_count ? formatCount(i.team_count) : <span className={styles.subtle}>—</span>) },
    tab === 'pending'
      ? { id: 'created', header: 'Sent', width: '104px', align: 'end', render: (i) => <RelativeTime date={i.created_at} /> }
      : {
          id: 'failed',
          header: 'Failed',
          width: 'minmax(160px, 1.4fr)',
          render: (i) => (
            <span className={styles.cellMain}>
              <span>{i.failed_reason ?? 'Failed'}</span>
              <span className={styles.subtle}>{i.failed_at ? <RelativeTime date={i.failed_at} /> : ' '}</span>
            </span>
          ),
        },
  ];

  useShortcuts('Organization invitations', {
    i: { handler: () => access.isOwner && setInviting(true), description: 'Invite member', group: 'Organization' },
  });

  if (isAccessError(list.error) && !access.loading && !access.isOwner) {
    return (
      <div className={styles.page}>
        <PageHeader title="Invitations" />
        <OwnerRequired org={org} what="see and manage invitations" />
      </div>
    );
  }

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Invitations"
        description={`Pending and failed invitations to join ${org}. Invitations expire after 7 days.`}
        actions={
          access.isOwner && (
            <Button variant="primary" leadingIcon={PersonAddIcon} kbd="I" onClick={() => setInviting(true)}>
              Invite member
            </Button>
          )
        }
      />
      <div className={styles.toolbar}>
        <Tabs
          size="sm"
          items={[
            { id: 'pending', label: 'Pending' },
            { id: 'failed', label: 'Failed' },
          ]}
          value={tab}
          onChange={(id) => setQuery({ tab: id === 'pending' ? null : id, role: null })}
        />
        {tab === 'pending' && <Tabs size="sm" items={ROLE_FILTERS} value={role} onChange={(id) => setQuery({ role: id || null })} />}
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {list.done ? `${formatCount(list.items.length)} ${tab} invitation${list.items.length === 1 ? '' : 's'}` : ''}
        </span>
      </div>
      <DataTable
        aria-label={tab === 'pending' ? 'Pending invitations' : 'Failed invitations'}
        rows={list.items}
        columns={[
          ...columns,
          {
            id: 'actions',
            header: <span className="visually-hidden">Actions</span>,
            width: '44px',
            align: 'end',
            render: (i) =>
              access.isOwner && tab === 'pending' ? (
                <span onClick={(e) => e.stopPropagation()}>
                  <IconButton icon={XIcon} size="sm" label={`Cancel invitation for ${invitee(i)}`} onClick={() => askCancel(i)} />
                </span>
              ) : null,
          },
        ]}
        getKey={(i) => i.id}
        onOpen={setOpen}
        loading={list.loading && list.items.length === 0}
        empty={
          list.error ? (
            <EmptyState icon={MailIcon} title="Could not load invitations">
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={MailIcon} title={tab === 'pending' ? 'No pending invitations' : 'No failed invitations'}>
              {tab === 'pending' ? 'Invite people by username or email address.' : 'Invitations that expired or bounced show up here.'}
            </EmptyState>
          )
        }
      />
      <Drawer
        open={!!open}
        onClose={() => setOpen(null)}
        title={open ? `Invitation for ${invitee(open)}` : 'Invitation'}
        footer={
          open &&
          access.isOwner &&
          !open.failed_at && (
            <Button variant="danger" leadingIcon={TrashIcon} onClick={() => askCancel(open)}>
              Cancel invitation
            </Button>
          )
        }
      >
        {open && <InvitationDetails org={org} inv={open} />}
      </Drawer>
      <InviteDialog org={org} open={inviting} onClose={() => setInviting(false)} />
      {confirm.dialog}
    </div>
  );
}

function InvitationDetails({ org, inv }: { org: string; inv: OrgInvitation }) {
  const teams = useResource(inv.team_count > 0 ? `org:invitation-teams:${org}/${inv.id}` : null, () => invitationTeams(org, inv.id));
  return (
    <div className={styles.stack}>
      <KeyValue
        items={[
          ['Invitee', inv.login ? <Link to={`/${encodeURIComponent(inv.login)}`}>{inv.login}</Link> : inv.email],
          ...(inv.login && inv.email ? [['Email', inv.email] as [string, string]] : []),
          ['Role', INVITE_ROLE_LABEL[inv.role] ?? inv.role],
          ['Invited by', inv.inviter.login],
          ['Sent', formatDateTime(inv.created_at)],
          ...(inv.failed_at ? [['Failed', `${formatDateTime(inv.failed_at)}${inv.failed_reason ? ` · ${inv.failed_reason}` : ''}`] as [string, string]] : []),
          ...(inv.invitation_source ? [['Source', inv.invitation_source] as [string, string]] : []),
        ]}
      />
      <div>
        <h3 className={styles.panelTitle}>Teams</h3>
        {inv.team_count === 0 ? (
          <p className={styles.muted}>Not added to any team.</p>
        ) : teams.data ? (
          <ul className={styles.list}>
            {teams.data.map((t) => (
              <li key={t.id} className={styles.listItem} style={{ paddingLeft: 0 }}>
                <Link to={teamPath(org, t.slug)} className={styles.listMain}>
                  {t.name}
                </Link>
              </li>
            ))}
          </ul>
        ) : teams.error ? (
          <p className={styles.muted}>{errorMessage(teams.error)}</p>
        ) : (
          <Skeleton width="50%" />
        )}
      </div>
    </div>
  );
}

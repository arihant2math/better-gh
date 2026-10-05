import { useRef, useState, type ReactNode } from 'react';
import { invalidate, mutate, refresh, useResource } from '../../api/cache';
import styles from '../../components/admin/admin.module.css';
import { formatDateTime, formatKb } from '../../components/admin/format';
import { ErrorState, KeyValue, PageHeader, Panel, StatusPill, useConfirm } from '../../components/admin/kit';
import { invalidateLists, updateLists } from '../../components/admin/usePagedList';
import { Link, navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Avatar, Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { GearIcon, KebabHorizontalIcon, LinkExternalIcon, LockIcon, PencilIcon, TrashIcon } from '../../ui/icons';
import { Menu, type MenuEntry } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import d from './AdminDetail.module.css';
import { DetailSkeleton, ItemList, LOGIN_RULE, NotFound, QuotaPanel, RefField, RepoBriefList, isNotFound, isValidLogin, modalOpen, usePrompt } from './detail';
import { deleteOrg, getOrg, updateOrg, type AccountSummary, type OrgDetail } from './api';

const enc = encodeURIComponent;
const orgKey = (login: string) => `admin:org:${login}`;

function syncOrgLists(s: AccountSummary) {
  updateLists<AccountSummary>('/_bgh/admin/orgs', (items) => items.map((o) => (o.id === s.id ? s : o)));
  invalidateLists('/_bgh/admin/orgs');
}

const SETTING_LABELS: Record<string, string> = {
  description: 'Description',
  billing_email: 'Billing email',
  is_verified: 'Verified',
  default_repository_permission: 'Default repository permission',
  members_can_create_repositories: 'Members can create repositories',
  members_can_create_public_repositories: 'Members can create public repositories',
  members_can_create_private_repositories: 'Members can create private repositories',
  members_can_fork_private_repositories: 'Members can fork private repositories',
  two_factor_requirement_enabled: 'Require two-factor authentication',
  has_organization_projects: 'Organization projects',
  has_repository_projects: 'Repository projects',
  archived_at: 'Archived',
};

function settingValue(k: string, v: unknown): ReactNode {
  if (v === null || v === undefined || v === '') return <span className={styles.subtle}>{k === 'archived_at' ? 'No' : 'Not set'}</span>;
  if (typeof v === 'boolean') return v ? 'Yes' : 'No';
  if (k.endsWith('_at') && typeof v === 'string') return formatDateTime(v);
  return typeof v === 'object' ? JSON.stringify(v) : String(v);
}

export default function OrgDetailPage() {
  const { org = '' } = useParams<{ org: string }>();
  const key = orgKey(org);
  const { data, error, loading } = useResource(key, () => getOrg(org));
  if (!data) {
    if (error) {
      if (isNotFound(error)) return <NotFound what="Organization" name={org} back={{ to: '/site-admin/orgs', label: 'Back to organizations' }} />;
      return (
        <div className={styles.page}>
          <ErrorState error={error} onRetry={() => void refresh(key, () => getOrg(org)).catch(() => undefined)} />
        </div>
      );
    }
    if (loading) return <DetailSkeleton />;
    return null;
  }
  return <OrgDetailView key={data.organization.id} login={org} data={data} />;
}

function OrgDetailView({ login, data }: { login: string; data: OrgDetail }) {
  const key = orgKey(login);
  const o = data.organization;
  const settings = data.settings ?? {};
  const archivedAt = typeof settings.archived_at === 'string' ? settings.archived_at : null;
  const archived = !!archivedAt;
  const owners = data.members.filter((m) => m.role === 'admin').length;
  const confirm = useConfirm();
  const prompt = usePrompt();
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);

  const set = (fn: (prev: OrgDetail) => OrgDetail) => mutate<OrgDetail>(key, (prev) => fn(prev ?? data));

  const toggleArchive = () => {
    const next = !archived;
    const apply = async () => {
      const before = data.settings;
      set((p) => ({ ...p, settings: { ...(p.settings ?? {}), archived_at: next ? new Date().toISOString() : null } }));
      try {
        const s = await updateOrg(login, { archived: next });
        set((p) => ({ ...p, organization: s }));
        syncOrgLists(s);
        toast({ kind: 'success', title: next ? `Archived ${o.login}` : `Unarchived ${o.login}` });
      } catch (err) {
        set((p) => ({ ...p, settings: before }));
        throw err;
      }
    };
    confirm({
      title: next ? `Archive ${o.login}?` : `Unarchive ${o.login}?`,
      body: next
        ? 'The organization becomes read-only: members can still read its repositories, but no new repositories, teams or settings changes are allowed. You can unarchive it later.'
        : 'The organization becomes writable again for its members.',
      confirmLabel: next ? 'Archive organization' : 'Unarchive',
      danger: next,
      onConfirm: apply,
    });
  };

  const rename = () =>
    prompt({
      title: `Rename ${o.login}`,
      label: 'New organization name',
      initial: o.login,
      body: 'Repository URLs and git remotes using the old name stop working; the old name becomes available to others.',
      validate: (v) => (isValidLogin(v) ? null : LOGIN_RULE),
      submitLabel: 'Rename organization',
      onSubmit: async (next) => {
        const s = await updateOrg(login, { login: next });
        mutate<OrgDetail>(orgKey(s.login), () => ({ ...data, organization: s }));
        invalidate(key);
        syncOrgLists(s);
        invalidateLists('/_bgh/admin/repos');
        toast({ kind: 'success', title: `Renamed to ${s.login}` });
        navigate(`/site-admin/orgs/${enc(s.login)}`, { replace: true });
      },
    });

  const remove = () => {
    const transfer = { current: '' };
    confirm({
      title: `Delete ${o.login}?`,
      body: (
        <>
          This permanently deletes the organization, its teams and memberships. Its {data.repositories.length} repositor
          {data.repositories.length === 1 ? 'y is' : 'ies are'} deleted unless you transfer them.
        </>
      ),
      extra: <RefField id="org-del-transfer" label="Transfer repositories to (optional)" placeholder="login of a user or organization" into={transfer} />,
      confirmLabel: 'Delete this organization',
      danger: true,
      confirmText: o.login,
      onConfirm: async () => {
        await deleteOrg(login, transfer.current || undefined);
        updateLists<AccountSummary>('/_bgh/admin/orgs', (items) => items.filter((x) => x.id !== o.id));
        invalidateLists('/_bgh/admin/orgs');
        invalidateLists('/_bgh/admin/repos');
        if (transfer.current) {
          invalidate(`admin:user:${transfer.current}`);
          invalidate(orgKey(transfer.current));
        }
        invalidate(key);
        toast({ kind: 'success', title: `Deleted ${o.login}` });
        navigate('/site-admin/orgs');
      },
    });
  };

  const settingsUrl = `/organizations/${enc(o.login)}/settings/profile`;
  const menu: MenuEntry[] = [
    { id: 'settings', label: 'Open organization settings', icon: GearIcon, onSelect: () => navigate(settingsUrl) },
    { id: 'rename', label: 'Rename…', icon: PencilIcon, trailing: 'R', onSelect: rename },
    { id: 'archive', label: archived ? 'Unarchive…' : 'Archive…', icon: LockIcon, trailing: 'A', onSelect: toggleArchive },
    { id: 'sep', separator: true },
    { id: 'delete', label: 'Delete organization…', icon: TrashIcon, danger: true, onSelect: remove },
  ];

  const guard = (fn: () => void) => () => {
    if (modalOpen()) return false;
    fn();
  };
  useShortcuts('Organization', {
    '.': { handler: guard(() => setMenuOpen(true)), description: 'Organization actions menu', group: 'Organization' },
    r: { handler: guard(rename), description: 'Rename organization', group: 'Organization' },
    a: { handler: guard(toggleArchive), description: 'Archive / unarchive organization', group: 'Organization' },
  });

  return (
    <div className={styles.page}>
      <PageHeader
        leading={<Avatar user={{ login: o.login, avatarUrl: o.avatar_url, name: o.name }} size={48} square />}
        title={
          <span className={d.titleRow}>
            {o.login}
            <span className={d.pills}>
              <Tag>Organization</Tag>
              {archived && <StatusPill status="warning">Archived</StatusPill>}
              {o.suspended && <StatusPill status="error">Suspended</StatusPill>}
            </span>
          </span>
        }
        description={o.name || (typeof settings.description === 'string' && settings.description) || 'No display name'}
        actions={
          <>
            <Button size="sm" leadingIcon={GearIcon} onClick={() => navigate(settingsUrl)}>
              Open organization settings
            </Button>
            <Button size="sm" leadingIcon={LinkExternalIcon} onClick={() => navigate(`/${enc(o.login)}`)}>
              Profile
            </Button>
            <Button ref={menuRef} size="sm" leadingIcon={KebabHorizontalIcon} kbd="." aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => setMenuOpen((v) => !v)}>
              Actions
            </Button>
            <Menu open={menuOpen} onClose={() => setMenuOpen(false)} anchor={menuRef} items={menu} placement="bottom-end" aria-label="Organization actions" />
          </>
        }
      />

      {archived && (
        <div className={d.callout} role="status">
          <LockIcon size={16} />
          <div>
            <strong>Archived {archivedAt && <RelativeTime date={archivedAt} />}.</strong> The organization is read-only for its members.
          </div>
        </div>
      )}

      <div className={d.columns}>
        <div className={styles.stack}>
          <Panel title="Summary">
            <KeyValue
              items={[
                ['ID', <span className={styles.mono}>{o.id}</span>],
                ['Created', formatDateTime(o.created_at)],
                ['Updated', formatDateTime(o.updated_at)],
                ['Last active', o.last_active_at ? <RelativeTime date={o.last_active_at} /> : <span className={styles.subtle}>Never</span>],
                ['Members', `${data.members.length} (${owners} owner${owners === 1 ? '' : 's'})`],
                ['Teams', data.teams.length],
                ['Repositories', o.repos_count],
                ['Disk usage', formatKb(o.disk_usage_kb)],
              ]}
            />
          </Panel>

          <Panel title={`Members (${data.members.length})`} padded={false}>
            <ItemList empty="No members." scroll>
              {data.members.map((m) => (
                <li key={m.id} className={styles.listItem}>
                  <Avatar user={{ login: m.login, avatarUrl: '' }} size={20} />
                  <span className={styles.listMain}>
                    <Link to={`/site-admin/users/${enc(m.login)}`} className={d.listLink}>
                      {m.login}
                    </Link>
                  </span>
                  <span className={d.rowMeta}>
                    {m.suspended && <StatusPill status="error">Suspended</StatusPill>}
                    {m.role === 'admin' ? <StatusPill status="info">Owner</StatusPill> : <Tag>{m.role}</Tag>}
                  </span>
                </li>
              ))}
            </ItemList>
          </Panel>

          <Panel title={`Teams (${data.teams.length})`} padded={false}>
            <ItemList empty="No teams." scroll>
              {data.teams.map((t) => (
                <li key={t.id} className={styles.listItem}>
                  <span className={styles.listMain}>
                    <Link to={`/organizations/${enc(o.login)}/settings/teams/${enc(t.slug)}`} className={d.listLink}>
                      {t.name}
                    </Link>{' '}
                    <span className={styles.subtle}>@{t.slug}</span>
                  </span>
                  <span className={d.rowMeta}>
                    {t.members_count} member{t.members_count === 1 ? '' : 's'}
                  </span>
                </li>
              ))}
            </ItemList>
          </Panel>

          <Panel title={`Repositories (${data.repositories.length})`} padded={false}>
            <RepoBriefList owner={o.login} repos={data.repositories} />
          </Panel>

          <Panel title="Settings" actions={<Button size="sm" variant="ghost" onClick={() => navigate(settingsUrl)}>Edit</Button>}>
            {data.settings ? (
              <KeyValue
                items={Object.entries(data.settings)
                  .filter(([k]) => k !== 'org_id')
                  .map(([k, v]) => [SETTING_LABELS[k] ?? k, settingValue(k, v)] as [ReactNode, ReactNode])}
              />
            ) : (
              <span className={styles.subtle}>No settings stored for this organization.</span>
            )}
          </Panel>
        </div>

        <div className={styles.stack}>
          <QuotaPanel login={o.login} quota={data.quota} onChange={(quota) => set((p) => ({ ...p, quota }))} />

          <Panel title="Danger zone" danger padded={false}>
            <div className={styles.dangerRow}>
              <div>
                <strong>Rename organization</strong>
                <span className={styles.subtle}>Old URLs and remotes stop working.</span>
              </div>
              <Button size="sm" variant="danger" onClick={rename}>
                Rename
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>{archived ? 'Unarchive organization' : 'Archive organization'}</strong>
                <span className={styles.subtle}>{archived ? 'Make it writable again.' : 'Make the organization read-only.'}</span>
              </div>
              <Button size="sm" variant={archived ? 'secondary' : 'danger'} kbd="A" onClick={toggleArchive}>
                {archived ? 'Unarchive' : 'Archive'}
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>Delete organization</strong>
                <span className={styles.subtle}>Permanently delete it; optionally transfer its repositories.</span>
              </div>
              <Button size="sm" variant="danger" onClick={remove}>
                Delete
              </Button>
            </div>
          </Panel>
        </div>
      </div>
      {confirm.dialog}
      {prompt.dialog}
    </div>
  );
}

import { useRef, useState } from 'react';
import { invalidate, mutate, refresh, useResource } from '../../api/cache';
import styles from '../../components/admin/admin.module.css';
import { formatDateTime, formatKb } from '../../components/admin/format';
import { CopyButton, ErrorState, KeyValue, PageHeader, Panel, RadioCards, StatusPill, errorMessage, useConfirm } from '../../components/admin/kit';
import { invalidateLists, updateLists } from '../../components/admin/usePagedList';
import { Link, navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Avatar, Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { AlertIcon, KebabHorizontalIcon, KeyIcon, LinkExternalIcon, LockIcon, PencilIcon, PersonIcon, ShieldIcon, SignOutIcon, TrashIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { Menu, type MenuEntry } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import d from './AdminDetail.module.css';
import { createImpersonationToken, deleteImpersonationTokens, deleteUser, disableTwoFactor, getUser, resetPassword, revokeSessions, updateUser, type AccountSummary, type UserDetail } from './api';

import { DetailSkeleton, ItemList, LOGIN_RULE, NotFound, QuotaPanel, RefField, RepoBriefList, isNotFound, isValidLogin, modalOpen, usePrompt } from './detail';

// ------------------------------------------------------------------ user page

const SCOPES: { id: string; description: string }[] = [
  { id: 'repo', description: 'Full control of repositories' },
  { id: 'public_repo', description: 'Public repositories only' },
  { id: 'repo:status', description: 'Commit statuses' },
  { id: 'read:org', description: 'Read org and team membership' },
  { id: 'write:org', description: 'Write org and team membership' },
  { id: 'admin:org', description: 'Full control of orgs and teams' },
  { id: 'user', description: 'Update all user data' },
  { id: 'read:user', description: 'Read user profile data' },
  { id: 'user:email', description: 'Read email addresses' },
  { id: 'gist', description: 'Create gists' },
  { id: 'notifications', description: 'Access notifications' },
  { id: 'workflow', description: 'Update workflow files' },
  { id: 'admin:repo_hook', description: 'Repository hooks' },
  { id: 'admin:public_key', description: 'SSH keys' },
  { id: 'delete_repo', description: 'Delete repositories' },
];

const enc = encodeURIComponent;

const userKey = (login: string) => `admin:user:${login}`;

/** Push a changed account summary into every cached user list. */
function syncUserLists(s: AccountSummary) {
  updateLists<AccountSummary>('/_bgh/admin/users', (items) => items.map((u) => (u.id === s.id ? s : u)));
  invalidateLists('/_bgh/admin/users');
}

export default function UserDetailPage() {
  const { login = '' } = useParams<{ login: string }>();
  const key = userKey(login);
  const { data, error, loading } = useResource(key, () => getUser(login));
  const reload = () => void refresh(key, () => getUser(login)).catch(() => undefined);

  if (!data) {
    if (error) {
      if (isNotFound(error)) return <NotFound what="User" name={login} back={{ to: '/site-admin/users', label: 'Back to users' }} />;
      return (
        <div className={styles.page}>
          <ErrorState error={error} onRetry={reload} />
        </div>
      );
    }
    if (loading) return <DetailSkeleton />;
    return null;
  }
  return <UserDetailView key={data.user.id} login={login} data={data} />;
}

function UserDetailView({ login, data }: { login: string; data: UserDetail }) {
  const key = userKey(login);
  const u = data.user;
  const confirm = useConfirm();
  const prompt = usePrompt();
  const [menuOpen, setMenuOpen] = useState(false);
  const [passwordOpen, setPasswordOpen] = useState(false);
  const [impersonateOpen, setImpersonateOpen] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const impersonationTokens = data.tokens.filter((t) => t.kind === 'impersonation');

  const set = (fn: (prev: UserDetail) => UserDetail) => mutate<UserDetail>(key, (prev) => fn(prev ?? data));

  /** PATCH the user; optimistic for the given fields, rolled back on failure. */
  const patch = async (body: Parameters<typeof updateUser>[1], optimistic: Partial<AccountSummary>) => {
    const before = u;
    set((p) => ({ ...p, user: { ...p.user, ...optimistic } }));
    try {
      const s = await updateUser(login, body);
      set((p) => ({ ...p, user: s }));
      syncUserLists(s);
      return s;
    } catch (err) {
      set((p) => ({ ...p, user: before }));
      throw err;
    }
  };

  const toggleSuspend = () => {
    if (u.suspended)
      confirm({
        title: `Unsuspend ${u.login}?`,
        body: 'They will be able to sign in, push and use the API again.',
        confirmLabel: 'Unsuspend',
        onConfirm: () => patch({ suspended: false }, { suspended: false, suspended_at: null, suspended_reason: null }),
      });
    else
      confirm({
        title: `Suspend ${u.login}?`,
        body: (
          <>
            Suspended users can't sign in, push or use the API; their sessions are ended immediately. Their content stays visible. Site administrators and your own
            account can't be suspended.
          </>
        ),
        confirmLabel: 'Suspend user',
        danger: true,
        reason: { label: 'Reason (shown to the user and recorded in the audit log)', required: true, placeholder: 'e.g. Spam, abuse report #123…' },
        onConfirm: (reason) =>
          patch({ suspended: true, suspended_reason: reason }, { suspended: true, suspended_at: new Date().toISOString(), suspended_reason: reason }).then(() =>
            set((p) => ({ ...p, sessions: { ...p.sessions, active: 0 } })),
          ),
      });
  };

  const toggleAdmin = () =>
    confirm({
      title: u.site_admin ? `Remove site administrator from ${u.login}?` : `Make ${u.login} a site administrator?`,
      body: u.site_admin
        ? 'They lose access to site admin and to repositories and organizations they are not a member of. The last site administrator can’t be demoted.'
        : 'Site administrators can manage every user, organization and repository and change instance settings.',
      confirmLabel: u.site_admin ? 'Demote' : 'Promote to site admin',
      danger: u.site_admin,
      onConfirm: () => patch({ site_admin: !u.site_admin }, { site_admin: !u.site_admin }),
    });

  const rename = () =>
    prompt({
      title: `Rename ${u.login}`,
      label: 'New username',
      initial: u.login,
      body: 'Old URLs, git remotes and @mentions using the previous name stop working; the old name becomes available to others.',
      validate: (v) => (isValidLogin(v) ? null : LOGIN_RULE),
      submitLabel: 'Rename user',
      onSubmit: async (next) => {
        const s = await updateUser(login, { login: next });
        mutate<UserDetail>(userKey(s.login), () => ({ ...data, user: s }));
        invalidate(key);
        syncUserLists(s);
        toast({ kind: 'success', title: `Renamed to ${s.login}` });
        navigate(`/site-admin/users/${enc(s.login)}`, { replace: true });
      },
    });

  const disable2fa = () =>
    confirm({
      title: `Disable two-factor authentication for ${u.login}?`,
      body: 'Use this when the user lost their second factor. Their recovery codes are discarded; they can enroll again after signing in.',
      confirmLabel: 'Disable 2FA',
      danger: true,
      onConfirm: async () => {
        await disableTwoFactor(login);
        const user = { ...u, two_factor_enabled: false };
        set((p) => ({ ...p, user, two_factor: { enabled: false, enabled_at: null } }));
        syncUserLists(user);
      },
    });

  const signOutEverywhere = () =>
    confirm({
      title: `Sign ${u.login} out everywhere?`,
      body: `Ends all ${data.sessions.active} active web session${data.sessions.active === 1 ? '' : 's'}. Access tokens and SSH keys keep working.`,
      confirmLabel: 'Sign out everywhere',
      danger: true,
      onConfirm: async () => {
        await revokeSessions(login);
        set((p) => ({ ...p, sessions: { ...p.sessions, active: 0 } }));
        toast({ kind: 'success', title: 'All sessions ended' });
      },
    });

  const revokeImpersonation = () =>
    confirm({
      title: 'Revoke impersonation tokens?',
      body: `Revokes ${impersonationTokens.length} impersonation token${impersonationTokens.length === 1 ? '' : 's'} created for ${u.login} by site administrators.`,
      confirmLabel: 'Revoke all',
      danger: true,
      onConfirm: async () => {
        await deleteImpersonationTokens(login);
        set((p) => ({ ...p, tokens: p.tokens.filter((t) => t.kind !== 'impersonation') }));
        toast({ kind: 'success', title: 'Impersonation tokens revoked' });
      },
    });

  const remove = () => {
    const transfer = { current: '' };
    confirm({
      title: `Delete ${u.login}?`,
      body: (
        <>
          This permanently deletes the account, its keys, tokens and memberships. Issues, comments and reviews they wrote stay, attributed to{' '}
          <strong>ghost</strong>. Owned repositories ({data.repositories.length}) are deleted unless you transfer them.
        </>
      ),
      extra: <RefField id="del-transfer" label="Transfer repositories to (optional)" placeholder="login of a user or organization" into={transfer} />,
      confirmLabel: 'Delete this user',
      danger: true,
      confirmText: u.login,
      onConfirm: async () => {
        await deleteUser(login, transfer.current || undefined);
        updateLists<AccountSummary>('/_bgh/admin/users', (items) => items.filter((x) => x.id !== u.id));
        invalidateLists('/_bgh/admin/users');
        if (transfer.current) {
          invalidate(userKey(transfer.current));
          invalidate(`admin:org:${transfer.current}`);
          invalidateLists('/_bgh/admin/repos');
        }
        invalidate(key);
        toast({ kind: 'success', title: `Deleted ${u.login}` });
        navigate('/site-admin/users');
      },
    });
  };

  const menu: MenuEntry[] = [
    { id: 'suspend', label: u.suspended ? 'Unsuspend…' : 'Suspend…', icon: LockIcon, trailing: 'S', onSelect: toggleSuspend },
    { id: 'admin', label: u.site_admin ? 'Remove site admin…' : 'Make site admin…', icon: ShieldIcon, onSelect: toggleAdmin },
    { id: 'rename', label: 'Rename…', icon: PencilIcon, trailing: 'R', onSelect: rename },
    { id: 'password', label: 'Reset password…', icon: KeyIcon, trailing: 'P', onSelect: () => setPasswordOpen(true) },
    ...(data.two_factor.enabled ? [{ id: '2fa', label: 'Disable two-factor…', icon: ShieldIcon, onSelect: disable2fa }] : []),
    ...(data.sessions.active > 0 ? [{ id: 'sessions', label: 'Sign out everywhere…', icon: SignOutIcon, onSelect: signOutEverywhere }] : []),
    { id: 'sep1', separator: true },
    { id: 'impersonate', label: 'Create impersonation token…', icon: PersonIcon, trailing: 'I', onSelect: () => setImpersonateOpen(true) },
    ...(impersonationTokens.length > 0 ? [{ id: 'revoke-imp', label: 'Revoke impersonation tokens…', icon: KeyIcon, onSelect: revokeImpersonation }] : []),
    { id: 'sep2', separator: true },
    { id: 'delete', label: 'Delete user…', icon: TrashIcon, danger: true, onSelect: remove },
  ];

  const guard = (fn: () => void) => () => {
    if (modalOpen()) return false;
    fn();
  };
  useShortcuts('User', {
    '.': { handler: guard(() => setMenuOpen(true)), description: 'User actions menu', group: 'User' },
    s: { handler: guard(toggleSuspend), description: 'Suspend / unsuspend user', group: 'User' },
    r: { handler: guard(rename), description: 'Rename user', group: 'User' },
    p: { handler: guard(() => setPasswordOpen(true)), description: 'Reset password', group: 'User' },
    i: { handler: guard(() => setImpersonateOpen(true)), description: 'Create impersonation token', group: 'User' },
  });

  return (
    <div className={styles.page}>
      <PageHeader
        leading={<Avatar user={{ login: u.login, avatarUrl: u.avatar_url, name: u.name }} size={48} />}
        title={
          <span className={d.titleRow}>
            {u.login}
            <span className={d.pills}>
              {u.type === 'Bot' && <Tag>Bot</Tag>}
              {u.site_admin && <StatusPill status="info">Site admin</StatusPill>}
              {u.suspended && <StatusPill status="error">Suspended</StatusPill>}
              {u.two_factor_enabled ? <StatusPill status="ok">2FA</StatusPill> : <StatusPill status="neutral">No 2FA</StatusPill>}
            </span>
          </span>
        }
        description={[u.name, u.email].filter(Boolean).join(' · ') || 'No name or public email'}
        actions={
          <>
            <Button size="sm" leadingIcon={LinkExternalIcon} onClick={() => navigate(`/${enc(u.login)}`)}>
              Profile
            </Button>
            <Button ref={menuRef} size="sm" leadingIcon={KebabHorizontalIcon} kbd="." aria-haspopup="menu" aria-expanded={menuOpen} onClick={() => setMenuOpen((o) => !o)}>
              Actions
            </Button>
            <Menu open={menuOpen} onClose={() => setMenuOpen(false)} anchor={menuRef} items={menu} placement="bottom-end" aria-label="User actions" />
          </>
        }
      />

      {u.suspended && (
        <div className={`${d.callout} ${d.calloutDanger}`} role="status">
          <LockIcon size={16} />
          <div>
            <strong>Suspended {u.suspended_at ? <RelativeTime date={u.suspended_at} /> : ''}.</strong>{' '}
            {u.suspended_reason ? <>Reason: {u.suspended_reason}</> : 'No reason recorded.'}
          </div>
        </div>
      )}

      <div className={d.columns}>
        <div className={styles.stack}>
          <Panel title="Profile">
            <KeyValue
              items={[
                ['ID', <span className={styles.mono}>{u.id}</span>],
                ['Type', u.type],
                ['Email', u.email ?? <span className={styles.subtle}>None</span>],
                ['Created', <span title={formatDateTime(u.created_at)}>{formatDateTime(u.created_at)}</span>],
                ['Updated', formatDateTime(u.updated_at)],
                ['Last active', u.last_active_at ? <RelativeTime date={u.last_active_at} /> : <span className={styles.subtle}>Never</span>],
                ['Repositories', u.repos_count],
                ['Disk usage', formatKb(u.disk_usage_kb)],
              ]}
            />
          </Panel>

          <Panel title={`Repositories (${data.repositories.length})`} padded={false}>
            <RepoBriefList owner={u.login} repos={data.repositories} />
          </Panel>

          <Panel title={`Organizations (${data.organizations.length})`} padded={false}>
            <ItemList empty="Not a member of any organization.">
              {data.organizations.map((o) => (
                <li key={o.id} className={styles.listItem}>
                  <Avatar user={{ login: o.login, avatarUrl: '' }} size={20} square />
                  <span className={styles.listMain}>
                    <Link to={`/site-admin/orgs/${enc(o.login)}`} className={d.listLink}>
                      {o.login}
                    </Link>
                  </span>
                  <span className={d.rowMeta}>{o.role === 'admin' ? <StatusPill status="info">Owner</StatusPill> : <Tag>{o.role}</Tag>}</span>
                </li>
              ))}
            </ItemList>
          </Panel>

          <Panel title="Emails" padded={false}>
            <ItemList empty="No email addresses.">
              {data.emails.map((e) => (
                <li key={e.email} className={styles.listItem}>
                  <span className={styles.listMain}>{e.email}</span>
                  <span className={d.rowMeta}>
                    {e.primary && <StatusPill status="info">Primary</StatusPill>}
                    {e.verified ? <StatusPill status="ok">Verified</StatusPill> : <StatusPill status="warning">Unverified</StatusPill>}
                    {e.visibility && <span>{e.visibility}</span>}
                  </span>
                </li>
              ))}
            </ItemList>
          </Panel>

          <Panel title="SSH keys" padded={false}>
            <ItemList empty="No SSH keys.">
              {data.ssh_keys.map((k) => (
                <li key={k.id} className={styles.listItem}>
                  <KeyIcon size={16} />
                  <span className={styles.listMain}>
                    <strong>{k.title}</strong>
                    <br />
                    <span className={styles.mono}>{k.fingerprint}</span>
                  </span>
                  <span className={d.rowMeta}>
                    {k.last_used_at ? (
                      <>
                        Last used <RelativeTime date={k.last_used_at} />
                      </>
                    ) : (
                      'Never used'
                    )}
                  </span>
                </li>
              ))}
            </ItemList>
          </Panel>

          <Panel title="GPG keys" padded={false}>
            <ItemList empty="No GPG keys.">
              {data.gpg_keys.map((k) => {
                const expired = !!k.expires_at && new Date(k.expires_at).getTime() < Date.now();
                return (
                  <li key={k.id} className={styles.listItem}>
                    <span className={`${styles.listMain} ${styles.mono}`}>{k.key_id}</span>
                    <span className={d.rowMeta}>
                      {k.expires_at ? (
                        expired ? (
                          <StatusPill status="warning">Expired {formatDateTime(k.expires_at)}</StatusPill>
                        ) : (
                          <>Expires {formatDateTime(k.expires_at)}</>
                        )
                      ) : (
                        'No expiry'
                      )}
                    </span>
                  </li>
                );
              })}
            </ItemList>
          </Panel>

          <Panel
            title="Access tokens"
            padded={false}
            actions={
              <>
                {impersonationTokens.length > 0 && (
                  <Button size="sm" variant="ghost" onClick={revokeImpersonation}>
                    Revoke impersonation
                  </Button>
                )}
                <Button size="sm" onClick={() => setImpersonateOpen(true)} kbd="I">
                  Impersonate
                </Button>
              </>
            }
          >
            <ItemList empty="No access tokens.">
              {data.tokens.map((t) => {
                const expired = !!t.expires_at && new Date(t.expires_at).getTime() < Date.now();
                return (
                  <li key={t.id} className={styles.listItem}>
                    <span className={styles.listMain}>
                      <span className={d.inline}>
                        <strong>{t.name || 'Unnamed token'}</strong>
                        {t.kind === 'impersonation' ? <StatusPill status="warning">Impersonation</StatusPill> : <Tag>{t.kind}</Tag>}
                        {t.token_last_eight && <span className={styles.mono}>…{t.token_last_eight}</span>}
                      </span>
                      <span className={styles.subtle}>{t.scopes.length ? t.scopes.join(', ') : 'No scopes'}</span>
                    </span>
                    <span className={d.rowMeta} style={{ flexDirection: 'column', alignItems: 'flex-end', gap: 0 }}>
                      <span>{t.last_used_at ? <>Used <RelativeTime date={t.last_used_at} /></> : 'Never used'}</span>
                      <span>
                        {t.expires_at ? (
                          expired ? (
                            <StatusPill status="error">Expired</StatusPill>
                          ) : (
                            <>Expires {formatDateTime(t.expires_at)}</>
                          )
                        ) : (
                          'No expiry'
                        )}
                      </span>
                    </span>
                  </li>
                );
              })}
            </ItemList>
          </Panel>
        </div>

        <div className={styles.stack}>
          <Panel title="Security">
            <KeyValue
              items={[
                [
                  'Two-factor',
                  data.two_factor.enabled ? (
                    <span className={d.inline}>
                      <StatusPill status="ok">Enabled</StatusPill>
                      {data.two_factor.enabled_at && <span className={styles.subtle}>since {formatDateTime(data.two_factor.enabled_at)}</span>}
                    </span>
                  ) : (
                    <StatusPill status="warning">Not enabled</StatusPill>
                  ),
                ],
                ['Active sessions', data.sessions.active],
                ['Last seen', data.sessions.last_seen_at ? <RelativeTime date={data.sessions.last_seen_at} /> : <span className={styles.subtle}>Never</span>],
              ]}
            />
            <div className={d.inline} style={{ marginTop: 12 }}>
              <Button size="sm" leadingIcon={SignOutIcon} disabled={data.sessions.active === 0} onClick={signOutEverywhere}>
                Sign out everywhere
              </Button>
              {data.two_factor.enabled && (
                <Button size="sm" variant="danger" onClick={disable2fa}>
                  Disable 2FA
                </Button>
              )}
            </div>
          </Panel>

          <QuotaPanel login={u.login} quota={data.quota} onChange={(quota) => set((p) => ({ ...p, quota }))} />

          <Panel title="Admin actions" padded={false}>
            <div className={styles.dangerRow}>
              <div>
                <strong>{u.site_admin ? 'Site administrator' : 'Regular user'}</strong>
                <span className={styles.subtle}>{u.site_admin ? 'Has full access to site admin.' : 'Grant full access to site admin.'}</span>
              </div>
              <Button size="sm" onClick={toggleAdmin}>
                {u.site_admin ? 'Demote' : 'Promote'}
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>Password</strong>
                <span className={styles.subtle}>Generate or set a new password; signs the user out.</span>
              </div>
              <Button size="sm" onClick={() => setPasswordOpen(true)}>
                Reset…
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>Impersonation</strong>
                <span className={styles.subtle}>Act as this user via the API with a scoped token.</span>
              </div>
              <Button size="sm" onClick={() => setImpersonateOpen(true)}>
                Create token…
              </Button>
            </div>
          </Panel>

          <Panel title="Danger zone" danger padded={false}>
            <div className={styles.dangerRow}>
              <div>
                <strong>{u.suspended ? 'Unsuspend user' : 'Suspend user'}</strong>
                <span className={styles.subtle}>{u.suspended ? 'Restore access to this account.' : 'Block sign-in, git and API access.'}</span>
              </div>
              <Button size="sm" variant={u.suspended ? 'secondary' : 'danger'} kbd="S" onClick={toggleSuspend}>
                {u.suspended ? 'Unsuspend' : 'Suspend'}
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>Rename user</strong>
                <span className={styles.subtle}>Old URLs and remotes stop working.</span>
              </div>
              <Button size="sm" variant="danger" onClick={rename}>
                Rename
              </Button>
            </div>
            <div className={styles.dangerRow}>
              <div>
                <strong>Delete user</strong>
                <span className={styles.subtle}>Permanently delete the account; optionally transfer its repositories.</span>
              </div>
              <Button size="sm" variant="danger" onClick={remove}>
                Delete
              </Button>
            </div>
          </Panel>
        </div>
      </div>

      <PasswordDialog
        open={passwordOpen}
        login={u.login}
        onClose={() => setPasswordOpen(false)}
        onDone={() => set((p) => ({ ...p, sessions: { ...p.sessions, active: 0 } }))}
      />
      <ImpersonateDialog
        open={impersonateOpen}
        login={u.login}
        onClose={() => setImpersonateOpen(false)}
        onCreated={() => void refresh(key, () => getUser(login)).catch(() => undefined)}
      />
      {confirm.dialog}
      {prompt.dialog}
    </div>
  );
}

// ------------------------------------------------------------------ password reset

function PasswordDialog({ open, login, onClose, onDone }: { open: boolean; login: string; onClose: () => void; onDone: () => void }) {
  const [mode, setMode] = useState<'generate' | 'set'>('generate');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<string | null | undefined>(undefined);
  const close = () => {
    onClose();
    setMode('generate');
    setPassword('');
    setError(null);
    setResult(undefined);
  };
  const tooShort = mode === 'set' && password.length > 0 && password.length < 8;
  const blocked = mode === 'set' && password.length < 8;
  const submit = async () => {
    if (blocked || busy) return;
    setBusy(true);
    setError(null);
    try {
      const r = await resetPassword(login, mode === 'set' ? password : undefined);
      setResult(r.password);
      setPassword('');
      onDone();
      if (!r.password) toast({ kind: 'success', title: `Password set for ${login}` });
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  const done = result !== undefined;
  return (
    <Dialog
      open={open}
      onClose={close}
      title={`Reset password for ${login}`}
      footer={
        done ? (
          <Button variant="primary" onClick={close}>
            Done
          </Button>
        ) : (
          <>
            <Button onClick={close}>Cancel</Button>
            <Button variant="danger" loading={busy} disabled={blocked} onClick={() => void submit()}>
              {mode === 'generate' ? 'Generate password' : 'Set password'}
            </Button>
          </>
        )
      }
    >
      {done ? (
        <div className={styles.form}>
          {result ? (
            <>
              <p className={styles.confirmBody}>Share this temporary password with {login} over a secure channel. It won’t be shown again.</p>
              <div className={d.secret}>
                <code>{result}</code>
                <CopyButton text={result} label="Copy password" />
              </div>
            </>
          ) : (
            <p className={styles.confirmBody}>The new password is set.</p>
          )}
          <p className={styles.subtle}>{login} has been signed out of every session.</p>
        </div>
      ) : (
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <p className={styles.confirmBody}>The user is signed out everywhere. The change is recorded in the audit log.</p>
          <RadioCards
            name="pw-mode"
            label="Password"
            value={mode}
            onChange={setMode}
            options={[
              { value: 'generate', label: 'Generate', description: 'A random temporary password, shown once.' },
              { value: 'set', label: 'Set a password', description: 'Type the new password yourself.' },
            ]}
          />
          {mode === 'set' && (
            <Field label="New password" htmlFor="pw-new" error={tooShort ? 'At least 8 characters.' : null}>
              <Input id="pw-new" type="password" autoComplete="new-password" value={password} onChange={(e) => setPassword(e.target.value)} autoFocus invalid={tooShort} />
            </Field>
          )}
          {error && (
            <div className={styles.formError} role="alert">
              <AlertIcon size={14} /> {error}
            </div>
          )}
          <button type="submit" hidden />
        </form>
      )}
    </Dialog>
  );
}

// ------------------------------------------------------------------ impersonation

function ImpersonateDialog({ open, login, onClose, onCreated }: { open: boolean; login: string; onClose: () => void; onCreated: () => void }) {
  const [scopes, setScopes] = useState<string[]>(['repo', 'read:org']);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [token, setToken] = useState<string | null | undefined>(undefined);
  const close = () => {
    onClose();
    setError(null);
    setToken(undefined);
  };
  const toggle = (id: string, on: boolean) => setScopes((s) => (on ? [...s, id] : s.filter((x) => x !== id)));
  const submit = async () => {
    if (busy || scopes.length === 0) return;
    setBusy(true);
    setError(null);
    try {
      const t = await createImpersonationToken(login, scopes);
      setToken(t.token || null);
      onCreated();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  const done = token !== undefined;
  return (
    <Dialog
      open={open}
      onClose={close}
      title={`Impersonate ${login}`}
      footer={
        done ? (
          <Button variant="primary" onClick={close}>
            Done
          </Button>
        ) : (
          <>
            <Button onClick={close}>Cancel</Button>
            <Button variant="primary" loading={busy} disabled={scopes.length === 0} onClick={() => void submit()}>
              Create token
            </Button>
          </>
        )
      }
    >
      <div className={styles.form}>
        <div className={d.callout} style={{ margin: 0 }}>
          <AlertIcon size={16} />
          <span>Creating and using an impersonation token is recorded in the audit log, including your account and IP address.</span>
        </div>
        {done ? (
          token ? (
            <>
              <p className={styles.confirmBody}>Copy the token now — it won’t be shown again. Scopes: {scopes.join(', ')}.</p>
              <div className={d.secret}>
                <code>{token}</code>
                <CopyButton text={token} label="Copy token" />
              </div>
            </>
          ) : (
            <p className={styles.confirmBody}>
              An impersonation token with exactly these scopes already exists for {login}, so no new token was issued. Revoke the existing impersonation tokens to
              create a fresh one.
            </p>
          )
        ) : (
          <form
            className={styles.form}
            onSubmit={(e) => {
              e.preventDefault();
              void submit();
            }}
          >
            <fieldset className={d.scopes}>
              <legend>Scopes</legend>
              {SCOPES.map((s) => (
                <label key={s.id} className={d.check}>
                  <input type="checkbox" checked={scopes.includes(s.id)} onChange={(e) => toggle(s.id, e.target.checked)} />
                  <span>
                    <span className={styles.mono}>{s.id}</span>
                    <small>{s.description}</small>
                  </span>
                </label>
              ))}
            </fieldset>
            {scopes.length === 0 && <span className={styles.subtle}>Pick at least one scope.</span>}
            {error && (
              <div className={styles.formError} role="alert">
                <AlertIcon size={14} /> {error}
              </div>
            )}
            <button type="submit" hidden />
          </form>
        )}
      </div>
    </Dialog>
  );
}

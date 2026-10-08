import { useState, type ReactNode } from 'react';
import { ApiError } from '../../api/client';
import { refresh, useResource } from '../../api/cache';
import { createToken, type AccessToken } from '../../api/developerSettings';
import styles from '../../components/admin/admin.module.css';
import { formatDateTime, plural } from '../../components/admin/format';
import { CopyButton, ErrorState, PageHeader, Panel, SearchInput, StatusPill, errorMessage } from '../../components/admin/kit';
import { Link } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { Skeleton } from '../../ui/EmptyState';
import { AlertIcon, ChevronLeftIcon, ChevronRightIcon, KeyIcon } from '../../ui/icons';
import { Field, Input, Select } from '../../ui/Input';
import { getSettings, listScim, scimBase, scimPath, scimUserNameFilter, type ScimGroup, type ScimUser } from '../../api/admin';
import d from './AdminDetail.module.css';
import sc from './scim.module.css';

const PAGE = 25;
/** Shared with the settings page's cache entry. */
const SETTINGS_KEY = 'admin:settings';

const primaryEmail = (u: ScimUser) => (u.emails?.find((e) => e.primary) ?? u.emails?.[0])?.value ?? null;

/** `/site-admin/scim`: endpoint, tokens and what the identity provider has provisioned. */
export default function ScimPage() {
  const settings = useResource(SETTINGS_KEY, getSettings);
  const [tokenOpen, setTokenOpen] = useState(false);
  useShortcuts('SCIM provisioning', {
    c: { handler: () => setTokenOpen(true), description: 'Generate SCIM token', group: 'SCIM' },
  });
  const enabled = settings.data?.auth_providers.scim?.enabled;
  const endpoint = `${location.origin}${scimBase()}`;
  return (
    <div className={styles.page}>
      <PageHeader
        title="SCIM provisioning"
        description="Your identity provider creates, updates and deprovisions accounts and groups through SCIM 2.0; groups drive the members of mapped teams."
        actions={
          <Button variant="primary" leadingIcon={KeyIcon} kbd="c" onClick={() => setTokenOpen(true)}>
            Generate SCIM token
          </Button>
        }
      />
      <div className={styles.stack}>
        <Panel title="Endpoint">
          <div className={sc.status}>
            {enabled === undefined ? (
              settings.error ? (
                <span className={styles.subtle}>Could not load the settings: {errorMessage(settings.error)}</span>
              ) : (
                <Skeleton width={160} />
              )
            ) : enabled ? (
              <StatusPill status="ok">Enabled</StatusPill>
            ) : (
              <>
                <StatusPill status="neutral">Disabled</StatusPill>
                <span className={styles.subtle}>
                  Turn on <Link to="/site-admin/settings#authentication">SCIM provisioning</Link> in the authentication settings.
                </span>
              </>
            )}
          </div>
          <div className={d.secret}>
            <code>{endpoint}</code>
            <CopyButton text={endpoint} label="Copy SCIM endpoint" />
          </div>
          <p className={sc.help}>
            Configure the identity provider with this base URL and a token with the <span className={styles.mono}>scim:enterprise</span> scope (Bearer
            authentication). Organization owners can provision memberships at <span className={styles.mono}>/api/v3/scim/v2/organizations/&lt;org&gt;/</span>{' '}
            with an <span className={styles.mono}>admin:org</span> token.
          </p>
        </Panel>
        {enabled !== false && (
          <>
            <UsersPanel />
            <GroupsPanel />
          </>
        )}
      </div>
      <TokenDialog open={tokenOpen} onClose={() => setTokenOpen(false)} />
    </div>
  );
}

// ------------------------------------------------------------------ lists

function useScimPage<T>(resource: 'Users' | 'Groups', startIndex: number, filter?: string) {
  const path = scimPath(resource, { startIndex, count: PAGE, filter });
  const loader = () => listScim<T>(path);
  return { ...useResource(path, loader, { ttlMs: 10_000 }), reload: () => void refresh(path, loader).catch(() => undefined) };
}

/** Header + rows of a simple (non-virtualized) grid table. */
function Table({ columns, header, children }: { columns: string; header: ReactNode[]; children: ReactNode }) {
  return (
    <div className={sc.table}>
      <div className={styles.thead} style={{ gridTemplateColumns: columns }} role="row">
        {header.map((h, i) => (
          <span key={i} className={styles.th} role="columnheader">
            {h}
          </span>
        ))}
      </div>
      {children}
    </div>
  );
}

function Pager({ start, shown, total, onPage }: { start: number; shown: number; total: number; onPage: (start: number) => void }) {
  return (
    <div className={styles.tfoot}>
      <span>{total === 0 ? 'None' : `${start}–${start + shown - 1} of ${total}`}</span>
      <span className={styles.toolbarSpacer} />
      <Button size="sm" variant="ghost" leadingIcon={ChevronLeftIcon} disabled={start <= 1} onClick={() => onPage(Math.max(1, start - PAGE))}>
        Previous
      </Button>
      <Button size="sm" variant="ghost" disabled={start + shown > total || shown === 0} onClick={() => onPage(start + PAGE)}>
        Next <ChevronRightIcon size={16} />
      </Button>
    </div>
  );
}

function ListState({ error, loading, empty, onRetry }: { error: unknown; loading: boolean; empty: string | null; onRetry: () => void }) {
  if (error) {
    if (error instanceof ApiError && error.status === 404) return <div className={sc.empty}>SCIM provisioning is disabled.</div>;
    return <ErrorState error={error} onRetry={onRetry} title="Could not load the list" />;
  }
  if (loading)
    return (
      <div className={sc.empty}>
        <Skeleton width="60%" />
      </div>
    );
  return empty ? <div className={sc.empty}>{empty}</div> : null;
}

const USER_COLUMNS = 'minmax(140px, 1.2fr) minmax(120px, 1fr) minmax(160px, 1.4fr) 90px 150px';

function UsersPanel() {
  const [start, setStart] = useState(1);
  const [query, setQuery] = useState('');
  const filter = query.trim() ? scimUserNameFilter(query.trim()) : undefined;
  const page = useScimPage<ScimUser>('Users', start, filter);
  const list = page.data;
  return (
    <Panel
      title={list ? `Provisioned users · ${list.totalResults}` : 'Provisioned users'}
      padded={false}
      actions={
        <SearchInput
          label="Filter by userName"
          placeholder="Exact userName"
          value={query}
          width={220}
          onChange={(v) => {
            setQuery(v);
            setStart(1);
          }}
        />
      }
    >
      <Table columns={USER_COLUMNS} header={['userName', 'Display name', 'Primary email', 'Status', 'Created']}>
        {list?.Resources.map((u) => (
          <div key={u.id} className={styles.tr} style={{ gridTemplateColumns: USER_COLUMNS }} role="row">
            <span className={styles.td}>
              <span className={styles.cellMain}>
                <span className={styles.mono}>{u.userName}</span>
                {u.externalId && <span className={styles.subtle}>{u.externalId}</span>}
              </span>
            </span>
            <span className={styles.td}>{u.displayName ?? u.name?.formatted ?? <span className={styles.subtle}>—</span>}</span>
            <span className={styles.td}>{primaryEmail(u) ?? <span className={styles.subtle}>—</span>}</span>
            <span className={styles.td}>{u.active ? <StatusPill status="ok">Active</StatusPill> : <StatusPill status="warning">Inactive</StatusPill>}</span>
            <span className={`${styles.td} ${styles.subtle}`}>{formatDateTime(u.meta.created)}</span>
          </div>
        ))}
        <ListState
          error={page.error}
          loading={!list && page.loading}
          empty={list && list.Resources.length === 0 ? (filter ? `No user named “${query.trim()}”.` : 'No users provisioned yet.') : null}
          onRetry={page.reload}
        />
      </Table>
      {list && <Pager start={list.startIndex} shown={list.Resources.length} total={list.totalResults} onPage={setStart} />}
    </Panel>
  );
}

const GROUP_COLUMNS = 'minmax(160px, 2fr) minmax(120px, 1fr) 110px 150px';

function GroupsPanel() {
  const [start, setStart] = useState(1);
  const page = useScimPage<ScimGroup>('Groups', start);
  const list = page.data;
  return (
    <Panel title={list ? `Provisioned groups · ${list.totalResults}` : 'Provisioned groups'} padded={false}>
      <Table columns={GROUP_COLUMNS} header={['Display name', 'External ID', 'Members', 'Created']}>
        {list?.Resources.map((g) => (
          <div key={g.id} className={styles.tr} style={{ gridTemplateColumns: GROUP_COLUMNS }} role="row">
            <span className={styles.td}>
              <strong>{g.displayName}</strong>
            </span>
            <span className={`${styles.td} ${styles.mono}`}>{g.externalId ?? <span className={styles.subtle}>—</span>}</span>
            <span className={`${styles.td} ${styles.num}`}>{plural(g.members?.length ?? 0, 'member')}</span>
            <span className={`${styles.td} ${styles.subtle}`}>{formatDateTime(g.meta.created)}</span>
          </div>
        ))}
        <ListState error={page.error} loading={!list && page.loading} empty={list && list.Resources.length === 0 ? 'No groups provisioned yet.' : null} onRetry={page.reload} />
      </Table>
      {list && <Pager start={list.startIndex} shown={list.Resources.length} total={list.totalResults} onPage={setStart} />}
    </Panel>
  );
}

// ------------------------------------------------------------------ token

const EXPIRY: { value: string; label: string }[] = [
  { value: '', label: 'No expiration' },
  { value: '90', label: '90 days' },
  { value: '365', label: '1 year' },
];

function TokenDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [name, setName] = useState('SCIM provisioning');
  const [expiry, setExpiry] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [token, setToken] = useState<AccessToken | null>(null);
  const close = () => {
    onClose();
    setError(null);
    setToken(null);
  };
  const submit = async () => {
    if (busy || !name.trim()) return;
    setBusy(true);
    setError(null);
    try {
      setToken(await createToken({ name: name.trim(), scopes: ['scim:enterprise'], ...(expiry ? { expires_in_days: Number(expiry) } : {}) }));
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog
      open={open}
      onClose={close}
      title="Generate SCIM token"
      footer={
        token ? (
          <Button variant="primary" onClick={close}>
            Done
          </Button>
        ) : (
          <>
            <Button onClick={close}>Cancel</Button>
            <Button variant="primary" loading={busy} disabled={!name.trim()} onClick={() => void submit()}>
              Generate token
            </Button>
          </>
        )
      }
    >
      {token?.token ? (
        <div className={styles.form}>
          <p className={styles.confirmBody}>
            Copy the token now — it won’t be shown again. It is a personal access token of yours with the <span className={styles.mono}>scim:enterprise</span>{' '}
            scope{token.expires_at ? `, valid until ${formatDateTime(token.expires_at)}` : ''}; revoke it under Settings → Developer settings.
          </p>
          <div className={d.secret}>
            <code>{token.token}</code>
            <CopyButton text={token.token} label="Copy token" />
          </div>
        </div>
      ) : (
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <div className={d.callout} style={{ margin: 0 }}>
            <AlertIcon size={16} />
            <span>The token can create, change and suspend every account on this instance. Store it only in the identity provider.</span>
          </div>
          <div className={styles.formRow}>
            <Field label="Name" htmlFor="scim-token-name">
              <Input id="scim-token-name" value={name} autoComplete="off" onChange={(e) => setName(e.target.value)} />
            </Field>
            <Field label="Expiration" htmlFor="scim-token-expiry">
              <Select id="scim-token-expiry" value={expiry} onChange={(e) => setExpiry(e.target.value)}>
                {EXPIRY.map((x) => (
                  <option key={x.value} value={x.value}>
                    {x.label}
                  </option>
                ))}
              </Select>
            </Field>
          </div>
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

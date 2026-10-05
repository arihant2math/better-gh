import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { mutate, refresh, useResource } from '../../api/cache';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatDateTime } from '../../components/admin/format';
import { CopyButton, Drawer, ErrorState, JsonView, KeyValue, PageHeader, RadioCards, StatusPill, Switch, attempt, errorMessage, useConfirm } from '../../components/admin/kit';
import { invalidateLists, usePagedList } from '../../components/admin/usePagedList';
import { setQuery, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button, IconButton, cx } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, PencilIcon, PlusIcon, SyncIcon, TrashIcon, WebhookIcon } from '../../ui/icons';
import { Field, Input, Select } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import {
  ORG_HOOK_EVENTS,
  createHook,
  deleteHook,
  deliveriesPath,
  deliveriesPrefix,
  deliveryKey,
  getDelivery,
  hooksKey,
  isNotAllowed,
  listHooks,
  pingHook,
  redeliver,
  updateHook,
  type HookDeliveryItem,
  type HookInput,
  type OrgHook,
} from './api';
import { OwnerRequired } from './common';
import local from './OrgSettings.module.css';

const eventsSummary = (events: string[]) =>
  events.includes('*') ? 'Everything' : events.length === 1 ? events[0]! : events.length <= 3 ? events.join(', ') : `${events.slice(0, 2).join(', ')} +${events.length - 2}`;

const hookHost = (h: OrgHook) => {
  try {
    return new URL(h.config.url ?? '').host;
  } catch {
    return h.config.url ?? '';
  }
};

const deliveryStatus = (d: HookDeliveryItem) =>
  d.status === 'OK' ? (
    <StatusPill status="ok">{d.status_code || 'OK'}</StatusPill>
  ) : d.status_code ? (
    <StatusPill status="error">{d.status_code}</StatusPill>
  ) : d.status.toLowerCase() === 'pending' ? (
    <StatusPill status="neutral">Pending</StatusPill>
  ) : (
    <span title={d.status}>
      <StatusPill status="error">Failed</StatusPill>
    </span>
  );

export default function OrgHooksPage() {
  const { org = '' } = useParams<{ org: string }>();
  const query = useQuery();
  const hooks = useResource(hooksKey(org), () => listHooks(org));
  const selectedId = Number(query.get('hook')) || hooks.data?.[0]?.id || null;
  const selected = hooks.data?.find((h) => h.id === selectedId) ?? null;
  const [editing, setEditing] = useState<OrgHook | 'new' | null>(null);
  const confirm = useConfirm();
  // Bumped after a ping / creation so the deliveries list refetches.
  const [nonce, setNonce] = useState(0);
  const deliveriesChanged = (hookId: number) =>
    setTimeout(() => {
      invalidateLists(deliveriesPrefix(org, hookId));
      setNonce((n) => n + 1);
    }, 800);
  const reloadHooks = () => void refresh(hooksKey(org), () => listHooks(org));

  const ping = (h: OrgHook) =>
    void attempt('Could not ping the webhook', async () => {
      await pingHook(org, h.id);
      // The ping delivery is recorded asynchronously; refresh shortly after.
      deliveriesChanged(h.id);
    }, `Ping sent to ${hookHost(h)}`);

  const askDelete = (h: OrgHook) =>
    confirm({
      title: 'Delete webhook?',
      body: (
        <>
          Future events will no longer be delivered to <strong className={styles.mono}>{h.config.url}</strong>. Its delivery history is deleted too.
        </>
      ),
      confirmLabel: 'Delete webhook',
      danger: true,
      onConfirm: async () => {
        await deleteHook(org, h.id);
        mutate<OrgHook[]>(hooksKey(org), (prev) => (prev ?? []).filter((x) => x.id !== h.id));
        if (selectedId === h.id) setQuery({ hook: null });
        toast({ kind: 'success', title: 'Webhook deleted' });
      },
    });

  const toggleActive = async (h: OrgHook) => {
    const active = !h.active;
    mutate<OrgHook[]>(hooksKey(org), (prev) => (prev ?? []).map((x) => (x.id === h.id ? { ...x, active } : x)));
    try {
      const updated = await updateHook(org, h.id, { active });
      mutate<OrgHook[]>(hooksKey(org), (prev) => (prev ?? []).map((x) => (x.id === h.id ? updated : x)));
    } catch (err) {
      mutate<OrgHook[]>(hooksKey(org), (prev) => (prev ?? []).map((x) => (x.id === h.id ? { ...x, active: h.active } : x)));
      toast({ kind: 'error', title: 'Could not update the webhook', description: errorMessage(err) });
    }
  };

  useShortcuts('Organization webhooks', {
    n: { handler: () => setEditing('new'), description: 'Add webhook', group: 'Webhooks' },
    e: { handler: () => {
        if (selected) setEditing(selected);
      }, description: 'Edit selected webhook', group: 'Webhooks' },
    p: { handler: () => {
        if (selected) ping(selected);
      }, description: 'Ping selected webhook', group: 'Webhooks' },
  });

  if (hooks.error && !hooks.data) {
    return (
      <div className={styles.page}>
        <PageHeader title="Webhooks" />
        {isNotAllowed(hooks.error) ? <OwnerRequired org={org} what="manage webhooks" /> : <ErrorState error={hooks.error} onRetry={reloadHooks} />}
      </div>
    );
  }

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Webhooks"
        description={`Webhooks send a POST request to an external URL when events happen anywhere in ${org}.`}
        actions={
          <Button variant="primary" leadingIcon={PlusIcon} kbd="N" onClick={() => setEditing('new')}>
            Add webhook
          </Button>
        }
      />
      <div className={local.hooksPane}>
        {!hooks.data ? (
          <div className={local.hookList}>
            {Array.from({ length: 2 }, (_, i) => (
              <div key={i} className={local.hookRow}>
                <Skeleton width="60%" />
              </div>
            ))}
          </div>
        ) : hooks.data.length === 0 ? (
          <EmptyState icon={WebhookIcon} title="No webhooks yet" action={<Button onClick={() => setEditing('new')}>Add webhook</Button>}>
            Send events from every repository of {org} to your own services.
          </EmptyState>
        ) : (
          <ul className={local.hookList} aria-label="Webhooks">
            {hooks.data.map((h) => (
              <li key={h.id} className={cx(local.hookRow, h.id === selectedId && local.hookRowSelected)}>
                <button type="button" className={local.hookMain} onClick={() => setQuery({ hook: String(h.id) })} aria-current={h.id === selectedId || undefined}>
                  {h.active ? <StatusPill status="ok">Active</StatusPill> : <StatusPill status="neutral">Inactive</StatusPill>}
                  <span className={styles.cellMain}>
                    <strong className={styles.mono}>{h.config.url}</strong>
                    <span className={styles.subtle}>
                      {eventsSummary(h.events)} · {h.config.content_type === 'json' ? 'application/json' : 'form'}
                      {String(h.config.insecure_ssl) === '1' ? ' · SSL verification off' : ''} · updated <RelativeTime date={h.updated_at} />
                    </span>
                  </span>
                </button>
                <span className={local.hookActions}>
                  <Button size="sm" variant="ghost" onClick={() => void toggleActive(h)}>
                    {h.active ? 'Disable' : 'Enable'}
                  </Button>
                  <IconButton icon={SyncIcon} label="Ping" size="sm" onClick={() => ping(h)} />
                  <IconButton icon={PencilIcon} label="Edit" size="sm" onClick={() => setEditing(h)} />
                  <IconButton icon={TrashIcon} label="Delete" size="sm" onClick={() => askDelete(h)} />
                </span>
              </li>
            ))}
          </ul>
        )}
      </div>
      {selected && <Deliveries key={selected.id} org={org} hook={selected} nonce={nonce} />}
      <HookDialog
        org={org}
        hook={editing === 'new' ? null : editing}
        open={editing !== null}
        onClose={() => setEditing(null)}
        onSaved={(h, created) => {
          mutate<OrgHook[]>(hooksKey(org), (prev) => (created ? [...(prev ?? []), h] : (prev ?? []).map((x) => (x.id === h.id ? h : x))));
          if (created) {
            setQuery({ hook: String(h.id) });
            // Creating a hook pings it.
            deliveriesChanged(h.id);
          }
        }}
      />
      {confirm.dialog}
    </div>
  );
}


// ------------------------------------------------------------------ deliveries

function Deliveries({ org, hook, nonce }: { org: string; hook: OrgHook; nonce: number }) {
  const [status, setStatus] = useState('');
  const list = usePagedList<HookDeliveryItem>(deliveriesPath(org, hook.id, status));
  const { reload } = list;
  useEffect(() => {
    if (nonce) void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- reload only when a ping/creation bumps the nonce
  }, [nonce]);
  const [open, setOpen] = useState<HookDeliveryItem | null>(null);

  const columns: Column<HookDeliveryItem>[] = [
    { id: 'status', header: 'Status', width: '84px', render: deliveryStatus },
    {
      id: 'event',
      header: 'Event',
      width: 'minmax(160px, 2fr)',
      render: (d) => (
        <span className={styles.cellMain}>
          <span className={styles.mono}>
            {d.event}
            {d.action ? `.${d.action}` : ''}
          </span>
          <span className={styles.subtle}>{d.guid}</span>
        </span>
      ),
    },
    { id: 'redelivery', header: 'Redelivery', width: '96px', hideBelow: 760, render: (d) => (d.redelivery ? <StatusPill status="info">Redelivery</StatusPill> : null) },
    { id: 'duration', header: 'Duration', width: '84px', align: 'end', hideBelow: 640, render: (d) => `${d.duration < 1 ? Math.round(d.duration * 1000) + ' ms' : d.duration.toFixed(2) + ' s'}` },
    {
      id: 'delivered',
      header: 'Delivered',
      width: '112px',
      align: 'end',
      render: (d) => (
        <span title={formatDateTime(d.delivered_at)}>
          <RelativeTime date={d.delivered_at} />
        </span>
      ),
    },
  ];

  return (
    <>
      <div className={local.deliveriesHeader}>
        <h2 className={styles.panelTitle}>
          Recent deliveries <span className={styles.subtle}>· {hookHost(hook)}</span>
        </h2>
        <Tabs
          size="sm"
          items={[
            { id: '', label: 'All' },
            { id: 'success', label: 'Succeeded' },
            { id: 'failure', label: 'Failed' },
          ]}
          value={status}
          onChange={setStatus}
        />
        <IconButton icon={SyncIcon} label="Refresh deliveries" size="sm" onClick={() => void list.reload()} />
      </div>
      <DataTable
        aria-label="Recent deliveries"
        rows={list.items}
        columns={columns}
        getKey={(d) => d.id}
        onOpen={setOpen}
        loading={list.loading}
        hasMore={!!list.next}
        onEndReached={list.loadMore}
        rowHeight={40}
        empty={
          list.error ? (
            <EmptyState icon={AlertIcon} title="Could not load deliveries">
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={WebhookIcon} title="No deliveries yet">
              {hook.active ? 'Deliveries appear here once a subscribed event happens. Ping the hook to test it.' : 'This webhook is inactive.'}
            </EmptyState>
          )
        }
      />
      <DeliveryDrawer
        org={org}
        hookId={hook.id}
        item={open}
        onClose={() => setOpen(null)}
        onRedelivered={() => {
          setOpen(null);
          setTimeout(() => void list.reload(), 800);
        }}
      />
    </>
  );
}

function headerItems(h: Record<string, string> | null | undefined): [string, string][] {
  return Object.entries(h ?? {}).map(([k, v]) => [k, String(v)]);
}

function DeliveryDrawer({ org, hookId, item, onClose, onRedelivered }: { org: string; hookId: number; item: HookDeliveryItem | null; onClose: () => void; onRedelivered: () => void }) {
  const detail = useResource(item ? deliveryKey(org, hookId, item.id) : null, () => getDelivery(org, hookId, item!.id), { immutable: true });
  const [tab, setTab] = useState('request');
  const [busy, setBusy] = useState(false);
  const d = detail.data;
  const payload = useMemo(() => (d ? JSON.stringify(d.request.payload, null, 2) : ''), [d]);
  const doRedeliver = async () => {
    if (!item) return;
    setBusy(true);
    const ok = await attempt('Could not redeliver', () => redeliver(org, hookId, item.id), 'Redelivery requested');
    setBusy(false);
    if (ok) onRedelivered();
  };
  return (
    <Drawer
      open={!!item}
      onClose={onClose}
      title={item ? `${item.event}${item.action ? `.${item.action}` : ''} · ${item.guid}` : 'Delivery'}
      footer={
        <Button variant="primary" leadingIcon={SyncIcon} loading={busy} onClick={() => void doRedeliver()}>
          Redeliver
        </Button>
      }
    >
      {item && (
        <div className={styles.stack}>
          <KeyValue
            items={[
              ['Result', <span>{deliveryStatus(item)} {item.status !== 'OK' && <span className={styles.muted}>{item.status}</span>}</span>],
              ['Delivered', formatDateTime(item.delivered_at)],
              ['Duration', `${item.duration.toFixed(2)} s`],
              ['Redelivery', item.redelivery ? 'Yes' : 'No'],
              ...(d?.url ? [['URL', <span className={styles.mono}>{d.url}</span>] as [string, ReactNode]] : []),
            ]}
          />
          <Tabs
            size="sm"
            items={[
              { id: 'request', label: 'Request' },
              { id: 'response', label: 'Response' },
            ]}
            value={tab}
            onChange={setTab}
          />
          {!d ? (
            detail.error ? (
              <p className={styles.muted}>{errorMessage(detail.error)}</p>
            ) : (
              <Skeleton height={120} />
            )
          ) : tab === 'request' ? (
            <>
              <h3 className={styles.panelTitle}>Headers</h3>
              <KeyValue items={headerItems(d.request.headers)} />
              <div className={local.jsonHeader}>
                <h3 className={styles.panelTitle}>Payload</h3>
                <CopyButton text={payload} label="Copy payload" />
              </div>
              <JsonView value={d.request.payload} />
            </>
          ) : (
            <>
              <h3 className={styles.panelTitle}>Headers</h3>
              {headerItems(d.response.headers).length ? <KeyValue items={headerItems(d.response.headers)} /> : <p className={styles.muted}>No response headers.</p>}
              <h3 className={styles.panelTitle}>Body</h3>
              {d.response.payload ? <pre className={styles.json}>{d.response.payload}</pre> : <p className={styles.muted}>Empty body.</p>}
            </>
          )}
        </div>
      )}
    </Drawer>
  );
}

// ------------------------------------------------------------------ create / edit

type EventMode = 'push' | 'all' | 'custom';

function HookDialog({ org, hook, open, onClose, onSaved }: { org: string; hook: OrgHook | null; open: boolean; onClose: () => void; onSaved: (h: OrgHook, created: boolean) => void }) {
  const init = () => ({
    url: hook?.config.url ?? '',
    contentType: (hook?.config.content_type === 'form' ? 'form' : 'json') as 'json' | 'form',
    secret: '',
    insecure: String(hook?.config.insecure_ssl ?? '0') === '1',
    mode: (!hook || (hook.events.length === 1 && hook.events[0] === 'push') ? 'push' : hook.events.includes('*') ? 'all' : 'custom') as EventMode,
    events: hook && !hook.events.includes('*') ? hook.events : ['push'],
    active: hook?.active ?? true,
  });
  const [form, setForm] = useState(init);
  const [filter, setFilter] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shownFor, setShownFor] = useState<{ open: boolean; hook: OrgHook | null }>({ open: false, hook: null });
  if (open !== shownFor.open || hook !== shownFor.hook) {
    setShownFor({ open, hook });
    if (open) {
      setForm(init());
      setFilter('');
      setError(null);
    }
  }
  const set = (p: Partial<ReturnType<typeof init>>) => setForm((f) => ({ ...f, ...p }));
  const urlOk = /^https?:\/\/[^\s/]+/i.test(form.url.trim());
  const events = form.mode === 'push' ? ['push'] : form.mode === 'all' ? ['*'] : form.events;
  const valid = urlOk && events.length > 0;
  const hasSecret = !!hook?.config.secret;
  const shown = ORG_HOOK_EVENTS.filter((e) => !filter || e.name.includes(filter.toLowerCase().replace(/\s+/g, '_')));

  const submit = async () => {
    if (!valid || busy) return;
    setBusy(true);
    setError(null);
    const config: HookInput['config'] = { url: form.url.trim(), content_type: form.contentType, insecure_ssl: form.insecure ? '1' : '0' };
    if (form.secret) config.secret = form.secret;
    try {
      const saved = hook ? await updateHook(org, hook.id, { config, events, active: form.active }) : await createHook(org, { config, events, active: form.active });
      onSaved(saved, !hook);
      toast({ kind: 'success', title: hook ? 'Webhook updated' : 'Webhook created', description: hook ? undefined : 'A ping was sent to check the URL.' });
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
      title={hook ? 'Edit webhook' : 'Add webhook'}
      className={local.wideDialog}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!valid} onClick={() => void submit()}>
            {hook ? 'Update webhook' : 'Add webhook'}
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
        <Field label="Payload URL" htmlFor="hk-url" error={form.url && !urlOk ? 'Enter an http(s) URL.' : null}>
          <Input id="hk-url" type="url" value={form.url} onChange={(e) => set({ url: e.target.value })} placeholder="https://example.com/postreceive" autoFocus spellCheck={false} invalid={!!form.url && !urlOk} />
        </Field>
        <div className={styles.formRow}>
          <Field label="Content type" htmlFor="hk-ct">
            <Select id="hk-ct" value={form.contentType} onChange={(e) => set({ contentType: e.target.value as 'json' | 'form' })}>
              <option value="json">application/json</option>
              <option value="form">application/x-www-form-urlencoded</option>
            </Select>
          </Field>
          <Field label="Secret" htmlFor="hk-secret" hint={hasSecret ? 'A secret is set. Leave blank to keep it.' : 'Used to sign payloads (X-Hub-Signature-256).'}>
            <Input id="hk-secret" type="password" value={form.secret} onChange={(e) => set({ secret: e.target.value })} autoComplete="new-password" placeholder={hasSecret ? '********' : ''} />
          </Field>
        </div>
        <div>
          <div className={local.sectionLabel}>SSL verification</div>
          <RadioCards
            name="hk-ssl"
            label="SSL verification"
            value={form.insecure ? '1' : '0'}
            onChange={(v) => set({ insecure: v === '1' })}
            options={[
              { value: '0', label: 'Enable SSL verification', description: 'Recommended.' },
              { value: '1', label: 'Disable', description: 'Not recommended: payloads may be intercepted.' },
            ]}
          />
        </div>
        <div>
          <div className={local.sectionLabel}>Which events would you like to trigger this webhook?</div>
          <RadioCards
            name="hk-mode"
            label="Events"
            value={form.mode}
            onChange={(v) => set({ mode: v })}
            options={[
              { value: 'push', label: 'Just the push event' },
              { value: 'all', label: 'Send me everything' },
              { value: 'custom', label: 'Let me select individual events' },
            ]}
          />
        </div>
        {form.mode === 'custom' && (
          <div className={local.eventsBox}>
            <div className={local.eventsToolbar}>
              <Input size="sm" aria-label="Filter events" placeholder="Filter events" value={filter} onChange={(e) => setFilter(e.target.value)} />
              <span className={styles.meta}>{form.events.length} selected</span>
              <Button size="sm" variant="ghost" onClick={() => set({ events: [] })} disabled={!form.events.length}>
                Clear
              </Button>
            </div>
            <div className={local.eventsGrid} role="group" aria-label="Events">
              {shown.map((e) => (
                <label key={e.name} className={local.eventItem}>
                  <input
                    type="checkbox"
                    checked={form.events.includes(e.name)}
                    onChange={(ev) => set({ events: ev.target.checked ? [...form.events, e.name] : form.events.filter((x) => x !== e.name) })}
                  />
                  <span>
                    <span className={styles.mono}>{e.name}</span>
                    <span className={local.eventDesc}>{e.description}</span>
                  </span>
                </label>
              ))}
            </div>
            {form.events.length === 0 && <div className={styles.formError}>Select at least one event.</div>}
          </div>
        )}
        <Switch label="Active" description="Deliver event details when this hook is triggered." checked={form.active} onChange={(v) => set({ active: v })} />
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

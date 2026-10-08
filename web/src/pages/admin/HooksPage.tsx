import { useRef, useState } from 'react';
import { v3 } from '../../api/client';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatDateTime } from '../../components/admin/format';
import { PageHeader, StatusPill, Switch, attempt, errorMessage, useConfirm } from '../../components/admin/kit';
import { usePagedList } from '../../api/usePagedList';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button, IconButton } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import { AlertIcon, KebabHorizontalIcon, PlusIcon, WebhookIcon } from '../../ui/icons';
import { Field, Input, Select } from '../../ui/Input';
import { Menu } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import { createHook, deleteHook, pingHook, updateHook, type GlobalHook, type HookInput } from '../../api/admin';
import h from './hooks.module.css';

const HOOKS_PATH = `${v3('admin', 'hooks')}?per_page=100`;

/** Events a global webhook may subscribe to (`GLOBAL_EVENTS` in crates/bgh-admin/src/hooks.rs). */
const EVENTS: { id: string; label: string; description: string }[] = [
  { id: 'user', label: 'User', description: 'Accounts created, renamed, promoted, suspended or deleted.' },
  { id: 'organization', label: 'Organization', description: 'Organizations created, renamed or deleted.' },
  { id: 'repository', label: 'Repository', description: 'Repositories created, deleted, transferred or changed visibility.' },
  { id: 'team', label: 'Team', description: 'Teams created, changed or deleted.' },
  { id: 'membership', label: 'Membership', description: 'Members added to or removed from teams.' },
];
const ALL_EVENTS = '*';

const hostOf = (url: string) => {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
};

const isHttpUrl = (s: string) => /^https?:\/\/[^\s/]+/i.test(s.trim());

const eventsText = (events: string[]) => (events.includes(ALL_EVENTS) ? 'All events' : events.join(', '));

export default function HooksPage() {
  const list = usePagedList<GlobalHook>(HOOKS_PATH);
  const [editing, setEditing] = useState<GlobalHook | 'new' | null>(null);
  const confirm = useConfirm();

  useShortcuts('Global webhooks', {
    c: { handler: () => setEditing('new'), description: 'New global webhook', group: 'Webhooks' },
  });

  const replace = (hook: GlobalHook) => list.update((items) => items.map((x) => (x.id === hook.id ? hook : x)));

  const ping = (hook: GlobalHook) =>
    void attempt(`Could not ping ${hostOf(hook.config.url)}`, () => pingHook(hook.id), `Ping sent to ${hostOf(hook.config.url)}`);

  const toggleActive = async (hook: GlobalHook) => {
    const active = !hook.active;
    replace({ ...hook, active });
    try {
      replace(await updateHook(hook.id, { active }));
      toast({ kind: 'success', title: active ? 'Webhook activated' : 'Webhook deactivated' });
    } catch (err) {
      replace(hook);
      toast({ kind: 'error', title: 'Could not update the webhook', description: errorMessage(err) });
    }
  };

  const remove = (hook: GlobalHook) =>
    confirm({
      title: 'Delete global webhook?',
      body: (
        <>
          Deliveries to <strong>{hook.config.url}</strong> stop immediately. This can’t be undone.
        </>
      ),
      confirmText: hostOf(hook.config.url),
      confirmLabel: 'Delete webhook',
      danger: true,
      onConfirm: async () => {
        await deleteHook(hook.id);
        list.update((items) => items.filter((x) => x.id !== hook.id));
        toast({ kind: 'success', title: 'Webhook deleted' });
      },
    });

  const columns: Column<GlobalHook>[] = [
    {
      id: 'url',
      header: 'Payload URL',
      width: 'minmax(240px, 3fr)',
      render: (x) => (
        <span className={styles.cellMain}>
          <span className={styles.mono} title={x.config.url}>
            {x.config.url}
          </span>
          <span className={styles.subtle}>{eventsText(x.events)}</span>
        </span>
      ),
    },
    {
      id: 'status',
      header: 'Status',
      width: '120px',
      render: (x) => (
        <>
          {x.active ? <StatusPill status="ok">Active</StatusPill> : <StatusPill status="neutral">Inactive</StatusPill>}
          {x.config.insecure_ssl === '1' && (
            <span title="SSL verification is disabled">
              <StatusPill status="warning">No SSL</StatusPill>
            </span>
          )}
        </>
      ),
    },
    { id: 'type', header: 'Content type', width: '104px', hideBelow: 760, render: (x) => <span className={styles.mono}>{x.config.content_type}</span> },
    { id: 'created', header: 'Created', width: '104px', align: 'end', hideBelow: 900, render: (x) => <RelativeTime date={x.created_at} /> },
    {
      id: 'actions',
      header: <span className="visually-hidden">Actions</span>,
      width: '40px',
      align: 'end',
      render: (x) => <RowMenu hook={x} onEdit={() => setEditing(x)} onPing={() => ping(x)} onToggle={() => void toggleActive(x)} onDelete={() => remove(x)} />,
    },
  ];

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Global webhooks"
        description="Instance-wide webhooks for account and organization events, delivered to every configured URL."
        actions={
          <Button variant="primary" leadingIcon={PlusIcon} kbd="c" onClick={() => setEditing('new')}>
            Add webhook
          </Button>
        }
      />
      <DataTable
        aria-label="Global webhooks"
        rows={list.items}
        columns={columns}
        getKey={(x) => x.id}
        onOpen={(x) => setEditing(x)}
        loading={list.loading}
        hasMore={!!list.next}
        onEndReached={() => void list.loadMore()}
        footer={<span>Delivery history isn’t available for global webhooks yet.</span>}
        empty={
          list.error ? (
            <EmptyState icon={WebhookIcon} title="Could not load webhooks" action={<Button onClick={() => void list.reload()}>Try again</Button>}>
              {errorMessage(list.error)}
            </EmptyState>
          ) : (
            <EmptyState
              icon={WebhookIcon}
              title="No global webhooks"
              action={
                <Button variant="primary" leadingIcon={PlusIcon} onClick={() => setEditing('new')}>
                  Add webhook
                </Button>
              }
            >
              Get a POST for every user and organization change on this instance.
            </EmptyState>
          )
        }
      />
      <HookDialog
        hook={editing}
        onClose={() => setEditing(null)}
        onSaved={(hook, created) => {
          if (created) list.update((items) => [...items, hook]);
          else replace(hook);
          setEditing(null);
          toast({ kind: 'success', title: created ? 'Webhook created' : 'Webhook updated' });
        }}
      />
      {confirm.dialog}
    </div>
  );
}

function RowMenu({ hook, onEdit, onPing, onToggle, onDelete }: { hook: GlobalHook; onEdit: () => void; onPing: () => void; onToggle: () => void; onDelete: () => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  return (
    <span onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
      <IconButton ref={ref} icon={KebabHorizontalIcon} size="sm" label={`Actions for ${hostOf(hook.config.url)}`} tooltip={false} onClick={() => setOpen((o) => !o)} aria-haspopup="menu" aria-expanded={open} />
      <Menu
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        placement="bottom-end"
        aria-label="Webhook actions"
        items={[
          { id: 'edit', label: 'Edit', onSelect: onEdit },
          { id: 'ping', label: 'Send ping', description: 'Delivers a ping event now.', onSelect: onPing },
          { id: 'toggle', label: hook.active ? 'Deactivate' : 'Activate', onSelect: onToggle },
          { separator: true, id: 'sep' },
          { id: 'delete', label: 'Delete…', danger: true, onSelect: onDelete },
        ]}
      />
    </span>
  );
}

interface HookForm {
  url: string;
  content_type: 'json' | 'form';
  secret: string;
  secretStored: boolean;
  clearSecret: boolean;
  verifySsl: boolean;
  events: string[];
  active: boolean;
}

const formOf = (hook: GlobalHook | null): HookForm =>
  hook
    ? {
        url: hook.config.url,
        content_type: hook.config.content_type,
        secret: '',
        secretStored: !!hook.config.secret,
        clearSecret: false,
        verifySsl: hook.config.insecure_ssl !== '1',
        events: [...hook.events],
        active: hook.active,
      }
    : { url: 'https://', content_type: 'json', secret: '', secretStored: false, clearSecret: false, verifySsl: true, events: ['user', 'organization'], active: true };

function HookDialog({ hook, onClose, onSaved }: { hook: GlobalHook | 'new' | null; onClose: () => void; onSaved: (hook: GlobalHook, created: boolean) => void }) {
  const existing = hook && hook !== 'new' ? hook : null;
  const [form, setForm] = useState<HookForm>(() => formOf(existing));
  const [shownFor, setShownFor] = useState(hook);
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (hook !== shownFor) {
    setShownFor(hook);
    setForm(formOf(existing));
    setSubmitted(false);
    setError(null);
    setBusy(false);
  }
  const set = (patch: Partial<HookForm>) => setForm((f) => ({ ...f, ...patch }));
  const urlError = !isHttpUrl(form.url) ? 'Enter an http:// or https:// URL.' : null;
  const eventsError = form.events.length === 0 ? 'Choose at least one event.' : null;
  const showUrlError = (submitted || (form.url && form.url !== 'https://')) && urlError;
  const all = form.events.includes(ALL_EVENTS);

  const toggleEvent = (id: string, on: boolean) => set({ events: on ? [...form.events.filter((e) => e !== id), id] : form.events.filter((e) => e !== id) });

  const submit = async () => {
    setSubmitted(true);
    if (urlError || eventsError || busy) return;
    const config: NonNullable<HookInput['config']> = {
      url: form.url.trim(),
      content_type: form.content_type,
      insecure_ssl: form.verifySsl ? '0' : '1',
    };
    if (form.secret) config.secret = form.secret;
    else if (form.clearSecret) config.secret = '';
    const body: HookInput = { config, events: all ? [ALL_EVENTS] : form.events, active: form.active };
    setBusy(true);
    setError(null);
    try {
      const saved = existing ? await updateHook(existing.id, body) : await createHook(body);
      onSaved(saved, !existing);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={!!hook}
      onClose={onClose}
      title={existing ? 'Edit global webhook' : 'Add global webhook'}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} onClick={() => void submit()}>
            {existing ? 'Update webhook' : 'Add webhook'}
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
        <Field label="Payload URL" htmlFor="hook-url" error={showUrlError || null}>
          <Input id="hook-url" type="url" value={form.url} invalid={!!showUrlError} autoFocus spellCheck={false} autoComplete="off" onChange={(e) => set({ url: e.target.value })} />
        </Field>
        <div className={styles.formRow}>
          <Field label="Content type" htmlFor="hook-ct">
            <Select id="hook-ct" value={form.content_type} onChange={(e) => set({ content_type: e.target.value as HookForm['content_type'] })}>
              <option value="json">application/json</option>
              <option value="form">application/x-www-form-urlencoded</option>
            </Select>
          </Field>
          <Field
            label="Secret"
            htmlFor="hook-secret"
            hint={
              form.clearSecret
                ? 'The stored secret will be removed.'
                : form.secretStored
                  ? 'A secret is stored. Type a new one to replace it.'
                  : 'Used to sign payloads (X-Hub-Signature-256). Write-only.'
            }
          >
            <div className={h.secretRow}>
              <Input
                id="hook-secret"
                type="password"
                autoComplete="new-password"
                value={form.secret}
                disabled={form.clearSecret}
                placeholder={form.clearSecret ? 'Will be removed' : form.secretStored ? 'Stored — leave unchanged' : ''}
                onChange={(e) => set({ secret: e.target.value })}
              />
              {form.secretStored && (
                <Button size="sm" variant="ghost" onClick={() => set({ clearSecret: !form.clearSecret, secret: '' })}>
                  {form.clearSecret ? 'Undo' : 'Remove'}
                </Button>
              )}
            </div>
          </Field>
        </div>
        <Switch
          checked={form.verifySsl}
          onChange={(verifySsl) => set({ verifySsl })}
          label="Verify SSL certificates"
          description="Check the receiver’s TLS certificate when delivering payloads."
        />
        {!form.verifySsl && (
          <div className={h.warning} role="alert">
            <AlertIcon size={14} /> Payloads may be intercepted or tampered with in transit. Only disable verification for trusted, internal receivers.
          </div>
        )}
        <fieldset className={h.events}>
          <legend className={h.legend}>Events</legend>
          <label className={h.event}>
            <input type="checkbox" checked={all} onChange={(e) => set({ events: e.target.checked ? [ALL_EVENTS] : ['user', 'organization'] })} />
            <span>
              <span className={h.eventLabel}>Send me everything</span>
              <span className={h.eventDesc}>Every event listed below, including ones added later.</span>
            </span>
          </label>
          {EVENTS.map((ev) => (
            <label key={ev.id} className={h.event} data-disabled={all || undefined}>
              <input type="checkbox" checked={all || form.events.includes(ev.id)} disabled={all} onChange={(e) => toggleEvent(ev.id, e.target.checked)} />
              <span>
                <span className={h.eventLabel}>
                  {ev.label} <span className={styles.mono}>{ev.id}</span>
                </span>
                <span className={h.eventDesc}>{ev.description}</span>
              </span>
            </label>
          ))}
          <p className={styles.subtle} style={{ margin: '4px 0 0' }}>
            A <span className={styles.mono}>ping</span> event is always sent when you use “Send ping”.
          </p>
          {submitted && eventsError && <div className={h.fieldError}>{eventsError}</div>}
        </fieldset>
        <Switch checked={form.active} onChange={(active) => set({ active })} label="Active" description="Deliver events to this URL." />
        {existing && (
          <p className={styles.subtle} style={{ margin: 0 }}>
            Created {formatDateTime(existing.created_at)} · updated <RelativeTime date={existing.updated_at} />. Delivery history isn’t available for global webhooks yet.
          </p>
        )}
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

import { useEffect, useId, useState } from 'react';
import { invalidate, useResource } from '../../../api/cache';
import {
  createHook,
  deleteHook,
  getDelivery,
  getHook,
  listDeliveries,
  listHooks,
  pingHook,
  redeliver,
  testHook,
  updateHook,
  type Hook,
  type HookInput,
} from '../../../api/repoSettings';
import { Banner, ButtonRow, Checkbox, ConfirmDialog, FormStack, ItemList, ItemRow, PageHeader, Pill, RadioCards, Section, apiFieldErrors, errorMessage } from '../../../components/settings/kit';
import { Link, navigate, setQuery, useQuery } from '../../../router';
import type { Repo } from '../../../sync/models';
import { Button, cx } from '../../../ui/Button';
import { EmptyState } from '../../../ui/EmptyState';
import { AlertIcon, CheckCircleIcon, ChevronRightIcon, ClockIcon, PlusIcon, SyncIcon, TrashIcon, WebhookIcon, ZapIcon } from '../../../ui/icons';
import { Field, Input, Select } from '../../../ui/Input';
import { RelativeTime } from '../../../ui/RelativeTime';
import { Tabs } from '../../../ui/Tabs';
import { toast } from '../../../ui/Toast';
import { HOOK_EVENTS, deliveryOk, eventsFor, eventsMode, eventsSummary, type EventsMode } from '../model';
import styles from '../RepoSettings.module.css';
import { ListSkeleton, LoadError, repoKey, useLocalResource, type SectionProps } from '../shared';
import { hookUrlError } from '../validation';
import type { HookDeliveryItem } from '../../../api/types';

export default function WebhooksSettings({ repo, rest, base }: SectionProps) {
  if (rest[0] === 'new') return <HookForm key="new" repo={repo} base={base} />;
  if (rest[0] && /^\d+$/.test(rest[0])) return <EditHook key={rest[0]} repo={repo} base={base} id={Number(rest[0])} />;
  return <HookList repo={repo} base={base} />;
}

function StatusIcon({ status, label }: { status: string | undefined; label?: string }) {
  if (status === 'OK' || status === 'active')
    return (
      <span className={cx(styles.statusDot, styles.ok)} title={label ?? 'Last delivery was successful'}>
        <CheckCircleIcon size={16} aria-label={label ?? 'Last delivery was successful'} />
      </span>
    );
  if (!status || status === 'unused' || status === 'pending')
    return (
      <span className={cx(styles.statusDot, styles.small)} title={label ?? 'No deliveries yet'}>
        <ClockIcon size={16} aria-label={label ?? (status === 'pending' ? 'Pending' : 'No deliveries yet')} />
      </span>
    );
  return (
    <span className={cx(styles.statusDot, styles.bad)} title={label ?? 'Last delivery failed'}>
      <AlertIcon size={16} aria-label={label ?? 'Last delivery failed'} />
    </span>
  );
}

// ------------------------------------------------------------------ list

function HookList({ repo, base }: { repo: Repo; base: string }) {
  const hooks = useLocalResource(repoKey(repo, 'hooks'), () => listHooks(repo.owner, repo.name));
  const [deleting, setDeleting] = useState<Hook | null>(null);
  return (
    <>
      <PageHeader
        title="Webhooks"
        description="Webhooks allow external services to be notified when certain events happen. When the specified events happen, we'll send a POST request to each of the URLs you provide."
        actions={
          <Button variant="primary" size="sm" leadingIcon={PlusIcon} onClick={() => navigate(`${base}/hooks/new`)}>
            Add webhook
          </Button>
        }
      />
      {hooks.error ? <LoadError error={hooks.error} /> : null}
      {!hooks.data ? (
        hooks.error ? null : <ListSkeleton rows={2} />
      ) : hooks.data.length === 0 ? (
        <EmptyState icon={WebhookIcon} title="No webhooks yet" action={<Button onClick={() => navigate(`${base}/hooks/new`)}>Add webhook</Button>}>
          Send events from this repository to CI servers, chat bots or your own services.
        </EmptyState>
      ) : (
        <ItemList aria-label="Webhooks">
          {hooks.data.map((h) => (
            <ItemRow
              key={h.id}
              leading={<StatusIcon status={h.last_response?.status} label={h.last_response?.message ?? undefined} />}
              title={
                <span className={styles.row}>
                  <Link to={`${base}/hooks/${h.id}`} className={styles.mono}>
                    {h.config.url}
                  </Link>
                  {!h.active && <Pill>Inactive</Pill>}
                </span>
              }
              meta={`(${eventsSummary(h.events)})`}
              actions={
                <>
                  <Button size="sm" onClick={() => navigate(`${base}/hooks/${h.id}`)}>
                    Edit
                  </Button>
                  <Button size="sm" variant="danger" leadingIcon={TrashIcon} aria-label={`Delete webhook ${h.config.url}`} onClick={() => setDeleting(h)}>
                    Delete
                  </Button>
                </>
              }
            />
          ))}
        </ItemList>
      )}
      <DeleteHookDialog
        repo={repo}
        hook={deleting}
        onClose={() => setDeleting(null)}
        onDeleted={(id) => hooks.update((l) => l.filter((x) => x.id !== id))}
      />
    </>
  );
}

function DeleteHookDialog({ repo, hook, onClose, onDeleted }: { repo: Repo; hook: Hook | null; onClose: () => void; onDeleted: (id: number) => void }) {
  return (
    <ConfirmDialog
      open={!!hook}
      onClose={onClose}
      title="Delete webhook?"
      confirmLabel="Yes, delete webhook"
      onConfirm={async () => {
        if (!hook) return;
        await deleteHook(repo.owner, repo.name, hook.id);
        invalidate(repoKey(repo, 'hooks'));
        onDeleted(hook.id);
        toast({ kind: 'success', title: 'Webhook deleted' });
      }}
    >
      <p className={styles.muted}>
        This action cannot be undone. Future events will no longer be delivered to <code>{hook?.config.url}</code>.
      </p>
    </ConfirmDialog>
  );
}

// ------------------------------------------------------------------ edit page

function EditHook({ repo, base, id }: { repo: Repo; base: string; id: number }) {
  const hook = useResource(`${repoKey(repo, 'hook')}${id}`, () => getHook(repo.owner, repo.name, id));
  const tab = useQuery().get('tab') === 'deliveries' ? 'deliveries' : 'settings';
  if (hook.error) return <LoadError error={hook.error} />;
  if (!hook.data) return <ListSkeleton rows={5} />;
  return (
    <>
      <PageHeader
        title={<span className={styles.row}>Webhooks / Manage webhook</span>}
        description={<Link to={`${base}/hooks`}>← All webhooks</Link>}
      />
      <div style={{ marginBottom: 20 }}>
        <Tabs
          items={[
            { id: 'settings', label: 'Settings' },
            { id: 'deliveries', label: 'Recent Deliveries' },
          ]}
          value={tab}
          onChange={(t) => setQuery({ tab: t === 'settings' ? null : t })}
        />
      </div>
      {tab === 'settings' ? <HookForm repo={repo} base={base} hook={hook.data} /> : <Deliveries repo={repo} hook={hook.data} />}
    </>
  );
}

// ------------------------------------------------------------------ form

function HookForm({ repo, base, hook }: { repo: Repo; base: string; hook?: Hook }) {
  const ids = { url: useId(), ct: useId(), secret: useId() };
  const [url, setUrl] = useState(hook?.config.url ?? '');
  const [contentType, setContentType] = useState<'json' | 'form'>(hook?.config.content_type ?? 'form');
  const [secret, setSecret] = useState('');
  const [ssl, setSsl] = useState<'0' | '1'>(hook?.config.insecure_ssl ?? '0');
  const [mode, setMode] = useState<EventsMode>(hook ? eventsMode(hook.events) : 'push');
  const [custom, setCustom] = useState<string[]>(hook && eventsMode(hook.events) === 'custom' ? hook.events : ['push']);
  const [active, setActive] = useState(hook?.active ?? true);
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [server, setServer] = useState<string | null>(null);
  const [deleting, setDeleting] = useState(false);
  const urlErr = hookUrlError(url);
  const eventsErr = mode === 'custom' && custom.length === 0 ? 'Select at least one event.' : null;

  const submit = async () => {
    setTouched(true);
    if (urlErr || eventsErr || busy) return;
    setBusy(true);
    setServer(null);
    const input: HookInput = {
      active,
      events: eventsFor(mode, custom),
      config: { url: url.trim(), content_type: contentType, insecure_ssl: ssl },
    };
    if (secret) input.config.secret = secret;
    try {
      if (hook) {
        await updateHook(repo.owner, repo.name, hook.id, input);
        invalidate(`${repoKey(repo, 'hook')}${hook.id}`);
        invalidate(repoKey(repo, 'hooks'));
        setSecret('');
        toast({ kind: 'success', title: 'Okay, the hook was successfully updated.' });
      } else {
        const created = await createHook(repo.owner, repo.name, input);
        invalidate(repoKey(repo, 'hooks'));
        toast({ kind: 'success', title: 'Okay, that hook was successfully created.', description: created.active ? 'We sent a ping payload to test it out.' : undefined });
        navigate(`${base}/hooks/${created.id}?tab=deliveries`);
      }
    } catch (e) {
      setServer(apiFieldErrors(e).message);
    } finally {
      setBusy(false);
    }
  };

  const toggleEvent = (id: string, on: boolean) => setCustom((c) => (on ? [...c, id] : c.filter((x) => x !== id)));

  return (
    <>
      {!hook && (
        <PageHeader
          title="Add webhook"
          description="We'll send a POST request to the URL below with details of any subscribed events. You can also specify which data format you'd like to receive (JSON, x-www-form-urlencoded, etc)."
        />
      )}
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <FormStack>
          <Field label="Payload URL" htmlFor={ids.url} error={touched ? urlErr : null}>
            <Input
              id={ids.url}
              value={url}
              autoFocus={!hook}
              inputMode="url"
              placeholder="https://example.com/postreceive"
              spellCheck={false}
              autoComplete="off"
              invalid={touched && !!urlErr}
              onChange={(e) => setUrl(e.target.value)}
              onBlur={() => url && setTouched(true)}
            />
          </Field>
          <Field label="Content type" htmlFor={ids.ct}>
            <Select id={ids.ct} value={contentType} onChange={(e) => setContentType(e.target.value as 'json' | 'form')}>
              <option value="form">application/x-www-form-urlencoded</option>
              <option value="json">application/json</option>
            </Select>
          </Field>
          <Field
            label="Secret"
            htmlFor={ids.secret}
            hint={hook?.config.secret ? 'A secret is set. Leave blank to keep the current secret.' : 'Used to sign payloads (X-Hub-Signature-256).'}
          >
            <Input
              id={ids.secret}
              type="password"
              value={secret}
              autoComplete="new-password"
              placeholder={hook?.config.secret ? '•••••••• (unchanged)' : ''}
              onChange={(e) => setSecret(e.target.value)}
            />
          </Field>
          <div className={styles.group}>
            <span className={styles.subhead}>SSL verification</span>
            <RadioCards
              aria-label="SSL verification"
              value={ssl}
              onChange={setSsl}
              columns={2}
              options={[
                { value: '0', label: 'Enable SSL verification', description: 'Verify certificates when delivering payloads.' },
                { value: '1', label: 'Disable (not recommended)', description: 'Payloads may be intercepted.' },
              ]}
            />
            {ssl === '1' && <Banner tone="warning">By disabling SSL verification, you are vulnerable to man-in-the-middle and other attacks.</Banner>}
          </div>
          <div className={styles.group}>
            <span className={styles.subhead}>Which events would you like to trigger this webhook?</span>
            <RadioCards
              aria-label="Events"
              value={mode}
              onChange={setMode}
              options={[
                { value: 'push', label: 'Just the push event.' },
                { value: 'all', label: 'Send me everything.' },
                { value: 'custom', label: 'Let me select individual events.' },
              ]}
            />
            {mode === 'custom' && (
              <>
                <div className={styles.eventGrid} role="group" aria-label="Individual events">
                  {HOOK_EVENTS.map((e) => (
                    <Checkbox key={e.id} label={e.label} description={e.description} checked={custom.includes(e.id)} onChange={(v) => toggleEvent(e.id, v)} />
                  ))}
                </div>
                {touched && eventsErr && <Banner tone="danger">{eventsErr}</Banner>}
              </>
            )}
          </div>
          <Checkbox label="Active" description="We will deliver event details when this hook is triggered." checked={active} onChange={setActive} />
          {server && (
            <Banner tone="danger" icon={AlertIcon}>
              {server}
            </Banner>
          )}
          <ButtonRow>
            <Button type="submit" variant="primary" loading={busy}>
              {hook ? 'Update webhook' : 'Add webhook'}
            </Button>
            <Button onClick={() => navigate(`${base}/hooks`)}>Cancel</Button>
            {hook && (
              <>
                <span className={styles.spacer} />
                <Button variant="danger" leadingIcon={TrashIcon} onClick={() => setDeleting(true)}>
                  Delete webhook
                </Button>
              </>
            )}
          </ButtonRow>
        </FormStack>
      </form>
      {hook && (
        <DeleteHookDialog
          repo={repo}
          hook={deleting ? hook : null}
          onClose={() => setDeleting(false)}
          onDeleted={() => navigate(`${base}/hooks`)}
        />
      )}
    </>
  );
}

// ------------------------------------------------------------------ deliveries

function Deliveries({ repo, hook }: { repo: Repo; hook: Hook }) {
  const key = `${repoKey(repo, 'deliveries')}${hook.id}`;
  const list = useLocalResource(key, () => listDeliveries(repo.owner, repo.name, hook.id));
  const [open, setOpen] = useState<number | null>(null);
  const [busy, setBusy] = useState<'ping' | 'test' | 'reload' | null>(null);

  const reload = async () => {
    const items = await listDeliveries(repo.owner, repo.name, hook.id);
    list.update(() => items);
  };
  /** Deliveries are sent by a background job: refresh now and once more shortly after. */
  const reloadSoon = async () => {
    await reload();
    setTimeout(() => void reload().catch(() => undefined), 1500);
  };
  const run = async (what: 'ping' | 'test' | 'reload') => {
    setBusy(what);
    try {
      if (what === 'ping') {
        await pingHook(repo.owner, repo.name, hook.id);
        toast({ kind: 'success', title: 'Ping sent', description: 'A ping event was queued for delivery.' });
      } else if (what === 'test') {
        await testHook(repo.owner, repo.name, hook.id);
        toast({ kind: 'success', title: hook.events.includes('push') || hook.events.includes('*') ? 'Test push sent' : 'This hook does not subscribe to push events' });
      }
      await reloadSoon();
    } catch (e) {
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };

  return (
    <Section
      title="Recent Deliveries"
      actions={
        <>
          <Button size="sm" leadingIcon={ZapIcon} loading={busy === 'ping'} onClick={() => void run('ping')}>
            Ping
          </Button>
          <Button size="sm" loading={busy === 'test'} onClick={() => void run('test')}>
            Test push
          </Button>
          <Button size="sm" variant="ghost" leadingIcon={SyncIcon} loading={busy === 'reload'} aria-label="Refresh deliveries" onClick={() => void run('reload')}>
            Refresh
          </Button>
        </>
      }
    >
      {list.error ? <LoadError error={list.error} /> : null}
      {!list.data ? (
        list.error ? null : <ListSkeleton rows={3} />
      ) : list.data.length === 0 ? (
        <EmptyState icon={WebhookIcon} title="No deliveries yet">
          Send a ping to check that your endpoint is reachable.
        </EmptyState>
      ) : (
        <ul className={styles.deliveries} aria-label="Recent deliveries">
          {list.data.map((d) => (
            <DeliveryRow
              key={d.id}
              repo={repo}
              hookId={hook.id}
              d={d}
              open={open === d.id}
              onToggle={() => setOpen(open === d.id ? null : d.id)}
              onRedelivered={() => void reloadSoon().catch(() => undefined)}
            />
          ))}
        </ul>
      )}
    </Section>
  );
}

function DeliveryRow({
  repo,
  hookId,
  d,
  open,
  onToggle,
  onRedelivered,
}: {
  repo: Repo;
  hookId: number;
  d: HookDeliveryItem;
  open: boolean;
  onToggle: () => void;
  onRedelivered: () => void;
}) {
  const bodyId = useId();
  return (
    <li className={styles.delivery}>
      <button type="button" className={styles.deliveryHead} aria-expanded={open} aria-controls={bodyId} onClick={onToggle}>
        <ChevronRightIcon size={16} className={cx(styles.chev, open && styles.chevOpen)} />
        <StatusIcon status={d.status} label={d.status} />
        <span className={styles.mono}>{d.guid}</span>
        <Pill>{d.action ? `${d.event}.${d.action}` : d.event}</Pill>
        {d.redelivery && <Pill tone="accent">redelivery</Pill>}
        <span className={styles.spacer} />
        <span className={styles.small}>
          <RelativeTime date={d.delivered_at} />
        </span>
      </button>
      {open && (
        <div id={bodyId} className={styles.deliveryBody}>
          <DeliveryDetail repo={repo} hookId={hookId} id={d.id} onRedelivered={onRedelivered} />
        </div>
      )}
    </li>
  );
}

function DeliveryDetail({ repo, hookId, id, onRedelivered }: { repo: Repo; hookId: number; id: number; onRedelivered: () => void }) {
  const detail = useResource(`${repoKey(repo, 'delivery')}${hookId}/${id}`, () => getDelivery(repo.owner, repo.name, hookId, id));
  const [tab, setTab] = useState<'request' | 'response'>('request');
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  useEffect(() => setTab('request'), [id]);
  if (detail.error) return <LoadError error={detail.error} />;
  const d = detail.data;
  if (!d) return <ListSkeleton rows={2} />;
  const headers = (h: Record<string, string>) =>
    Object.entries(h)
      .map(([k, v]) => `${k}: ${v}`)
      .join('\n') || '(none)';
  return (
    <>
      <div className={styles.row}>
        <Tabs
          size="sm"
          items={[
            { id: 'request', label: 'Request' },
            { id: 'response', label: `Response ${d.status_code || ''}`.trim() },
          ]}
          value={tab}
          onChange={(t) => setTab(t as 'request' | 'response')}
        />
        <span className={styles.spacer} />
        <span className={styles.small}>
          {deliveryOk(d.status) ? 'Completed' : d.status} in {d.duration.toFixed(2)}s
        </span>
        <Button size="sm" leadingIcon={SyncIcon} loading={busy} onClick={() => setConfirm(true)}>
          Redeliver
        </Button>
      </div>
      {tab === 'request' ? (
        <>
          <div className={styles.subhead}>Headers</div>
          <pre className={styles.pre}>{headers(d.request.headers)}</pre>
          <div className={styles.subhead}>Payload</div>
          <pre className={styles.pre}>{JSON.stringify(d.request.payload, null, 2)}</pre>
        </>
      ) : (
        <>
          <div className={styles.subhead}>Headers</div>
          <pre className={styles.pre}>{headers(d.response.headers)}</pre>
          <div className={styles.subhead}>Body</div>
          <pre className={styles.pre}>{d.response.payload || '(empty)'}</pre>
        </>
      )}
      <ConfirmDialog
        open={confirm}
        onClose={() => setConfirm(false)}
        title="Redeliver payload?"
        confirmLabel="Yes, redeliver this payload"
        danger={false}
        onConfirm={async () => {
          setBusy(true);
          try {
            await redeliver(repo.owner, repo.name, hookId, id);
            toast({ kind: 'success', title: 'Redelivery queued' });
            onRedelivered();
          } finally {
            setBusy(false);
          }
        }}
      >
        <p className={styles.muted}>
          The <code>{d.event}</code> payload <code>{d.guid}</code> will be delivered again to <code>{d.url}</code> with the hook's current settings.
        </p>
      </ConfirmDialog>
    </>
  );
}

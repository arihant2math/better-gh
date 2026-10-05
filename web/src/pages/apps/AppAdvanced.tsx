/**
 * "Advanced" tab of a GitHub App's settings (P46): webhook delivery
 * settings (content type, SSL verification, last response) and the app
 * hook's recent deliveries with request/response details and redelivery.
 * Lazy-loaded from AppsManager.
 */
import { useState } from 'react';
import { getAppHook, getHookDelivery, listHookDeliveries, redeliverHook, updateApp, type AppDetail, type HookDelivery } from '../../api/apps';
import { invalidate, useResource } from '../../api/cache';
import { apiFieldErrors, Banner, ButtonRow, Checkbox, FormStack, ItemList, ItemRow, Pill, Section } from '../../components/settings/kit';
import { Button } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { AlertIcon, CheckCircleIcon, SyncIcon, XCircleIcon } from '../../ui/icons';
import { Field, Select } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import styles from './apps.module.css';

const hookKey = (slug: string) => `apps:hook:${slug}`;
const deliveriesKey = (slug: string, filter: string) => `apps:deliveries:${slug}:${filter}`;

export default function AppAdvanced({ app, onUpdated }: { app: AppDetail; onUpdated: (a: AppDetail) => void }) {
  return (
    <>
      <DeliverySettings app={app} onUpdated={onUpdated} />
      <Deliveries slug={app.slug} hasUrl={!!app.webhook_url} />
    </>
  );
}

function DeliverySettings({ app, onUpdated }: { app: AppDetail; onUpdated: (a: AppDetail) => void }) {
  const hook = useResource(hookKey(app.slug), () => getAppHook(app.slug));
  const [contentType, setContentType] = useState(app.webhook_content_type);
  const [insecure, setInsecure] = useState(app.webhook_insecure_ssl);
  const [busy, setBusy] = useState(false);
  const dirty = contentType !== app.webhook_content_type || insecure !== app.webhook_insecure_ssl;
  const last = hook.data?.last_response;
  const save = async () => {
    setBusy(true);
    try {
      const updated = await updateApp(app.slug, {
        webhook_content_type: contentType,
        webhook_insecure_ssl: insecure,
      });
      invalidate(hookKey(app.slug));
      onUpdated(updated);
      toast({ kind: 'success', title: 'Webhook settings saved' });
    } catch (e) {
      toast({ kind: 'error', title: apiFieldErrors(e).message });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section
      title="Webhook delivery"
      description="Events from every installation are delivered to the app's webhook URL, signed with its secret (X-Hub-Signature-256) and carrying an installation object."
    >
      <FormStack>
        <div className={styles.kv}>
          <span>URL</span>
          <span>{app.webhook_url ? <code>{app.webhook_url}</code> : <em>Not set</em>}</span>
          <span>Status</span>
          <span>
            {!app.webhook_active ? (
              <Pill>Inactive</Pill>
            ) : !last ? (
              <Skeleton width={80} />
            ) : last.status === 'active' ? (
              <Pill tone="success">Last delivery succeeded</Pill>
            ) : last.status === 'unused' ? (
              <Pill>No deliveries yet</Pill>
            ) : (
              <Pill tone="danger">{last.message ?? 'Last delivery failed'}</Pill>
            )}
          </span>
        </div>
        <Field label="Content type" htmlFor="app-hook-content-type">
          <Select id="app-hook-content-type" value={contentType} onChange={(e) => setContentType(e.target.value as 'json' | 'form')}>
            <option value="json">application/json</option>
            <option value="form">application/x-www-form-urlencoded</option>
          </Select>
        </Field>
        <Checkbox
          checked={!insecure}
          onChange={(c) => setInsecure(!c)}
          label="Enable SSL verification"
          description="Verify the TLS certificate of the webhook URL. Disabling it is not recommended."
        />
        {insecure && (
          <Banner tone="warning" icon={AlertIcon}>
            Deliveries will not verify the receiver’s certificate.
          </Banner>
        )}
        <ButtonRow>
          <Button variant="primary" loading={busy} disabled={!dirty} onClick={() => void save()}>
            Save webhook settings
          </Button>
        </ButtonRow>
      </FormStack>
    </Section>
  );
}

function Deliveries({ slug, hasUrl }: { slug: string; hasUrl: boolean }) {
  const [filter, setFilter] = useState<'' | 'success' | 'failure'>('');
  const res = useResource(deliveriesKey(slug, filter), () => listHookDeliveries(slug, filter || undefined));
  const [open, setOpen] = useState<number | null>(null);
  const refresh = () => {
    invalidate(deliveriesKey(slug, filter));
    invalidate(hookKey(slug));
  };
  return (
    <Section
      title="Recent deliveries"
      description={hasUrl ? 'Deliveries of the last days. Select one to see its request and response, or redeliver it.' : 'Set a webhook URL to receive events.'}
      actions={
        <ButtonRow>
          <Select aria-label="Filter deliveries" value={filter} onChange={(e) => setFilter(e.target.value as typeof filter)}>
            <option value="">All</option>
            <option value="success">Succeeded</option>
            <option value="failure">Failed</option>
          </Select>
          <Button size="sm" leadingIcon={SyncIcon} onClick={refresh}>
            Refresh
          </Button>
        </ButtonRow>
      }
    >
      {res.data ? (
        <ItemList aria-label="Recent deliveries" empty="No deliveries yet.">
          {res.data.map((d) => (
            <ItemRow
              key={d.id}
              icon={d.status === 'OK' ? CheckCircleIcon : XCircleIcon}
              title={
                <button type="button" className={styles.deliveryToggle} aria-expanded={open === d.id} onClick={() => setOpen(open === d.id ? null : d.id)}>
                  <code>{d.guid}</code>
                  <span className={styles.deliveryEvent}>
                    {d.event}
                    {d.action ? `.${d.action}` : ''}
                  </span>
                </button>
              }
              meta={
                <span className={styles.meta}>
                  <span>{d.status_code ? `HTTP ${d.status_code}` : d.status}</span>
                  <span>{d.duration.toFixed(2)} s</span>
                  {d.redelivery && <Pill tone="accent">Redelivery</Pill>}
                  <RelativeTime date={d.delivered_at} />
                </span>
              }
            >
              {open === d.id && <DeliveryDetail slug={slug} id={d.id} onRedelivered={refresh} />}
            </ItemRow>
          ))}
        </ItemList>
      ) : res.error ? (
        <ItemList empty="Could not load deliveries." />
      ) : (
        <FormStack>
          <Skeleton width="60%" />
          <Skeleton width="45%" />
        </FormStack>
      )}
    </Section>
  );
}

function DeliveryDetail({ slug, id, onRedelivered }: { slug: string; id: number; onRedelivered: () => void }) {
  const res = useResource(`apps:delivery:${slug}:${id}`, () => getHookDelivery(slug, id));
  const [tab, setTab] = useState<'request' | 'response'>('request');
  const [busy, setBusy] = useState(false);
  const d: HookDelivery | undefined = res.data;
  if (!d) return res.error ? <p className={styles.hint}>Could not load this delivery.</p> : <Skeleton width="80%" />;
  const headers = tab === 'request' ? d.request.headers : d.response.headers;
  const body = tab === 'request' ? JSON.stringify(d.request.payload, null, 2) : (d.response.payload ?? '');
  return (
    <div className={styles.deliveryDetail} data-testid="delivery-detail">
      <ButtonRow>
        <Button size="sm" variant={tab === 'request' ? 'primary' : undefined} onClick={() => setTab('request')}>
          Request
        </Button>
        <Button size="sm" variant={tab === 'response' ? 'primary' : undefined} onClick={() => setTab('response')}>
          Response {d.status_code || ''}
        </Button>
        <Button
          size="sm"
          leadingIcon={SyncIcon}
          loading={busy}
          onClick={async () => {
            setBusy(true);
            try {
              await redeliverHook(slug, id);
              toast({ kind: 'success', title: 'Redelivery queued' });
              onRedelivered();
            } catch (e) {
              toast({ kind: 'error', title: apiFieldErrors(e).message });
            } finally {
              setBusy(false);
            }
          }}
        >
          Redeliver
        </Button>
      </ButtonRow>
      <h4 className={styles.deliveryHeading}>Headers</h4>
      <pre className={styles.deliveryPre}>
        {Object.entries(headers)
          .map(([k, v]) => `${k}: ${v}`)
          .join('\n') || '(none)'}
      </pre>
      <h4 className={styles.deliveryHeading}>{tab === 'request' ? 'Payload' : 'Body'}</h4>
      <pre className={styles.deliveryPre}>{body || '(empty)'}</pre>
    </div>
  );
}

/**
 * GitHub App manifest confirmation (P46): an integration posted a manifest
 * to `/settings/apps/new` (or the org variant); the server stored it and
 * sent the browser here with `?manifest=<token>`. Creating the app sends
 * the browser to the manifest's `redirect_url` with a one-time `code` the
 * integration converts into credentials. Lazy-loaded from AppsManager.
 */
import { useState } from 'react';
import { createFromManifest, getManifest } from '../../api/apps';
import { invalidate, useResource } from '../../api/cache';
import { apiFieldErrors, Banner, ButtonRow, FormStack, Pill, Section } from '../../components/settings/kit';
import { Link, navigate } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, ArrowLeftIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import styles from './apps.module.css';

export default function ManifestConfirm({ token, base }: { token: string; base: string }) {
  const res = useResource(`apps:manifest:${token}`, () => getManifest(token));
  const [name, setName] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const m = res.data;
  const back = (
    <Link to={base} className={styles.back}>
      <ArrowLeftIcon size={16} /> GitHub Apps
    </Link>
  );
  if (!m) {
    return (
      <>
        {back}
        {res.error ? (
          <EmptyState title="This app manifest expired">Manifests can be confirmed for one hour. Start again from the integration’s page.</EmptyState>
        ) : (
          <FormStack>
            <Skeleton width="40%" height={24} />
            <Skeleton width="70%" />
          </FormStack>
        )}
      </>
    );
  }
  const appName = name ?? m.name ?? '';
  const create = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await createFromManifest(token, appName.trim() || undefined);
      invalidate(`apps:list:${m.owner.login}`);
      const sameOrigin = r.redirect_url.startsWith(window.location.origin);
      if (sameOrigin) navigate(r.redirect_url.slice(window.location.origin.length));
      else window.location.assign(r.redirect_url);
    } catch (e) {
      const f = apiFieldErrors(e);
      setError(f.fields.name ?? f.message);
      setBusy(false);
    }
  };
  const perms = Object.entries(m.permissions);
  return (
    <>
      {back}
      <h1 className={styles.manifestTitle}>Create GitHub App for {m.owner.login}</h1>
      <p className={styles.hint}>
        An integration at <code>{m.url}</code> asks to register a GitHub App. Review it below; after creating it you are sent back to
        {m.redirect_url ? (
          <>
            {' '}
            <code>{m.redirect_url}</code>
          </>
        ) : (
          ' the app’s settings'
        )}
        , and the integration receives the app’s private key and secrets.
      </p>
      {m.app_slug ? (
        <Banner tone="info" icon={AlertIcon}>
          This manifest was already used to create{' '}
          <Link to={`${base}/${m.app_slug}`}>
            <code>{m.app_slug}</code>
          </Link>
          .
        </Banner>
      ) : !m.can_create ? (
        <Banner tone="warning" icon={AlertIcon}>
          You can’t register GitHub Apps for {m.owner.login}.
        </Banner>
      ) : null}
      <Section title="App">
        <FormStack>
          <Field label="GitHub App name" htmlFor="manifest-name" error={error ?? undefined} hint="You can change the name the integration proposed.">
            <Input id="manifest-name" value={appName} maxLength={34} invalid={!!error} onChange={(e) => setName(e.target.value)} />
          </Field>
          <div className={styles.kv}>
            {m.description && (
              <>
                <span>Description</span>
                <span>{m.description}</span>
              </>
            )}
            <span>Homepage</span>
            <span>
              <code>{m.url}</code>
            </span>
            <span>Webhook</span>
            <span>{m.webhook_url ? <code>{m.webhook_url}</code> : <em>None</em>}</span>
            {m.callback_urls.length > 0 && (
              <>
                <span>Callback URLs</span>
                <span>
                  {m.callback_urls.map((c) => (
                    <code key={c} className={styles.block}>
                      {c}
                    </code>
                  ))}
                </span>
              </>
            )}
            <span>Visibility</span>
            <span>{m.public ? 'Public: any account can install it' : 'Private: only this account can install it'}</span>
          </div>
        </FormStack>
      </Section>
      <Section title="Permissions and events">
        <div className={styles.manifestPerms} aria-label="Requested permissions">
          <Pill>metadata: read</Pill>
          {perms.map(([k, v]) => (
            <Pill key={k} tone={v === 'read' ? 'neutral' : 'accent'}>
              {k}: {v}
            </Pill>
          ))}
        </div>
        {m.events.length > 0 && (
          <p className={styles.hint}>
            Subscribes to{' '}
            {m.events.map((e, i) => (
              <span key={e}>
                {i > 0 && ', '}
                <code>{e}</code>
              </span>
            ))}
            .
          </p>
        )}
      </Section>
      <ButtonRow>
        <Button variant="primary" loading={busy} disabled={!m.can_create || !!m.app_slug || !appName.trim()} onClick={() => void create()}>
          Create GitHub App for {m.owner.login}
        </Button>
        <Button onClick={() => navigate(base)}>Cancel</Button>
      </ButtonRow>
    </>
  );
}

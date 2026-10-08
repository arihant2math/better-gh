/**
 * GitHub App registrations of one account (user settings `/settings/apps`,
 * org settings `/organizations/:org/settings/apps`): list, register,
 * edit (permissions, events, webhook), private keys, client secrets,
 * delete. The "Advanced" tab (webhook deliveries) and the manifest
 * confirmation (`new?manifest=…`) are lazy chunks (P46).
 */
import { lazy, Suspense, useId, useState, type FormEvent } from 'react';
import {
  createApp,
  createClientSecret,
  createKey,
  deleteApp,
  deleteClientSecret,
  deleteKey,
  getApp,
  listApps,
  updateApp,
  type Access,
  type AppDetail,
  type AppKey,
  type ClientSecret,
} from '../../api/apps';
import { invalidate, useResource } from '../../api/cache';
import {
  apiFieldErrors,
  Banner,
  ButtonRow,
  Checkbox,
  ConfirmDialog,
  CopyButton,
  downloadText,
  FormStack,
  ItemList,
  ItemRow,
  PageHeader,
  Pill,
  Section,
} from '../../components/settings/kit';
import { Link, navigate, useQuery } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { TabNav } from '../../ui/Tabs';
import { AlertIcon, AppsIcon, ArrowLeftIcon, DownloadIcon, KeyIcon, PlusIcon, TrashIcon } from '../../ui/icons';
import { Field, Input, Select, Textarea } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import styles from './apps.module.css';
import {
  ACCESS_LABEL,
  EMPTY_APP,
  EVENT_CHOICES,
  fromApp,
  levels,
  PERMISSION_GROUPS,
  slugify,
  toInput,
  validateAppForm,
  type AppFormValues,
} from './logic';
import { onReset } from '../../api/reset';

const listKey = (owner: string) => `apps:list:${owner}`;
const appKey = (slug: string) => `apps:app:${slug}`;

/** PEM of a key generated in this page view, kept in memory only. */
let freshKey: { slug: string; key: AppKey } | null = null;
onReset(() => (freshKey = null));

export function AppIcon({ name, size = 40 }: { name: string; size?: number }) {
  return (
    <span className={styles.appIcon} style={{ width: size, height: size, fontSize: Math.round(size * 0.42) }} aria-hidden>
      {name.charAt(0).toUpperCase()}
    </span>
  );
}

const AppAdvanced = lazy(() => import('./AppAdvanced'));
const ManifestConfirm = lazy(() => import('./ManifestConfirm'));

const loading = (
  <FormStack>
    <Skeleton width="40%" height={24} />
    <Skeleton width="70%" />
  </FormStack>
);

/** `sub` = path segments after `base` (`[]`, `['new']`, `[slug]`, `[slug, 'advanced']`). */
export function AppsManager({ owner, base, sub }: { owner: string; base: string; sub: string[] }) {
  const manifest = useQuery().get('manifest');
  if (sub[0] === 'new' && manifest)
    return (
      <Suspense fallback={loading}>
        <ManifestConfirm token={manifest} base={base} />
      </Suspense>
    );
  if (sub[0] === 'new') return <NewApp owner={owner} base={base} />;
  if (sub[0]) return <AppPageDetail key={sub[0]} slug={sub[0]} owner={owner} base={base} tab={sub[1] === 'advanced' ? 'advanced' : 'general'} />;
  return <AppList owner={owner} base={base} />;
}

function AppList({ owner, base }: { owner: string; base: string }) {
  const res = useResource(listKey(owner), () => listApps(owner));
  const items = res.data;
  return (
    <>
      <PageHeader
        title="GitHub Apps"
        description="GitHub Apps act as themselves (a bot account) with fine-grained permissions on the repositories they are installed on, and receive webhooks."
        actions={
          <Button variant="primary" leadingIcon={PlusIcon} onClick={() => navigate(`${base}/new`)}>
            New GitHub App
          </Button>
        }
      />
      {items ? (
        items.length === 0 ? (
          <EmptyState
            icon={AppsIcon}
            title="No GitHub Apps"
            action={
              <Button variant="primary" onClick={() => navigate(`${base}/new`)}>
                Register a GitHub App
              </Button>
            }
          >
            Build integrations that authenticate with their own identity and installation tokens.
          </EmptyState>
        ) : (
          <ItemList aria-label="GitHub Apps">
            {items.map((a) => (
              <ItemRow
                key={a.id}
                leading={<AppIcon name={a.name} />}
                title={
                  <Link to={`${base}/${a.slug}`} className={styles.titleLink}>
                    {a.name}
                  </Link>
                }
                meta={
                  <span className={styles.meta}>
                    <code>{a.slug}</code>
                    <span>{a.public ? 'Public' : 'Private'}</span>
                    <span>
                      {a.installations_count ?? 0} installation{a.installations_count === 1 ? '' : 's'}
                    </span>
                  </span>
                }
                actions={
                  <Button size="sm" onClick={() => navigate(`${base}/${a.slug}`)}>
                    Edit
                  </Button>
                }
              />
            ))}
          </ItemList>
        )
      ) : res.error ? (
        <ItemList empty="Could not load GitHub Apps." />
      ) : (
        <FormStack>
          <Skeleton width="50%" />
          <Skeleton width="70%" />
        </FormStack>
      )}
    </>
  );
}

// ------------------------------------------------------------------ form

function PermissionsEditor({ value, onChange }: { value: AppFormValues['permissions']; onChange: (v: AppFormValues['permissions']) => void }) {
  const id = useId();
  return (
    <div className={styles.permGroups}>
      {PERMISSION_GROUPS.map((g) => (
        <fieldset key={g.title} className={styles.permGroup}>
          <legend>{g.title}</legend>
          {g.items.map((p) => (
            <div key={p.key} className={styles.permRow}>
              <label htmlFor={`${id}-${p.key}`} className={styles.permText}>
                <strong>{p.label}</strong>
                <span>{p.description}</span>
              </label>
              <Select
                id={`${id}-${p.key}`}
                aria-label={g.title.startsWith('Repository') ? p.label : `${p.label} (${g.title.split(' ')[0]!.toLowerCase()})`}
                value={value[p.key] ?? ''}
                onChange={(e) => {
                  const next = { ...value };
                  if (e.target.value) next[p.key] = e.target.value as Access;
                  else delete next[p.key];
                  onChange(next);
                }}
              >
                <option value="">No access</option>
                {levels(p.max).map((l) => (
                  <option key={l} value={l}>
                    {ACCESS_LABEL[l]}
                  </option>
                ))}
              </Select>
            </div>
          ))}
        </fieldset>
      ))}
      <p className={styles.hint}>
        <strong>Metadata</strong> (read-only) is always granted: it lets the app search repositories and list collaborators.
      </p>
    </div>
  );
}

function AppForm({
  initial,
  editing,
  secretSet,
  submitLabel,
  onSubmit,
  onCancel,
}: {
  initial: AppFormValues;
  editing?: boolean;
  secretSet?: boolean;
  submitLabel: string;
  onSubmit: (v: AppFormValues) => Promise<void>;
  onCancel?: () => void;
}) {
  const id = useId();
  const [v, setV] = useState(initial);
  const [errors, setErrors] = useState<Partial<Record<keyof AppFormValues | 'form', string>>>({});
  const [busy, setBusy] = useState(false);
  const dirty = JSON.stringify(v) !== JSON.stringify(initial);
  const set = <K extends keyof AppFormValues>(k: K, val: AppFormValues[K]) => {
    setV((x) => ({ ...x, [k]: val }));
    setErrors((e) => ({ ...e, [k]: undefined, form: undefined }));
  };
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    const errs = validateAppForm(v);
    setErrors(errs);
    const first = Object.keys(errs)[0];
    if (first) {
      document.getElementById(`${id}-${first}`)?.focus();
      return;
    }
    setBusy(true);
    try {
      await onSubmit(v);
    } catch (x) {
      const f = apiFieldErrors(x);
      const nice: typeof errors = {};
      for (const [k, msg] of Object.entries(f.fields)) if (k in v) nice[k as keyof AppFormValues] = msg;
      if (!Object.keys(nice).length) nice.form = f.message;
      setErrors(nice);
    } finally {
      setBusy(false);
    }
  };
  const slug = slugify(v.name);
  return (
    <form onSubmit={(e) => void submit(e)} noValidate aria-label={submitLabel}>
      <FormStack wide>
        <Field
          label="GitHub App name"
          htmlFor={`${id}-name`}
          error={errors.name}
          hint={slug ? <>Its bot account will be <code>{slug}[bot]</code>.</> : 'The name of your app (at most 34 characters).'}
        >
          <Input id={`${id}-name`} value={v.name} maxLength={34} invalid={!!errors.name} autoFocus={!editing} onChange={(e) => set('name', e.target.value)} />
        </Field>
        <Field label="Description" htmlFor={`${id}-description`} hint="Optional. Shown on the app's public page.">
          <Textarea id={`${id}-description`} rows={3} value={v.description} maxLength={400} onChange={(e) => set('description', e.target.value)} />
        </Field>
        <Field label="Homepage URL" htmlFor={`${id}-homepage_url`} error={errors.homepage_url}>
          <Input
            id={`${id}-homepage_url`}
            type="url"
            value={v.homepage_url}
            invalid={!!errors.homepage_url}
            placeholder="https://example.com"
            onChange={(e) => set('homepage_url', e.target.value)}
          />
        </Field>
        <Field label="Callback URLs" htmlFor={`${id}-callback_urls`} error={errors.callback_urls} hint="One per line (up to 10). Users are sent here after authorizing the app.">
          <Textarea id={`${id}-callback_urls`} rows={2} value={v.callback_urls} onChange={(e) => set('callback_urls', e.target.value)} />
        </Field>
        <Field label="Setup URL" htmlFor={`${id}-setup_url`} error={errors.setup_url} hint="Optional. Users are redirected here after installing the app (with installation_id and setup_action).">
          <Input id={`${id}-setup_url`} type="url" value={v.setup_url} invalid={!!errors.setup_url} onChange={(e) => set('setup_url', e.target.value)} />
        </Field>
        <Checkbox
          checked={v.setup_on_update}
          onChange={(c) => set('setup_on_update', c)}
          label="Redirect on update"
          description="Also redirect to the setup URL after installations are updated (repositories added or removed)."
        />
        <h3 className={styles.formHeading}>Webhook</h3>
        <Checkbox checked={v.webhook_active} onChange={(c) => set('webhook_active', c)} label="Active" description="Deliver events to the webhook URL." />
        <Field label="Webhook URL" htmlFor={`${id}-webhook_url`} error={errors.webhook_url}>
          <Input id={`${id}-webhook_url`} type="url" value={v.webhook_url} invalid={!!errors.webhook_url} onChange={(e) => set('webhook_url', e.target.value)} />
        </Field>
        <Field
          label="Webhook secret"
          htmlFor={`${id}-webhook_secret`}
          hint={editing && secretSet ? 'A secret is set. Enter a new one to replace it.' : 'Optional. Used to sign deliveries (X-Hub-Signature-256).'}
        >
          <Input id={`${id}-webhook_secret`} type="password" autoComplete="new-password" value={v.webhook_secret} onChange={(e) => set('webhook_secret', e.target.value)} />
        </Field>
        <h3 className={styles.formHeading}>Permissions</h3>
        <PermissionsEditor value={v.permissions} onChange={(p) => set('permissions', p)} />
        {editing && (
          <Banner tone="info" icon={AlertIcon}>
            Existing installations keep their current permissions until an account owner accepts the new ones.
          </Banner>
        )}
        <h3 className={styles.formHeading}>Subscribe to events</h3>
        <div className={styles.events} role="group" aria-label="Events">
          {EVENT_CHOICES.map((ev) => (
            <Checkbox
              key={ev}
              checked={v.events.includes(ev)}
              onChange={(c) => set('events', c ? [...v.events, ev] : v.events.filter((x) => x !== ev))}
              label={<code>{ev}</code>}
            />
          ))}
        </div>
        <h3 className={styles.formHeading}>Where can this GitHub App be installed?</h3>
        <Checkbox
          checked={v.public}
          onChange={(c) => set('public', c)}
          label="Any account"
          description="Public: any user or organization can install it. Otherwise only the owning account can."
        />
        {errors.form && (
          <Banner tone="danger" icon={AlertIcon}>
            {errors.form}
          </Banner>
        )}
        <ButtonRow>
          <Button type="submit" variant="primary" loading={busy} disabled={editing && !dirty}>
            {submitLabel}
          </Button>
          {onCancel && <Button onClick={onCancel}>Cancel</Button>}
        </ButtonRow>
      </FormStack>
    </form>
  );
}

function NewApp({ owner, base }: { owner: string; base: string }) {
  return (
    <>
      <Link to={base} className={styles.back}>
        <ArrowLeftIcon size={16} /> GitHub Apps
      </Link>
      <PageHeader title="Register new GitHub App" description={`Owned by ${owner}.`} />
      <AppForm
        initial={EMPTY_APP}
        submitLabel="Create GitHub App"
        onCancel={() => navigate(base)}
        onSubmit={async (v) => {
          const app = await createApp({ ...toInput(v), owner });
          invalidate(listKey(owner));
          toast({ kind: 'success', title: `Registered ${app.name}`, description: 'Generate a private key to authenticate as the app.' });
          navigate(`${base}/${app.slug}`);
        }}
      />
    </>
  );
}

// ------------------------------------------------------------------ detail

function AppPageDetail({ slug, owner, base, tab }: { slug: string; owner: string; base: string; tab: 'general' | 'advanced' }) {
  const res = useResource(appKey(slug), () => getApp(slug));
  const [local, setLocal] = useState<AppDetail | undefined>(undefined);
  const app = local ?? res.data;
  const [newKey, setNewKey] = useState<AppKey | null>(() => (freshKey?.slug === slug ? freshKey.key : null));
  const [keyBusy, setKeyBusy] = useState(false);
  const [confirmKey, setConfirmKey] = useState<AppKey | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  if (!app) {
    return (
      <>
        <Link to={base} className={styles.back}>
          <ArrowLeftIcon size={16} /> GitHub Apps
        </Link>
        {res.error ? (
          <EmptyState title="GitHub App not found">It may have been deleted, or you don’t administer its owner.</EmptyState>
        ) : (
          <FormStack>
            <Skeleton width="40%" height={24} />
            <Skeleton width="70%" />
          </FormStack>
        )}
      </>
    );
  }

  const refresh = async () => {
    invalidate(appKey(app.slug));
    invalidate(listKey(owner));
    setLocal(await getApp(app.slug));
  };

  const generate = async () => {
    setKeyBusy(true);
    try {
      const k = await createKey(app.slug);
      freshKey = { slug: app.slug, key: k };
      setNewKey(k);
      if (k.pem) downloadText(`${app.slug}.${new Date().toISOString().slice(0, 10)}.private-key.pem`, k.pem);
      await refresh();
    } catch (e) {
      toast({ kind: 'error', title: apiFieldErrors(e).message });
    } finally {
      setKeyBusy(false);
    }
  };

  return (
    <>
      <Link to={base} className={styles.back}>
        <ArrowLeftIcon size={16} /> GitHub Apps
      </Link>
      <PageHeader
        title={
          <span className={styles.header}>
            <AppIcon name={app.name} />
            {app.name}
          </span>
        }
        description={app.description ?? undefined}
        actions={
          <Button onClick={() => navigate(`/apps/${app.slug}`)} leadingIcon={DownloadIcon}>
            Install App
          </Button>
        }
      />
      <TabNav
        aria-label="GitHub App settings"
        current={tab}
        className={styles.tabs}
        items={[
          { id: 'general', label: 'General', href: `${base}/${app.slug}` },
          { id: 'advanced', label: 'Advanced', href: `${base}/${app.slug}/advanced` },
        ]}
      />
      {tab === 'advanced' ? (
        <Suspense fallback={loading}>
          <AppAdvanced
            app={app}
            onUpdated={(a) => {
              invalidate(appKey(a.slug));
              setLocal(a);
            }}
          />
        </Suspense>
      ) : (
        <>
          <Section title="About">
            <div className={styles.kv}>
              <span>App ID</span>
              <span>
                <code data-testid="app-id">{app.id}</code>
                <CopyButton value={String(app.id)} />
              </span>
              <span>Client ID</span>
              <span>
                <code>{app.client_id}</code>
                <CopyButton value={app.client_id} />
              </span>
              <span>Bot account</span>
              <span>
                <Link to={`/${app.bot.login}`}>{app.bot.login}</Link>
              </span>
              <span>Public page</span>
              <span>
                <Link to={`/apps/${app.slug}`}>{app.html_url}</Link>
              </span>
              <span>Installations</span>
              <span>{app.installations_count ?? 0}</span>
            </div>
          </Section>
          <Section
            title="Private keys"
            description="Sign JWTs with a private key to authenticate as the app (iss = App ID, RS256, at most 10 minutes). The key is downloaded once and never stored here."
            actions={
              <Button size="sm" leadingIcon={KeyIcon} loading={keyBusy} onClick={() => void generate()}>
                Generate a private key
              </Button>
            }
          >
            {newKey?.pem && (
              <div className={styles.pemBox} role="status" aria-label="New private key">
                <div className={styles.pemWarn}>
                  <AlertIcon size={16} /> Your private key was downloaded. Store it safely: it can’t be shown again.
                </div>
                <ButtonRow>
                  <Button size="sm" leadingIcon={DownloadIcon} onClick={() => downloadText(`${app.slug}.private-key.pem`, newKey.pem!)}>
                    Download again
                  </Button>
                  <CopyButton value={newKey.pem} label="Copy PEM" />
                </ButtonRow>
              </div>
            )}
            <ItemList aria-label="Private keys" empty="No private keys. Generate one to authenticate as this app.">
              {app.keys.map((k) => (
                <ItemRow
                  key={k.id}
                  icon={KeyIcon}
                  title={<code className={styles.fingerprint}>{k.fingerprint}</code>}
                  meta={
                    <>
                      Added {new Date(k.created_at).toLocaleDateString()} {newKey?.id === k.id && <Pill tone="success">New</Pill>}
                    </>
                  }
                  actions={
                    <Button size="sm" variant="danger" aria-label={`Delete key ${k.fingerprint}`} onClick={() => setConfirmKey(k)}>
                      Delete
                    </Button>
                  }
                />
              ))}
            </ItemList>
          </Section>
          <ClientSecrets app={app} onChanged={() => void refresh()} />
          <Section title="General">
            <AppForm
              key={app.updated_at}
              editing
              secretSet={app.webhook_secret_set}
              initial={fromApp(app)}
              submitLabel="Save changes"
              onSubmit={async (v) => {
                const updated = await updateApp(app.slug, toInput(v, fromApp(app)));
                invalidate(listKey(owner));
                invalidate(appKey(app.slug));
                setLocal(updated);
                toast({ kind: 'success', title: 'GitHub App updated' });
                if (updated.slug !== app.slug) navigate(`${base}/${updated.slug}`);
              }}
            />
          </Section>
          <Section danger title="Danger zone">
            <div className={styles.dangerRow}>
              <div>
                <strong>Delete this GitHub App</strong>
                <p className={styles.hint}>It is uninstalled everywhere and its tokens stop working. Content made by its bot is shown as a ghost.</p>
              </div>
              <Button variant="danger" leadingIcon={TrashIcon} onClick={() => setConfirmDelete(true)}>
                Delete GitHub App
              </Button>
            </div>
          </Section>
        </>
      )}
      <ConfirmDialog
        open={!!confirmKey}
        onClose={() => setConfirmKey(null)}
        title="Delete private key?"
        confirmLabel="Delete key"
        onConfirm={async () => {
          await deleteKey(app.slug, confirmKey!.id);
          await refresh();
          toast({ kind: 'success', title: 'Private key deleted' });
        }}
      >
        <p>JWTs signed with this key stop working immediately.</p>
      </ConfirmDialog>
      <ConfirmDialog
        open={confirmDelete}
        onClose={() => setConfirmDelete(false)}
        title={`Delete ${app.name}`}
        confirmLabel="Delete this GitHub App"
        confirmText={app.slug}
        onConfirm={async () => {
          await deleteApp(app.slug);
          invalidate(listKey(owner));
          toast({ kind: 'success', title: `Deleted ${app.name}` });
          navigate(base);
        }}
      >
        <Banner tone="warning" icon={AlertIcon}>
          Deleting <strong>{app.name}</strong> removes {app.installations_count ?? 0} installation(s).
        </Banner>
      </ConfirmDialog>
    </>
  );
}

/** Client secrets: OAuth credentials for user-to-server tokens (P46). */
function ClientSecrets({ app, onChanged }: { app: AppDetail; onChanged: () => void }) {
  const [fresh, setFresh] = useState<ClientSecret | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<ClientSecret | null>(null);
  const generate = async () => {
    setBusy(true);
    try {
      setFresh(await createClientSecret(app.slug));
      onChanged();
    } catch (e) {
      toast({ kind: 'error', title: apiFieldErrors(e).message });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section
      title="Client secrets"
      description={
        <>
          Exchange OAuth codes for user-to-server tokens at <code>/login/oauth/access_token</code> with the client ID and a secret.
        </>
      }
      actions={
        <Button size="sm" leadingIcon={KeyIcon} loading={busy} onClick={() => void generate()}>
          Generate a new client secret
        </Button>
      }
    >
      {fresh?.client_secret && (
        <div className={styles.pemBox} role="status" aria-label="New client secret">
          <div className={styles.pemWarn}>
            <AlertIcon size={16} /> Copy your new client secret now: it can’t be shown again.
          </div>
          <ButtonRow>
            <code className={styles.fingerprint} data-testid="client-secret">
              {fresh.client_secret}
            </code>
            <CopyButton value={fresh.client_secret} />
          </ButtonRow>
        </div>
      )}
      <ItemList aria-label="Client secrets" empty="No client secrets. Generate one to use the app's OAuth flow.">
        {(app.client_secrets ?? []).map((c) => (
          <ItemRow
            key={c.id}
            icon={KeyIcon}
            title={<code className={styles.fingerprint}>*****{c.last_eight}</code>}
            meta={
              <>
                Added {new Date(c.created_at).toLocaleDateString()} · {c.last_used_at ? `Last used ${new Date(c.last_used_at).toLocaleDateString()}` : 'Never used'}
              </>
            }
            actions={
              <Button size="sm" variant="danger" aria-label={`Delete client secret ending ${c.last_eight}`} onClick={() => setConfirm(c)}>
                Delete
              </Button>
            }
          />
        ))}
      </ItemList>
      <ConfirmDialog
        open={!!confirm}
        onClose={() => setConfirm(null)}
        title="Delete client secret?"
        confirmLabel="Delete secret"
        onConfirm={async () => {
          await deleteClientSecret(app.slug, confirm!.id);
          if (fresh?.id === confirm!.id) setFresh(null);
          onChanged();
          toast({ kind: 'success', title: 'Client secret deleted' });
        }}
      >
        <p>Token exchanges using this secret stop working immediately.</p>
      </ConfirmDialog>
    </Section>
  );
}

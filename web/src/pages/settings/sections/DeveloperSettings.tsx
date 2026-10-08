import { useEffect, useId, useRef, useState, type FormEvent } from 'react';
import { invalidate, useResource } from '../../../api/cache';
import { createApp, deleteApp, getApp, listApps, regenerateSecret, updateApp, type OAuthApp } from '../../../api/developerSettings';
import {
  apiFieldErrors,
  Banner,
  ButtonRow,
  Checkbox,
  ConfirmDialog,
  CopyButton,
  FormStack,
  ItemList,
  PageHeader,
  Section,
} from '../../../components/settings/kit';
import { Link, navigate, useLocation } from '../../../router';
import { Button } from '../../../ui/Button';
import { EmptyState, Skeleton } from '../../../ui/EmptyState';
import { AlertIcon, ArrowLeftIcon, CodeIcon, PlusIcon, TrashIcon } from '../../../ui/icons';
import { Field, Input, Textarea } from '../../../ui/Input';
import { toast } from '../../../ui/Toast';
import { ListSkeleton, OneTimeSecret, subPath } from '../developer/common';
import styles from '../developer/developer.module.css';
import { formatDate, validateApp, type AppFormValues } from '../developer/logic';
import { useList } from '../../../api/useList';
import { onReset } from '../../../api/reset';

const LIST_KEY = 'dev:apps';

/** Client secret returned by create/regenerate, kept in memory only. */
let freshSecret: { appId: number; secret: string } | null = null;
onReset(() => (freshSecret = null));

/** `/settings/developers`, `/settings/developers/new`, `/settings/developers/{id}`. */
export default function DeveloperSettings() {
  const { pathname } = useLocation();
  const seg = subPath(pathname)[0];
  if (seg === 'new') return <NewApp />;
  if (seg && /^\d+$/.test(seg)) return <AppDetail key={seg} id={Number(seg)} />;
  return <AppList />;
}

function AppList() {
  const list = useList<OAuthApp>(LIST_KEY, listApps);
  const items = list.items;
  return (
    <>
      <PageHeader
        title="OAuth apps"
        description="OAuth apps let other tools sign people in with their account here and act on their behalf, within the scopes they grant."
        actions={
          <Button variant="primary" leadingIcon={PlusIcon} onClick={() => navigate('/settings/developers/new')}>
            New OAuth app
          </Button>
        }
      />
      {items ? (
        items.length === 0 ? (
          <EmptyState
            icon={CodeIcon}
            title="No OAuth applications"
            action={
              <Button variant="primary" onClick={() => navigate('/settings/developers/new')}>
                Register a new application
              </Button>
            }
          >
            OAuth apps can sign users in and call the API on their behalf.
          </EmptyState>
        ) : (
          <ItemList aria-label="OAuth apps">
            {items.map((a) => (
              <li key={a.id} className={styles.appRowLi}>
                <Link to={`/settings/developers/${a.id}`} className={styles.appRowLink}>
                  <span className={styles.appAvatar} aria-hidden>
                    {a.name.charAt(0).toUpperCase()}
                  </span>
                  <span className={styles.appRowText}>
                    <strong>{a.name}</strong>
                    <span className={styles.scopeDesc}>
                      {a.homepage_url || a.callback_url} · Created {formatDate(a.created_at)}
                    </span>
                  </span>
                  <span className={styles.mono} style={{ color: 'var(--fg-muted)' }}>
                    {a.client_id}
                  </span>
                </Link>
              </li>
            ))}
          </ItemList>
        )
      ) : list.error ? (
        <ItemList empty="Could not load your OAuth apps." />
      ) : (
        <ListSkeleton />
      )}
    </>
  );
}

// ------------------------------------------------------------------ form

const EMPTY: AppFormValues = {
  name: '',
  homepage_url: '',
  description: '',
  callback_url: '',
  device_flow_enabled: false,
};

function fromApp(a: OAuthApp): AppFormValues {
  return {
    name: a.name,
    homepage_url: a.homepage_url,
    description: a.description ?? '',
    callback_url: a.callback_url,
    device_flow_enabled: a.device_flow_enabled,
  };
}

function AppForm({
  initial,
  submitLabel,
  onSubmit,
  onCancel,
  autoFocus,
}: {
  initial: AppFormValues;
  submitLabel: string;
  onSubmit: (v: AppFormValues) => Promise<void>;
  onCancel?: () => void;
  autoFocus?: boolean;
}) {
  const id = useId();
  const [v, setV] = useState(initial);
  const [errors, setErrors] = useState<Partial<Record<keyof AppFormValues | 'form', string>>>({});
  const [busy, setBusy] = useState(false);
  const nameRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (autoFocus) nameRef.current?.focus();
  }, [autoFocus]);
  const dirty = JSON.stringify(v) !== JSON.stringify(initial);
  const set = <K extends keyof AppFormValues>(k: K, val: AppFormValues[K]) => {
    setV((x) => ({ ...x, [k]: val }));
    setErrors((e) => ({ ...e, [k]: undefined, form: undefined }));
  };
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    const errs = validateApp(v);
    setErrors(errs);
    const first = Object.keys(errs)[0];
    if (first) {
      document.getElementById(`${id}-${first}`)?.focus();
      return;
    }
    setBusy(true);
    try {
      await onSubmit({
        ...v,
        name: v.name.trim(),
        homepage_url: v.homepage_url.trim(),
        callback_url: v.callback_url.trim(),
      });
    } catch (x) {
      const f = apiFieldErrors(x);
      const fields = f.fields as Partial<Record<keyof AppFormValues, string>>;
      const nice: typeof errors = {};
      if (fields.name) nice.name = 'Application name is invalid';
      if (fields.homepage_url) nice.homepage_url = 'Homepage URL is not a valid URL';
      if (fields.callback_url) nice.callback_url = 'Authorization callback URL is not a valid URL';
      if (!Object.keys(nice).length) nice.form = f.message;
      setErrors(nice);
    } finally {
      setBusy(false);
    }
  };
  return (
    <form onSubmit={(e) => void submit(e)} noValidate aria-label={submitLabel}>
      <FormStack>
        <Field label="Application name" htmlFor={`${id}-name`} error={errors.name} hint="Something users will recognize and trust.">
          <Input id={`${id}-name`} ref={nameRef} value={v.name} maxLength={100} invalid={!!errors.name} onChange={(e) => set('name', e.target.value)} />
        </Field>
        <Field label="Homepage URL" htmlFor={`${id}-homepage_url`} error={errors.homepage_url} hint="The full URL to your application homepage.">
          <Input
            id={`${id}-homepage_url`}
            type="url"
            value={v.homepage_url}
            invalid={!!errors.homepage_url}
            placeholder="https://example.com"
            onChange={(e) => set('homepage_url', e.target.value)}
          />
        </Field>
        <Field label="Application description" htmlFor={`${id}-description`} hint="Optional. Shown to users when they authorize the app.">
          <Textarea id={`${id}-description`} rows={3} value={v.description} maxLength={400} onChange={(e) => set('description', e.target.value)} />
        </Field>
        <Field
          label="Authorization callback URL"
          htmlFor={`${id}-callback_url`}
          error={errors.callback_url}
          hint="Users are sent back here after authorizing. Redirect URIs must match its host and be at or below its path (any port for localhost)."
        >
          <Input
            id={`${id}-callback_url`}
            type="url"
            value={v.callback_url}
            invalid={!!errors.callback_url}
            placeholder="https://example.com/auth/callback"
            onChange={(e) => set('callback_url', e.target.value)}
          />
        </Field>
        <Checkbox
          checked={v.device_flow_enabled}
          onChange={(c) => set('device_flow_enabled', c)}
          label="Enable Device Flow"
          description="Allow this OAuth app to authorize users via the device flow (CLIs, headless tools)."
        />
        {errors.form && (
          <Banner tone="danger" icon={AlertIcon}>
            {errors.form}
          </Banner>
        )}
        <ButtonRow>
          <Button type="submit" variant="primary" loading={busy} disabled={!dirty && submitLabel !== 'Register application'}>
            {submitLabel}
          </Button>
          {onCancel && <Button onClick={onCancel}>Cancel</Button>}
        </ButtonRow>
      </FormStack>
    </form>
  );
}

function NewApp() {
  return (
    <>
      <Link to="/settings/developers" className={styles.back}>
        <ArrowLeftIcon size={16} /> OAuth apps
      </Link>
      <PageHeader title="Register a new OAuth app" />
      <AppForm
        initial={EMPTY}
        submitLabel="Register application"
        autoFocus
        onCancel={() => navigate('/settings/developers')}
        onSubmit={async (v) => {
          const app = await createApp({
            ...v,
            description: v.description.trim() || undefined,
          });
          invalidate(LIST_KEY);
          if (app.client_secret) freshSecret = { appId: app.id, secret: app.client_secret };
          toast({ kind: 'success', title: `Registered ${app.name}` });
          navigate(`/settings/developers/${app.id}`);
        }}
      />
    </>
  );
}

// ------------------------------------------------------------------ detail

function AppDetail({ id }: { id: number }) {
  const res = useResource(`dev:app:${id}`, () => getApp(id));
  const [app, setApp] = useState<OAuthApp | undefined>(undefined);
  const current = app ?? res.data;
  const [secret, setSecret] = useState(() => (freshSecret?.appId === id ? freshSecret.secret : null));
  useEffect(() => {
    freshSecret = null;
  }, []);
  const [confirmRegen, setConfirmRegen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [regenBusy, setRegenBusy] = useState(false);

  if (!current) {
    return (
      <>
        <Link to="/settings/developers" className={styles.back}>
          <ArrowLeftIcon size={16} /> OAuth apps
        </Link>
        {res.error ? (
          <EmptyState title="OAuth app not found">It may have been deleted, or it belongs to someone else.</EmptyState>
        ) : (
          <FormStack>
            <Skeleton width="40%" height={24} />
            <Skeleton width="70%" />
            <Skeleton width="60%" />
          </FormStack>
        )}
      </>
    );
  }

  const regenerate = async () => {
    setRegenBusy(true);
    try {
      const a = await regenerateSecret(id);
      setApp(a);
      setSecret(a.client_secret ?? null);
      invalidate(`dev:app:${id}`);
      invalidate(LIST_KEY);
    } finally {
      setRegenBusy(false);
    }
  };

  return (
    <>
      <Link to="/settings/developers" className={styles.back}>
        <ArrowLeftIcon size={16} /> OAuth apps
      </Link>
      <PageHeader
        title={
          <span className={styles.appHeader}>
            <span className={styles.appAvatar} aria-hidden>
              {current.name.charAt(0).toUpperCase()}
            </span>
            {current.name}
          </span>
        }
        description={current.description ?? undefined}
      />
      <Section title="Credentials">
        <div className={styles.kv}>
          <span className={styles.kvLabel}>Client ID</span>
          <span className={styles.kvValue}>
            <code className={styles.code} data-testid="client-id">
              {current.client_id}
            </code>
            <CopyButton value={current.client_id} label="Copy" />
          </span>
          <span className={styles.kvLabel}>Client secret</span>
          <span className={styles.kvValue}>
            {current.client_secret_last_eight ? (
              <code className={styles.code}>*****{current.client_secret_last_eight}</code>
            ) : (
              <span className={styles.scopeDesc}>None (public client)</span>
            )}
            <Button size="sm" loading={regenBusy} onClick={() => setConfirmRegen(true)}>
              Generate a new client secret
            </Button>
          </span>
        </div>
        {secret && (
          <OneTimeSecret value={secret} label="New client secret" warning="Make sure to copy your new client secret now. You won’t be able to see it again." />
        )}
      </Section>
      <Section title="Application settings" description={`Created ${formatDate(current.created_at)} · Last updated ${formatDate(current.updated_at)}`}>
        <AppForm
          key={current.updated_at}
          initial={fromApp(current)}
          submitLabel="Update application"
          onSubmit={async (v) => {
            const a = await updateApp(id, v);
            setApp(a);
            invalidate(`dev:app:${id}`);
            invalidate(LIST_KEY);
            toast({ kind: 'success', title: 'Application updated' });
          }}
        />
      </Section>
      <Section danger title="Danger zone">
        <div className={styles.dangerRow}>
          <div>
            <strong>Delete this OAuth application</strong>
            <p className={styles.scopeDesc}>All tokens it issued are revoked and users will have to authorize a new app. This cannot be undone.</p>
          </div>
          <Button variant="danger" leadingIcon={TrashIcon} onClick={() => setConfirmDelete(true)}>
            Delete application
          </Button>
        </div>
      </Section>
      <ConfirmDialog
        open={confirmRegen}
        onClose={() => setConfirmRegen(false)}
        title="Generate a new client secret?"
        danger={false}
        confirmLabel="Generate new secret"
        onConfirm={regenerate}
      >
        <p>The current client secret stops working immediately. Update every deployment of {current.name} with the new secret.</p>
      </ConfirmDialog>
      <ConfirmDialog
        open={confirmDelete}
        onClose={() => setConfirmDelete(false)}
        title={`Delete ${current.name}`}
        confirmLabel="Delete this OAuth application"
        confirmText={current.name}
        onConfirm={async () => {
          await deleteApp(id);
          invalidate(LIST_KEY);
          invalidate(`dev:app:${id}`);
          toast({ kind: 'success', title: `Deleted ${current.name}` });
          navigate('/settings/developers');
        }}
      >
        <Banner tone="warning" icon={AlertIcon}>
          Deleting <strong>{current.name}</strong> revokes all of its access tokens and authorizations.
        </Banner>
      </ConfirmDialog>
    </>
  );
}

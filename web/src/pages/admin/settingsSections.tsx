/** Section editors of the site settings page (controlled by `SettingsPage`). */
import { useState, type ReactNode } from 'react';
import { AnnouncementBanner, MaintenanceBanner } from '../../app/SiteBanners';
import styles from '../../components/admin/admin.module.css';
import { RadioCards, Switch } from '../../components/admin/kit';
import { Button, IconButton } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { MailIcon, PencilIcon, PlusIcon, TrashIcon, XIcon } from '../../ui/icons';
import { Field, Input, Select, Textarea } from '../../ui/Input';
import { Tooltip } from '../../ui/Tooltip';
import { fromLocalInput } from '../../components/admin/format';
import { domainError, emptyOidc, oidcErrors, type Errors, type OidcForm, type SecretForm, type SettingsForm } from './settingsForm';
import s from './settings.module.css';

interface Props<K extends keyof SettingsForm> {
  value: SettingsForm[K];
  onChange: (patch: Partial<SettingsForm[K]>) => void;
  errors: Errors;
}

// ------------------------------------------------------------------ helpers

/** Chip list of email domains; Enter, comma, space or blur commits the typed text. */
function DomainChips({ value, onChange, error }: { value: string[]; onChange: (v: string[]) => void; error?: string }) {
  const [text, setText] = useState('');
  const [inputError, setInputError] = useState<string | null>(null);
  const commit = () => {
    const parts = text
      .split(/[\s,;]+/)
      .map((p) => p.trim().toLowerCase().replace(/^\*?\./, ''))
      .filter(Boolean);
    if (parts.length === 0) {
      setInputError(null);
      return;
    }
    const valid = parts.filter((p) => !domainError(p));
    const invalid = parts.filter((p) => domainError(p));
    if (valid.length) onChange([...value, ...valid.filter((v) => !value.includes(v))].filter((v, i, a) => a.indexOf(v) === i));
    setText(invalid.join(' '));
    setInputError(invalid.length ? `${invalid[0]}: ${domainError(invalid[0]!)}` : null);
  };
  const shown = inputError ?? error ?? null;
  return (
    <Field
      label="Allowed email domains"
      htmlFor="set-domains"
      error={shown}
      hint="Leave empty to allow any domain. Press Enter or comma to add; applies to self-service sign-up."
    >
      <div className={s.chips} data-invalid={shown ? '' : undefined} onClick={(e) => (e.currentTarget.querySelector('input') as HTMLInputElement | null)?.focus()}>
        {value.map((d) => (
          <span key={d} className={s.chip} data-invalid={domainError(d) ? '' : undefined}>
            {d}
            <button type="button" className={s.chipRemove} aria-label={`Remove ${d}`} onClick={() => onChange(value.filter((x) => x !== d))}>
              <XIcon size={12} />
            </button>
          </span>
        ))}
        <input
          id="set-domains"
          className={s.chipInput}
          value={text}
          placeholder={value.length ? 'Add another domain…' : 'example.com'}
          autoComplete="off"
          spellCheck={false}
          aria-invalid={!!shown || undefined}
          onChange={(e) => {
            setText(e.target.value);
            setInputError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ',' || (e.key === ' ' && text.trim())) {
              e.preventDefault();
              commit();
            } else if (e.key === 'Backspace' && !text && value.length) {
              onChange(value.slice(0, -1));
            }
          }}
          onBlur={commit}
        />
      </div>
    </Field>
  );
}

/** Write-only secret: placeholder when stored, replace or remove it. */
function SecretInput({ id, label, value, onChange, hint }: { id: string; label: string; value: SecretForm; onChange: (v: SecretForm) => void; hint?: ReactNode }) {
  if (value.stored && value.clear)
    return (
      <Field label={label} htmlFor={id} hint="The stored secret will be removed when you save.">
        <div className={s.secretRow}>
          <Input id={id} disabled value="" placeholder="Will be removed" />
          <Button size="sm" onClick={() => onChange({ ...value, clear: false })}>
            Undo
          </Button>
        </div>
      </Field>
    );
  return (
    <Field label={label} htmlFor={id} hint={value.stored ? 'A secret is stored. Type a new one to replace it.' : hint}>
      <div className={s.secretRow}>
        <Input
          id={id}
          type="password"
          autoComplete="new-password"
          value={value.value}
          placeholder={value.stored ? 'Stored — leave unchanged' : ''}
          onChange={(e) => onChange({ ...value, value: e.target.value })}
        />
        {value.stored && (
          <Button size="sm" variant="ghost" onClick={() => onChange({ ...value, value: '', clear: true })}>
            Remove
          </Button>
        )}
      </div>
    </Field>
  );
}

function Preview({ children }: { children: ReactNode }) {
  return (
    <div className={s.preview} aria-label="Banner preview">
      <div className={s.previewLabel}>Preview</div>
      {children}
    </div>
  );
}

const positiveInput = (v: string) => v.replace(/[^\d]/g, '');

// ------------------------------------------------------------------ sections

export function SignupSection({ value, onChange, errors }: Props<'signup'>) {
  return (
    <div className={s.sectionBody}>
      <RadioCards
        name="signup-policy"
        label="Sign-up policy"
        value={value.policy}
        onChange={(policy) => onChange({ policy })}
        options={[
          { value: 'open', label: 'Open', description: 'Anyone can create an account.' },
          { value: 'invite', label: 'Invitation only', description: 'Only people with a pending organization invitation.' },
          { value: 'closed', label: 'Closed', description: 'Only site administrators create accounts.' },
        ]}
      />
      <DomainChips value={value.domains} onChange={(domains) => onChange({ domains })} error={errors['signup.domains']} />
    </div>
  );
}

export function RepositoriesSection({ value, onChange, errors }: Props<'repositories'>) {
  const err = errors['repositories.max_mb'];
  return (
    <div className={s.sectionBody}>
      <div>
        <div className={styles.switchLabel} style={{ marginBottom: 6 }}>
          Default visibility of new repositories
        </div>
        <RadioCards
          name="default-visibility"
          label="Default visibility of new repositories"
          value={value.default_visibility}
          onChange={(default_visibility) => onChange({ default_visibility })}
          options={[
            { value: 'public', label: 'Public', description: 'Anyone who can reach this instance.' },
            { value: 'internal', label: 'Internal', description: 'Every signed-in user. Personal repositories fall back to private.' },
            { value: 'private', label: 'Private', description: 'Only people given access.' },
          ]}
        />
      </div>
      <Switch
        checked={value.limited}
        onChange={(limited) => onChange({ limited, max_mb: limited && !value.max_mb ? '1024' : value.max_mb })}
        label="Limit repository size"
        description="Pushes that leave a repository above the limit are rejected. Per-account quotas take precedence."
      />
      {value.limited && (
        <div className={s.narrow}>
          <Field label="Maximum repository size" htmlFor="set-max-size" error={err}>
            <Input
              id="set-max-size"
              inputMode="numeric"
              value={value.max_mb}
              trailing="MB"
              invalid={!!err}
              onChange={(e) => onChange({ max_mb: positiveInput(e.target.value) })}
            />
          </Field>
        </div>
      )}
    </div>
  );
}

export function OrganizationsSection({ value, onChange }: Props<'organizations'>) {
  return (
    <RadioCards
      name="org-creation"
      label="Who can create organizations"
      value={value.creation}
      onChange={(creation) => onChange({ creation })}
      options={[
        { value: 'all', label: 'All users', description: 'Any signed-in user can create organizations.' },
        { value: 'admins_only', label: 'Site administrators only', description: 'Users ask an administrator to create one.' },
      ]}
    />
  );
}

export function AnnouncementSection({ value, onChange, errors }: Props<'announcement'>) {
  const expiresAt = fromLocalInput(value.expires);
  const expired = !!expiresAt && Date.parse(expiresAt) <= Date.now();
  const empty = !value.message.trim() && !value.expires && !value.user_dismissible;
  return (
    <div className={s.sectionBody}>
      <Field label="Message" htmlFor="set-ann-message" hint="Shown at the top of every page for signed-in users. Line breaks are kept.">
        <Textarea id="set-ann-message" rows={3} value={value.message} placeholder="e.g. Scheduled upgrade on Saturday 10:00 UTC." onChange={(e) => onChange({ message: e.target.value })} />
      </Field>
      <div className={s.inline}>
        <Field label="Expires (optional, local time)" htmlFor="set-ann-expires" error={errors['announcement.expires'] ?? (expired ? 'This time is in the past: the banner will not be shown.' : null)}>
          <Input id="set-ann-expires" type="datetime-local" value={value.expires} invalid={!!errors['announcement.expires']} onChange={(e) => onChange({ expires: e.target.value })} />
        </Field>
        {value.expires && (
          <Button size="sm" variant="ghost" onClick={() => onChange({ expires: '' })}>
            Never expire
          </Button>
        )}
      </div>
      <Switch checked={value.user_dismissible} onChange={(user_dismissible) => onChange({ user_dismissible })} label="Users can dismiss it" description="Dismissal is remembered per browser until the message changes." />
      <Preview>
        {value.message.trim() && !expired ? (
          <AnnouncementBanner message={value.message.trim()} dismissible={value.user_dismissible} onDismiss={() => undefined} />
        ) : (
          <div className={s.previewEmpty}>{expired ? 'Expired — no banner is shown.' : 'No announcement is shown.'}</div>
        )}
      </Preview>
      <div>
        <Button size="sm" leadingIcon={TrashIcon} disabled={empty} onClick={() => onChange({ message: '', expires: '', user_dismissible: false })}>
          Clear announcement
        </Button>
      </div>
    </div>
  );
}

export function RateLimitsSection({ value, onChange, errors }: Props<'rate_limits'>) {
  const a = errors['rate_limits.authenticated'];
  const u = errors['rate_limits.unauthenticated'];
  return (
    <div className={s.sectionBody}>
      <Switch
        checked={value.enabled}
        onChange={(enabled) => onChange({ enabled })}
        label="Enable API rate limiting"
        description="Requests over the limit get a 403 with X-RateLimit-* headers, like GitHub."
      />
      <div className={styles.formRow}>
        <Field label="Authenticated requests" htmlFor="set-rl-auth" error={a} hint="Per user, per hour.">
          <Input id="set-rl-auth" inputMode="numeric" trailing="/ hour" value={value.authenticated} invalid={!!a} disabled={!value.enabled} onChange={(e) => onChange({ authenticated: positiveInput(e.target.value) })} />
        </Field>
        <Field label="Unauthenticated requests" htmlFor="set-rl-anon" error={u} hint="Per client IP, per hour (also the anonymous GraphQL budget).">
          <Input id="set-rl-anon" inputMode="numeric" trailing="/ hour" value={value.unauthenticated} invalid={!!u} disabled={!value.enabled} onChange={(e) => onChange({ unauthenticated: positiveInput(e.target.value) })} />
        </Field>
      </div>
      <div className={styles.formRow}>
        <Field label="Search, authenticated" htmlFor="set-rl-search" error={errors['rate_limits.search_authenticated']} hint="Per user, per minute.">
          <Input
            id="set-rl-search"
            inputMode="numeric"
            trailing="/ min"
            value={value.search_authenticated}
            invalid={!!errors['rate_limits.search_authenticated']}
            disabled={!value.enabled}
            onChange={(e) => onChange({ search_authenticated: positiveInput(e.target.value) })}
          />
        </Field>
        <Field label="Search, unauthenticated" htmlFor="set-rl-search-anon" error={errors['rate_limits.search_unauthenticated']} hint="Per client IP, per minute.">
          <Input
            id="set-rl-search-anon"
            inputMode="numeric"
            trailing="/ min"
            value={value.search_unauthenticated}
            invalid={!!errors['rate_limits.search_unauthenticated']}
            disabled={!value.enabled}
            onChange={(e) => onChange({ search_unauthenticated: positiveInput(e.target.value) })}
          />
        </Field>
        <Field label="GraphQL" htmlFor="set-rl-graphql" error={errors['rate_limits.graphql']} hint="Per user, per hour.">
          <Input
            id="set-rl-graphql"
            inputMode="numeric"
            trailing="/ hour"
            value={value.graphql}
            invalid={!!errors['rate_limits.graphql']}
            disabled={!value.enabled}
            onChange={(e) => onChange({ graphql: positiveInput(e.target.value) })}
          />
        </Field>
      </div>
    </div>
  );
}

export function AuthSection({ value, onChange, errors }: Props<'auth_providers'>) {
  const [editing, setEditing] = useState<OidcForm | null>(null);
  const methodsError = errors['auth_providers.methods'];
  const save = (p: OidcForm) => {
    const exists = value.oidc.some((o) => o.key === p.key);
    onChange({ oidc: exists ? value.oidc.map((o) => (o.key === p.key ? p : o)) : [...value.oidc, p] });
    setEditing(null);
  };
  return (
    <div className={s.sectionBody}>
      <Switch
        checked={value.password_login}
        onChange={(password_login) => onChange({ password_login })}
        label="Password sign-in"
        description="Built-in username and password login. Turn off to require single sign-on."
      />
      <div>
        <div className={styles.switchLabel} style={{ marginBottom: 6 }}>
          OpenID Connect providers
        </div>
        {value.oidc.length > 0 ? (
          <div className={s.providers}>
            {value.oidc.map((p) => {
              const err = errors[`auth_providers.oidc.${p.key}`];
              return (
                <div key={p.key} className={s.provider}>
                  <div className={s.providerMain}>
                    <span>
                      <strong>{p.display_name || p.name || 'Unnamed provider'}</strong> <span className={styles.mono}>{p.name}</span>
                    </span>
                    <span className={styles.subtle}>
                      {p.issuer} · {p.auto_create_users ? 'creates accounts on first sign-in' : 'existing accounts only'}
                    </span>
                    {err && <span className={s.providerError}>{err}</span>}
                  </div>
                  <IconButton icon={PencilIcon} size="sm" label={`Edit ${p.name || 'provider'}`} onClick={() => setEditing(p)} />
                  <IconButton icon={TrashIcon} size="sm" label={`Remove ${p.name || 'provider'}`} onClick={() => onChange({ oidc: value.oidc.filter((o) => o.key !== p.key) })} />
                </div>
              );
            })}
          </div>
        ) : (
          <p className={styles.subtle} style={{ margin: 0 }}>
            No providers configured.
          </p>
        )}
        <div style={{ marginTop: 8 }}>
          <Button size="sm" leadingIcon={PlusIcon} onClick={() => setEditing(emptyOidc())}>
            Add provider
          </Button>
        </div>
      </div>
      {methodsError && (
        <div className={styles.formError} role="alert">
          {methodsError}
        </div>
      )}
      <OidcDialog provider={editing} all={value.oidc} onClose={() => setEditing(null)} onSave={save} />
    </div>
  );
}

function OidcDialog({ provider, all, onClose, onSave }: { provider: OidcForm | null; all: OidcForm[]; onClose: () => void; onSave: (p: OidcForm) => void }) {
  const [form, setForm] = useState<OidcForm | null>(provider);
  const [shownFor, setShownFor] = useState(provider);
  const [submitted, setSubmitted] = useState(false);
  if (provider !== shownFor) {
    setShownFor(provider);
    setForm(provider);
    setSubmitted(false);
  }
  const isNew = !!provider && !all.some((o) => o.key === provider.key);
  const errors = form ? oidcErrors(form, all) : {};
  const show = (k: 'name' | 'issuer' | 'client_id' | 'allowed_domains') => {
    const v = form?.[k] ?? '';
    return submitted || (v && v !== 'https://') ? errors[k] : undefined;
  };
  const set = (patch: Partial<OidcForm>) => setForm((f) => (f ? { ...f, ...patch } : f));
  const submit = () => {
    setSubmitted(true);
    if (form && Object.keys(errors).length === 0) onSave({ ...form, name: form.name.trim(), issuer: form.issuer.trim(), client_id: form.client_id.trim() });
  };
  return (
    <Dialog
      open={!!provider}
      onClose={onClose}
      title={isNew ? 'Add OIDC provider' : `Edit ${provider?.name ?? 'provider'}`}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" onClick={submit}>
            {isNew ? 'Add provider' : 'Apply'}
          </Button>
        </>
      }
    >
      {form && (
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            submit();
          }}
        >
          <div className={styles.formRow}>
            <Field
              label="Name"
              htmlFor="oidc-name"
              error={show('name')}
              hint={form.saved ? 'Part of the sign-in URL; remove and re-add the provider to rename it.' : 'Used in URLs: lower-case letters, digits, hyphens.'}
            >
              <Input
                id="oidc-name"
                value={form.name}
                disabled={form.saved}
                invalid={!!show('name')}
                autoFocus={!form.saved}
                autoComplete="off"
                spellCheck={false}
                onChange={(e) => set({ name: e.target.value.toLowerCase().replace(/\s+/g, '-') })}
              />
            </Field>
            <Field label="Display name" htmlFor="oidc-display" hint="Shown on the sign-in button.">
              <Input id="oidc-display" value={form.display_name} placeholder={form.name || 'e.g. Okta'} onChange={(e) => set({ display_name: e.target.value })} />
            </Field>
          </div>
          <Field label="Issuer URL" htmlFor="oidc-issuer" error={show('issuer')} hint="Discovery is read from /.well-known/openid-configuration.">
            <Input id="oidc-issuer" type="url" value={form.issuer} invalid={!!show('issuer')} spellCheck={false} onChange={(e) => set({ issuer: e.target.value })} />
          </Field>
          <Field label="Client ID" htmlFor="oidc-client" error={show('client_id')}>
            <Input id="oidc-client" value={form.client_id} invalid={!!show('client_id')} autoComplete="off" spellCheck={false} onChange={(e) => set({ client_id: e.target.value })} />
          </Field>
          <SecretInput id="oidc-secret" label="Client secret" value={form.secret} onChange={(secret) => set({ secret })} hint="Write-only: it can't be read back after saving." />
          <Field label="Scopes" htmlFor="oidc-scopes" hint="Space separated.">
            <Input id="oidc-scopes" value={form.scopes} spellCheck={false} onChange={(e) => set({ scopes: e.target.value })} />
          </Field>
          <div className={styles.formRow}>
            <Field label="Login claim (optional)" htmlFor="oidc-login-claim" hint="Claim proposing the username of new accounts; default preferred_username.">
              <Input id="oidc-login-claim" value={form.login_claim} placeholder="preferred_username" spellCheck={false} onChange={(e) => set({ login_claim: e.target.value })} />
            </Field>
            <Field label="Allowed email domains (optional)" htmlFor="oidc-domains" error={show('allowed_domains')} hint="Space separated; empty allows any domain.">
              <Input
                id="oidc-domains"
                value={form.allowed_domains}
                invalid={!!show('allowed_domains')}
                placeholder="example.com"
                spellCheck={false}
                onChange={(e) => set({ allowed_domains: e.target.value })}
              />
            </Field>
          </div>
          <Switch
            checked={form.auto_create_users}
            onChange={(auto_create_users) => set({ auto_create_users })}
            label="Create accounts on first sign-in"
            description="Otherwise only people with an existing account can sign in with this provider."
          />
          <p className={styles.subtle} style={{ margin: 0 }}>
            Changes apply when you save the settings page.
          </p>
          <button type="submit" hidden />
        </form>
      )}
    </Dialog>
  );
}

export function SmtpSection({ value, onChange, errors }: Props<'smtp'>) {
  const e = (k: string) => errors[`smtp.${k}`];
  return (
    <div className={s.sectionBody}>
      <Switch checked={value.enabled} onChange={(enabled) => onChange({ enabled })} label="Send email" description="Notifications, invitations and password resets." />
      <div className={styles.formRow}>
        <Field label="SMTP host" htmlFor="smtp-host" error={e('host')}>
          <Input id="smtp-host" value={value.host} placeholder="smtp.example.com" invalid={!!e('host')} spellCheck={false} onChange={(ev) => onChange({ host: ev.target.value })} />
        </Field>
        <Field label="Port" htmlFor="smtp-port" error={e('port')}>
          <Input id="smtp-port" inputMode="numeric" value={value.port} invalid={!!e('port')} onChange={(ev) => onChange({ port: positiveInput(ev.target.value) })} />
        </Field>
        <Field label="Encryption" htmlFor="smtp-tls">
          <Select id="smtp-tls" value={value.tls} onChange={(ev) => onChange({ tls: ev.target.value as SettingsForm['smtp']['tls'] })}>
            <option value="starttls">STARTTLS</option>
            <option value="tls">TLS (implicit)</option>
            <option value="none">None</option>
          </Select>
        </Field>
      </div>
      <div className={styles.formRow}>
        <Field label="Username (optional)" htmlFor="smtp-user">
          <Input id="smtp-user" value={value.username} autoComplete="off" spellCheck={false} onChange={(ev) => onChange({ username: ev.target.value })} />
        </Field>
        <SecretInput id="smtp-password" label="Password" value={value.password} onChange={(password) => onChange({ password })} hint="Write-only." />
      </div>
      <Field label="From address" htmlFor="smtp-from" error={e('from')} hint="e.g. Better GitHub <noreply@example.com>">
        <Input id="smtp-from" value={value.from} invalid={!!e('from')} spellCheck={false} onChange={(ev) => onChange({ from: ev.target.value })} />
      </Field>
      <div>
        <Tooltip label="Not supported by this server yet">
          <span className={s.disabledWrap} tabIndex={0} aria-label="Send test email (not supported by this server yet)">
            <Button size="sm" leadingIcon={MailIcon} disabled tabIndex={-1}>
              Send test email
            </Button>
          </span>
        </Tooltip>
      </div>
    </div>
  );
}

export function MaintenanceSection({ value, onChange, errors, onEnable }: Props<'maintenance'> & { onEnable: () => void }) {
  const scheduledAt = fromLocalInput(value.scheduled);
  const upcoming = !value.enabled && !!scheduledAt && Date.parse(scheduledAt) > Date.now();
  return (
    <div className={s.sectionBody}>
      <Switch
        checked={value.enabled}
        onChange={(enabled) => (enabled ? onEnable() : onChange({ enabled }))}
        label="Maintenance mode"
        description="Everyone but site administrators gets “503 Service Unavailable” from the API and git."
      />
      <Field label="Message (optional)" htmlFor="set-maint-message" hint="Shown in the banner and returned in API error responses.">
        <Textarea id="set-maint-message" rows={2} value={value.message} placeholder="This instance is undergoing maintenance." onChange={(e) => onChange({ message: e.target.value })} />
      </Field>
      <div className={s.inline}>
        <Field label="Scheduled start (optional, local time)" htmlFor="set-maint-at" error={errors['maintenance.scheduled']} hint="Announces upcoming maintenance in the banner; doesn't turn it on automatically.">
          <Input id="set-maint-at" type="datetime-local" value={value.scheduled} invalid={!!errors['maintenance.scheduled']} onChange={(e) => onChange({ scheduled: e.target.value })} />
        </Field>
        {value.scheduled && (
          <Button size="sm" variant="ghost" onClick={() => onChange({ scheduled: '' })}>
            Clear
          </Button>
        )}
      </div>
      <Preview>
        {value.enabled || upcoming ? (
          <MaintenanceBanner enabled={value.enabled} message={value.message.trim() || null} scheduledAt={scheduledAt} />
        ) : (
          <div className={s.previewEmpty}>No maintenance banner is shown.</div>
        )}
      </Preview>
    </div>
  );
}

export function ActionsSection({ value, onChange }: Props<'actions'>) {
  return (
    <div className={s.sectionBody}>
      <RadioCards
        name="actions-default-permissions"
        label="Default GITHUB_TOKEN permissions"
        value={value.default_workflow_permissions}
        onChange={(default_workflow_permissions) => onChange({ default_workflow_permissions })}
        options={[
          {
            value: 'read',
            label: 'Read repository contents and packages',
            description: 'Workflows without a permissions: key can only read. Recommended.',
          },
          {
            value: 'write',
            label: 'Read and write',
            description: 'Workflows without a permissions: key get write access to every category.',
          },
        ]}
      />
      <p className={styles.subtle} style={{ margin: 0 }}>
        Workflows can always narrow or widen this with <code>permissions:</code>. Tokens of pull requests from forks are always read-only, and no token can change
        workflow files.
      </p>
    </div>
  );
}

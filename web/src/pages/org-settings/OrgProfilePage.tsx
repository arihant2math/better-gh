import { useEffect, useRef, useState } from 'react';
import { mutate, refresh, useResource } from '../../api/cache';
import { fieldErrors, type FieldErrors } from '../../api/errors';
import styles from '../../components/admin/admin.module.css';
import { formatDateTime } from '../../components/admin/format';
import { ErrorState, PageHeader, Panel, RadioCards, Switch, errorMessage, useConfirm } from '../../components/admin/kit';
import { navigate, useParams } from '../../router';
import { canonicalAccountUrl } from '../profile/canonical';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Avatar, Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import { AlertIcon, LockIcon, TrashIcon } from '../../ui/icons';
import { Field, Input, Textarea } from '../../ui/Input';
import { toast } from '../../ui/Toast';
import {
  AVATAR_TYPES,
  MAX_AVATAR_BYTES,
  deleteOrgAvatar,
  getOrg,
  orgKey,
  updateOrg,
  uploadOrgAvatar,
  type DefaultRepoPermission,
  type OrgFull,
  type OrgPatch,
} from '../../api/orgSettings';
import { useOrgAccess } from './common';
import { OrgDangerZone } from './OrgDangerZone';
import local from './OrgSettings.module.css';

interface Form {
  name: string;
  description: string;
  email: string;
  billing_email: string;
  blog: string;
  location: string;
  twitter_username: string;
  company: string;
  default_repository_permission: DefaultRepoPermission;
  members_can_create_repositories: boolean;
  members_can_create_public_repositories: boolean;
  members_can_create_private_repositories: boolean;
  members_can_fork_private_repositories: boolean;
  members_can_create_teams: boolean;
  web_commit_signoff_required: boolean;
}

const TEXT_FIELDS = ['name', 'description', 'email', 'billing_email', 'blog', 'location', 'twitter_username', 'company'] as const;
const FLAG_FIELDS = [
  'members_can_create_repositories',
  'members_can_create_public_repositories',
  'members_can_create_private_repositories',
  'members_can_fork_private_repositories',
  'members_can_create_teams',
  'web_commit_signoff_required',
] as const;

function toForm(o: OrgFull): Form {
  return {
    name: o.name ?? '',
    description: o.description ?? '',
    email: o.email ?? '',
    billing_email: o.billing_email ?? '',
    blog: o.blog ?? '',
    location: o.location ?? '',
    twitter_username: o.twitter_username ?? '',
    company: o.company ?? '',
    default_repository_permission: o.default_repository_permission ?? 'read',
    members_can_create_repositories: o.members_can_create_repositories ?? true,
    members_can_create_public_repositories: o.members_can_create_public_repositories ?? o.members_can_create_repositories ?? true,
    members_can_create_private_repositories: o.members_can_create_private_repositories ?? o.members_can_create_repositories ?? true,
    members_can_fork_private_repositories: o.members_can_fork_private_repositories ?? false,
    // Not part of organization-full on every server: GitHub defaults.
    members_can_create_teams: o.members_can_create_teams ?? true,
    web_commit_signoff_required: o.web_commit_signoff_required ?? false,
  };
}

function diff(base: Form, form: Form): OrgPatch {
  const patch: Record<string, unknown> = {};
  for (const k of TEXT_FIELDS) if (form[k].trim() !== base[k].trim()) patch[k] = form[k].trim();
  if (form.default_repository_permission !== base.default_repository_permission) patch.default_repository_permission = form.default_repository_permission;
  for (const k of FLAG_FIELDS) if (form[k] !== base[k]) patch[k] = form[k];
  return patch;
}

const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

function validate(f: Form): Record<string, string> {
  const e: Record<string, string> = {};
  if (f.email.trim() && !EMAIL_RE.test(f.email.trim())) e.email = 'Enter a valid email address.';
  if (f.billing_email.trim() && !EMAIL_RE.test(f.billing_email.trim())) e.billing_email = 'Enter a valid email address.';
  if (f.blog.trim() && /\s/.test(f.blog.trim())) e.blog = 'URLs can’t contain spaces.';
  if (f.twitter_username.trim() && !/^@?[A-Za-z0-9_]{1,15}$/.test(f.twitter_username.trim())) e.twitter_username = 'Up to 15 letters, digits or underscores.';
  if (f.name.length > 255) e.name = 'Too long (255 characters max).';
  if (f.description.length > 160) e.description = 'Too long (160 characters max).';
  return e;
}

/** The server may accept a setting without reporting it in organization-full. */
const withUnreported = (text: string, value: boolean | undefined) => (value === undefined ? `${text} The current value isn’t reported by the server; saving sets it.` : text);

const PERMISSIONS: { value: DefaultRepoPermission; label: string; description: string }[] = [
  { value: 'none', label: 'No permission', description: 'Members can only clone and pull public repositories.' },
  { value: 'read', label: 'Read', description: 'Members can clone and pull all repositories.' },
  { value: 'write', label: 'Write', description: 'Members can clone, pull and push all repositories.' },
  { value: 'admin', label: 'Admin', description: 'Members can clone, pull, push and administer all repositories.' },
];

export default function OrgProfilePage() {
  const { org = '' } = useParams<{ org: string }>();
  const res = useResource(orgKey(org), () => getOrg(org));
  const access = useOrgAccess(org);
  const [base, setBase] = useState<OrgFull | null>(null);
  const [baseForm, setBaseForm] = useState<Form | null>(null);
  const [form, setForm] = useState<Form | null>(null);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [serverError, setServerError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const patch = baseForm && form ? diff(baseForm, form) : {};
  const dirty = Object.keys(patch).length > 0;
  // Adopt fresh server data unless the owner has unsaved edits.
  if (res.data && res.data !== base && !dirty) {
    setBase(res.data);
    setBaseForm(toForm(res.data));
    setForm(toForm(res.data));
  }
  const readOnly = !access.isOwner;
  const memberView = res.data?.default_repository_permission !== undefined;

  const set = (p: Partial<Form>) => {
    setForm((f) => (f ? { ...f, ...p } : f));
    setErrors((e) => {
      const next = { ...e };
      for (const k of Object.keys(p)) delete next[k];
      return next;
    });
  };

  const save = async () => {
    if (!form || !dirty || saving || readOnly) return;
    const v = validate(form);
    setErrors(v);
    setServerError(null);
    if (Object.keys(v).length) return;
    setSaving(true);
    try {
      const updated = await updateOrg(org, patch);
      // Fields the response may omit keep the value we just saved.
      const merged: OrgFull = { ...updated };
      for (const k of FLAG_FIELDS) if (merged[k] === undefined) merged[k] = form[k];
      mutate<OrgFull>(orgKey(org), () => merged);
      setBase(merged);
      setBaseForm(toForm(merged));
      setForm(toForm(merged));
      toast({ kind: 'success', title: 'Organization settings saved' });
    } catch (err) {
      const fe = fieldErrors(err);
      if (Object.keys(fe).length) setErrors(fe);
      setServerError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  };

  const discard = () => {
    if (baseForm) setForm(baseForm);
    setErrors({});
    setServerError(null);
  };

  useShortcuts('Organization profile', {
    'mod+s': { handler: () => void save(), description: 'Save changes', group: 'Organization' },
  });

  // `GET /orgs/{old}` resolves a renamed organization: move to its current name.
  const currentLogin = res.data?.login;
  useEffect(() => {
    if (!currentLogin) return;
    const to = canonicalAccountUrl(window.location, org, currentLogin, 2);
    if (to) navigate(to, { replace: true });
  }, [currentLogin, org]);

  if (res.error && !res.data) {
    return (
      <div className={styles.page}>
        <PageHeader title="General" />
        <ErrorState error={res.error} onRetry={() => void refresh(orgKey(org), () => getOrg(org))} />
      </div>
    );
  }

  const text = (key: (typeof TEXT_FIELDS)[number], label: string, opts: { type?: string; hint?: string; placeholder?: string; multiline?: boolean } = {}) => (
    <Field label={label} htmlFor={`org-${key}`} error={errors[key] ?? null} hint={opts.hint}>
      {opts.multiline ? (
        <Textarea id={`org-${key}`} rows={2} value={form?.[key] ?? ''} onChange={(e) => set({ [key]: e.target.value })} disabled={readOnly} placeholder={opts.placeholder} />
      ) : (
        <Input
          id={`org-${key}`}
          type={opts.type}
          value={form?.[key] ?? ''}
          onChange={(e) => set({ [key]: e.target.value })}
          disabled={readOnly}
          invalid={!!errors[key]}
          placeholder={opts.placeholder}
        />
      )}
    </Field>
  );

  return (
    <div className={styles.page}>
      <PageHeader title="General" description={`Profile and member privileges of ${org}.`} />
      {!access.loading && readOnly && res.data && (
        <div className={local.notice} role="status">
          <LockIcon size={16} /> You must be an owner of {org} to change these settings. They are shown read-only.
        </div>
      )}
      {!form ? (
        <Panel title="Profile">
          <div className={styles.stack}>
            {Array.from({ length: 6 }, (_, i) => (
              <Skeleton key={i} height={28} />
            ))}
          </div>
        </Panel>
      ) : (
        <form
          className={styles.stack}
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <Panel title="Profile">
            <div className={local.profileGrid}>
              <div className={styles.form}>
                {text('name', 'Organization display name')}
                {text('description', 'Description', { multiline: true, hint: 'Shown on the organization profile (160 characters).' })}
                <div className={styles.formRow}>
                  {text('email', 'Email (will be public)', { type: 'email' })}
                  {memberView && text('billing_email', 'Billing email (private)', { type: 'email' })}
                </div>
                <div className={styles.formRow}>
                  {text('blog', 'URL', { type: 'url', placeholder: 'https://example.com' })}
                  {text('location', 'Location')}
                </div>
                <div className={styles.formRow}>
                  {text('twitter_username', 'Social account (X)', { placeholder: 'username' })}
                  {text('company', 'Company')}
                </div>
              </div>
              {res.data && <AvatarEditor org={org} data={res.data} readOnly={readOnly} />}
            </div>
          </Panel>

          {memberView && (
            <Panel title="Member privileges">
              <div className={styles.form}>
                <div>
                  <div className={local.sectionLabel}>Base permissions</div>
                  <p className={local.sectionHint}>Permission every member has on every repository of the organization.</p>
                  <fieldset className={local.fieldset} disabled={readOnly}>
                    <RadioCards
                      name="org-default-perm"
                      label="Base permissions"
                      value={form.default_repository_permission}
                      onChange={(v) => set({ default_repository_permission: v })}
                      options={PERMISSIONS}
                    />
                  </fieldset>
                </div>
                <Switch
                  label="Repository creation"
                  description="Members can create repositories. Owners always can."
                  checked={form.members_can_create_repositories}
                  disabled={readOnly}
                  onChange={(v) =>
                    set({
                      members_can_create_repositories: v,
                      members_can_create_public_repositories: v ? form.members_can_create_public_repositories || !form.members_can_create_private_repositories : false,
                      members_can_create_private_repositories: v ? form.members_can_create_private_repositories || !form.members_can_create_public_repositories : false,
                    })
                  }
                />
                {form.members_can_create_repositories && (
                  <div className={local.nested}>
                    <Switch
                      label="Public"
                      description="Members can create public repositories."
                      checked={form.members_can_create_public_repositories}
                      disabled={readOnly}
                      onChange={(v) => set({ members_can_create_public_repositories: v, members_can_create_repositories: v || form.members_can_create_private_repositories })}
                    />
                    <Switch
                      label="Private"
                      description="Members can create private repositories."
                      checked={form.members_can_create_private_repositories}
                      disabled={readOnly}
                      onChange={(v) => set({ members_can_create_private_repositories: v, members_can_create_repositories: v || form.members_can_create_public_repositories })}
                    />
                  </div>
                )}
                <Switch
                  label="Repository forking"
                  description="Allow forking of private repositories."
                  checked={form.members_can_fork_private_repositories}
                  disabled={readOnly}
                  onChange={(v) => set({ members_can_fork_private_repositories: v })}
                />
                <Switch
                  label="Team creation"
                  description={withUnreported('Members can create teams. Owners always can.', res.data?.members_can_create_teams)}
                  checked={form.members_can_create_teams}
                  disabled={readOnly}
                  onChange={(v) => set({ members_can_create_teams: v })}
                />
                <Switch
                  label="Require contributors to sign off on web-based commits"
                  description={withUnreported('Commits made in the web interface must include a Signed-off-by trailer.', res.data?.web_commit_signoff_required)}
                  checked={form.web_commit_signoff_required}
                  disabled={readOnly}
                  onChange={(v) => set({ web_commit_signoff_required: v })}
                />
              </div>
            </Panel>
          )}

          {res.data && (
            <Panel title="About">
              <dl className={styles.kv}>
                <div className={styles.kvRow}>
                  <dt>Created</dt>
                  <dd>{formatDateTime(res.data.created_at)}</dd>
                </div>
                <div className={styles.kvRow}>
                  <dt>Public repositories</dt>
                  <dd>{res.data.public_repos}</dd>
                </div>
                {res.data.total_private_repos !== undefined && (
                  <div className={styles.kvRow}>
                    <dt>Private repositories</dt>
                    <dd>{res.data.total_private_repos}</dd>
                  </div>
                )}
                <div className={styles.kvRow}>
                  <dt>Status</dt>
                  <dd>
                    {res.data.is_verified && <Tag>Verified</Tag>} {res.data.archived_at ? <Tag>Archived {formatDateTime(res.data.archived_at)}</Tag> : 'Active'}
                  </dd>
                </div>
              </dl>
            </Panel>
          )}

          {(dirty || serverError) && !readOnly && (
            <div className={styles.saveBar} role="region" aria-label="Unsaved changes">
              <span>
                {serverError ? (
                  <span className={local.errorText} role="alert">
                    <AlertIcon size={14} /> {serverError}
                  </span>
                ) : (
                  `${Object.keys(patch).length} unsaved change${Object.keys(patch).length === 1 ? '' : 's'}`
                )}
              </span>
              <Button onClick={discard} disabled={saving || !dirty}>
                Discard
              </Button>
              <Button type="submit" variant="primary" loading={saving} disabled={!dirty} kbd="⌘S">
                Save changes
              </Button>
            </div>
          )}
        </form>
      )}
      {access.isOwner && res.data && <OrgDangerZone org={res.data.login ?? org} />}
    </div>
  );
}

function AvatarEditor({ org, data, readOnly }: { org: string; data: OrgFull; readOnly: boolean }) {
  const input = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState(false);
  const confirm = useConfirm();
  const apply = (avatar_url: string) => mutate<OrgFull>(orgKey(org), (prev) => ({ ...(prev ?? data), avatar_url }));
  const upload = async (file: File) => {
    if (!AVATAR_TYPES.includes(file.type)) {
      toast({ kind: 'error', title: 'Unsupported image', description: 'Use a PNG, JPEG, GIF or WebP image.' });
      return;
    }
    if (file.size > MAX_AVATAR_BYTES) {
      toast({ kind: 'error', title: 'Image too large', description: 'The picture must be 1 MB or smaller.' });
      return;
    }
    setBusy(true);
    try {
      const res = await uploadOrgAvatar(org, file);
      apply(res.avatar_url);
      toast({ kind: 'success', title: 'Profile picture updated' });
    } catch (err) {
      toast({ kind: 'error', title: 'Could not upload the picture', description: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };
  // Uploaded pictures are versioned by content hash (`?v=<sha>`).
  const custom = /[?&]v=[0-9a-f]{8,}/i.test(data.avatar_url);
  return (
    <div className={local.avatarCol}>
      <span className={local.sectionLabel}>Profile picture</span>
      <Avatar user={{ login: data.login, avatarUrl: data.avatar_url, name: data.name }} size={120} square />
      <input
        ref={input}
        type="file"
        accept={AVATAR_TYPES.join(',')}
        hidden
        aria-label="Profile picture file"
        onChange={(e) => {
          const f = e.target.files?.[0];
          e.target.value = '';
          if (f) void upload(f);
        }}
      />
      <div className={local.avatarActions}>
        <Button size="sm" loading={busy} disabled={readOnly} onClick={() => input.current?.click()}>
          Upload new picture
        </Button>
        {custom && (
          <Button
            size="sm"
            variant="ghost"
            leadingIcon={TrashIcon}
            disabled={readOnly || busy}
            onClick={() =>
              confirm({
                title: 'Remove profile picture?',
                body: `${org} will show a generated identicon instead.`,
                confirmLabel: 'Remove picture',
                danger: true,
                onConfirm: async () => {
                  const res = await deleteOrgAvatar(org);
                  if (res?.avatar_url) apply(res.avatar_url);
                  else void refresh(orgKey(org), () => getOrg(org));
                },
              })
            }
          >
            Remove
          </Button>
        )}
      </div>
      <span className={styles.subtle}>PNG, JPEG, GIF or WebP, up to 1 MB.</span>
      {confirm.dialog}
    </div>
  );
}

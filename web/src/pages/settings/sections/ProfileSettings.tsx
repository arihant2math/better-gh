import { observer } from 'mobx-react-lite';
import { useEffect, useId, useRef, useState, type FormEvent } from 'react';
import {
  KEYS,
  MAX_AVATAR_BYTES,
  applyViewerPatch,
  bustAvatar,
  deleteAvatar,
  getMe,
  listEmails,
  updateMe,
  uploadAvatar,
  useEditableResource,
  type PrivateUser,
  type ProfilePatch,
} from '@/api/userSettings';
import { session } from '@/app/session';
import { Banner, ButtonRow, Checkbox, ConfirmDialog, FormStack, PageHeader, apiFieldErrors, type FieldErrors } from '@/components/settings/kit';
import { Link } from '@/router';
import { Avatar } from '@/ui/Badge';
import { Button } from '@/ui/Button';
import { Skeleton } from '@/ui/EmptyState';
import { MentionIcon, PencilIcon, TrashIcon, UploadIcon } from '@/ui/icons';
import { Field, Input, Select, Textarea } from '@/ui/Input';
import { Spinner } from '@/ui/Spinner';
import { toast } from '@/ui/Toast';
import { AvatarCropDialog } from '../avatarCrop';
import styles from './userSettings.module.css';

const BIO_MAX = 160;
const IMAGE_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
/** Source images are re-encoded at 460×460, so only guard against absurd files. */
const MAX_SOURCE_BYTES = 20 * 1024 * 1024;

interface Form {
  name: string;
  email: string;
  bio: string;
  blog: string;
  twitter_username: string;
  company: string;
  location: string;
  hireable: boolean;
}

function toForm(u: PrivateUser): Form {
  return {
    name: u.name ?? '',
    email: u.email ?? '',
    bio: u.bio ?? '',
    blog: u.blog ?? '',
    twitter_username: u.twitter_username ?? '',
    company: u.company ?? '',
    location: u.location ?? '',
    hireable: !!u.hireable,
  };
}

/** Client-side mirror of the backend's PATCH /user validation. */
export function validateProfile(f: Form): FieldErrors {
  const e: FieldErrors = {};
  const len = (s: string) => [...s.trim()].length;
  if (len(f.name) > 255) e.name = 'Name is too long (maximum is 255 characters)';
  if (len(f.bio) > BIO_MAX) e.bio = `Bio is too long (maximum is ${BIO_MAX} characters)`;
  if (len(f.blog) > 255) e.blog = 'URL is too long (maximum is 255 characters)';
  const tw = f.twitter_username.trim().replace(/^@/, '');
  if (tw && !/^[A-Za-z0-9_]{1,15}$/.test(tw)) e.twitter_username = 'Use up to 15 letters, numbers or underscores';
  if (len(f.company) > 255) e.company = 'Company is too long (maximum is 255 characters)';
  if (len(f.location) > 255) e.location = 'Location is too long (maximum is 255 characters)';
  return e;
}

/** Only the fields that changed, in the PATCH /user shape. */
export function profileDiff(before: Form, after: Form): ProfilePatch {
  const patch: ProfilePatch = {};
  const text = ['name', 'email', 'bio', 'blog', 'company', 'location'] as const;
  for (const k of text) if (after[k].trim() !== before[k].trim()) patch[k] = after[k].trim() || null;
  const tw = (s: string) => s.trim().replace(/^@/, '');
  if (tw(after.twitter_username) !== tw(before.twitter_username)) patch.twitter_username = tw(after.twitter_username) || null;
  if (after.hireable !== before.hireable) patch.hireable = after.hireable;
  return patch;
}

export default observer(function ProfileSettings() {
  const me = useEditableResource(KEYS.me, getMe);
  const emails = useEditableResource(KEYS.emails, listEmails);
  const [form, setForm] = useState<Form | null>(null);
  const [base, setBase] = useState<Form | null>(null);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [general, setGeneral] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const ids = { name: useId(), email: useId(), bio: useId(), blog: useId(), tw: useId(), company: useId(), location: useId() };

  // Adopt server data until the user starts editing.
  useEffect(() => {
    if (!me.data) return;
    const next = toForm(me.data);
    setBase(next);
    setForm((f) => (f && base && JSON.stringify(f) !== JSON.stringify(base) ? f : next));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- only when server data changes
  }, [me.data]);

  const user = session.user;
  const dirty = !!form && !!base && Object.keys(profileDiff(base, form)).length > 0;
  const set = <K extends keyof Form>(k: K, v: Form[K]) => {
    setForm((f) => (f ? { ...f, [k]: v } : f));
    if (errors[k]) setErrors((e) => ({ ...e, [k]: undefined }));
  };

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (!form || !base || saving) return;
    const errs = validateProfile(form);
    setErrors(errs);
    setGeneral(null);
    if (Object.values(errs).some(Boolean)) return;
    const patch = profileDiff(base, form);
    if (!Object.keys(patch).length) return;
    setSaving(true);
    const optimistic = 'name' in patch ? applyViewerPatch({ name: patch.name ?? null }) : null;
    try {
      const updated = await updateMe(patch);
      optimistic?.settled();
      me.update(() => updated);
      const next = toForm(updated);
      setBase(next);
      setForm(next);
      toast({ kind: 'success', title: 'Profile updated successfully' });
    } catch (err) {
      optimistic?.rollback();
      const { message, fields } = apiFieldErrors(err);
      const mapped: FieldErrors = { ...fields };
      setErrors(mapped);
      setGeneral(Object.values(mapped).some(Boolean) ? null : message);
    } finally {
      setSaving(false);
    }
  };

  const verified = (emails.data ?? []).filter((e) => e.verified);
  const bioLeft = BIO_MAX - [...(form?.bio ?? '')].length;

  return (
    <>
      <PageHeader title="Public profile" description="This information appears on your profile page and next to your activity." />
      <div className={styles.profileGrid}>
        <form className={styles.profileForm} onSubmit={(e) => void submit(e)} noValidate aria-label="Public profile">
          {!form ? (
            <FormStack>
              {Array.from({ length: 6 }, (_, i) => (
                <Skeleton key={i} height={52} />
              ))}
            </FormStack>
          ) : (
            <FormStack>
              {general && <Banner tone="danger">{general}</Banner>}
              <Field label="Name" htmlFor={ids.name} error={errors.name} hint="Your name may appear around Better GitHub where you contribute or are mentioned. You can remove it at any time.">
                <Input id={ids.name} value={form.name} onChange={(e) => set('name', e.target.value)} invalid={!!errors.name} autoComplete="name" maxLength={255} />
              </Field>
              <Field
                label="Public email"
                htmlFor={ids.email}
                error={errors.email}
                hint={
                  <>
                    You can manage verified email addresses in your <Link to="/settings/emails">email settings</Link>.
                  </>
                }
              >
                <Select id={ids.email} value={form.email} onChange={(e) => set('email', e.target.value)} disabled={!emails.data}>
                  <option value="">Don't show my email address</option>
                  {form.email && !verified.some((v) => v.email.toLowerCase() === form.email.toLowerCase()) && <option value={form.email}>{form.email}</option>}
                  {verified.map((v) => (
                    <option key={v.email} value={v.email}>
                      {v.email}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field
                label="Bio"
                htmlFor={ids.bio}
                error={errors.bio}
                hint={
                  <span className={styles.counterRow}>
                    <span>Tell us a little bit about yourself.</span>
                    <span className={bioLeft < 0 ? styles.counterOver : styles.counter} aria-live="polite">
                      {bioLeft}
                    </span>
                  </span>
                }
              >
                <Textarea
                  id={ids.bio}
                  rows={3}
                  className={styles.bio}
                  value={form.bio}
                  onChange={(e) => set('bio', e.target.value)}
                  aria-invalid={!!errors.bio || bioLeft < 0}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) void submit();
                  }}
                />
              </Field>
              <Field label="URL" htmlFor={ids.blog} error={errors.blog}>
                <Input id={ids.blog} type="url" inputMode="url" placeholder="https://example.com" value={form.blog} onChange={(e) => set('blog', e.target.value)} invalid={!!errors.blog} autoComplete="url" />
              </Field>
              <Field label="X (Twitter) username" htmlFor={ids.tw} error={errors.twitter_username}>
                <Input id={ids.tw} value={form.twitter_username} leadingIcon={MentionIcon} onChange={(e) => set('twitter_username', e.target.value)} invalid={!!errors.twitter_username} placeholder="username" spellCheck={false} />
              </Field>
              <Field label="Company" htmlFor={ids.company} error={errors.company} hint="You can @mention your company's organization to link it.">
                <Input id={ids.company} value={form.company} onChange={(e) => set('company', e.target.value)} invalid={!!errors.company} autoComplete="organization" />
              </Field>
              <Field label="Location" htmlFor={ids.location} error={errors.location}>
                <Input id={ids.location} value={form.location} onChange={(e) => set('location', e.target.value)} invalid={!!errors.location} />
              </Field>
              <Checkbox checked={form.hireable} onChange={(v) => set('hireable', v)} label="Available for hire" description="Let people know you're open to job opportunities." />
              <p className={styles.small}>
                All of the fields on this page are optional and can be deleted at any time. By filling them out, you're giving us consent to share this data wherever
                your user profile appears.
              </p>
              <ButtonRow>
                <Button type="submit" variant="primary" loading={saving} disabled={!dirty}>
                  Update profile
                </Button>
                {dirty && !saving && (
                  <Button
                    variant="ghost"
                    onClick={() => {
                      setForm(base);
                      setErrors({});
                      setGeneral(null);
                    }}
                  >
                    Discard changes
                  </Button>
                )}
              </ButtonRow>
            </FormStack>
          )}
        </form>
        <AvatarPanel user={me.data ?? null} onChanged={(u) => me.data && me.update((prev) => ({ ...prev, avatar_url: u }))} fallback={user} />
      </div>
    </>
  );
});

// ------------------------------------------------------------------ avatar

const AvatarPanel = observer(function AvatarPanel({
  user,
  fallback,
  onChanged,
}: {
  user: PrivateUser | null;
  fallback: { login: string; name: string | null; avatarUrl: string } | null;
  onChanged: (avatarUrl: string) => void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [file, setFile] = useState<Blob | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [stamp, setStamp] = useState(0);
  const [over, setOver] = useState(false);
  const [confirmRemove, setConfirmRemove] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const current = session.user?.avatarUrl ?? user?.avatar_url ?? fallback?.avatarUrl ?? '';
  const shown = preview ?? (stamp ? bustAvatar(current, stamp) : current);
  const login = user?.login ?? fallback?.login ?? '';
  // Uploads are versioned by content hash (`?v=<12 hex>`); identicons use `?v=4` or nothing.
  const hasCustom = /^(data|blob):/.test(current) || /[?&]v=[0-9a-f]{12}(&|$)/.test(current);

  useEffect(() => () => void (preview && URL.revokeObjectURL(preview)), [preview]);

  const pick = (f: File | undefined | null) => {
    setError(null);
    if (!f) return;
    if (!IMAGE_TYPES.includes(f.type)) return setError('Choose a PNG, JPEG, GIF or WebP image.');
    if (f.size > MAX_SOURCE_BYTES) return setError('That image is too large. Choose one under 20 MB.');
    setFile(f);
  };

  const upload = async (blob: Blob) => {
    if (blob.size > MAX_AVATAR_BYTES) throw new Error('Avatar images must be 1 MB or smaller.');
    const url = URL.createObjectURL(blob);
    setPreview(url);
    setBusy(true);
    try {
      const res = await uploadAvatar(blob);
      const optimistic = applyViewerPatch({ avatarUrl: res.avatar_url });
      optimistic.settled();
      onChanged(res.avatar_url);
      setStamp(Date.now());
      toast({ kind: 'success', title: 'Your profile picture has been updated' });
    } catch (e) {
      throw e instanceof Error ? e : new Error('Upload failed');
    } finally {
      setBusy(false);
      setPreview(null);
    }
  };

  const remove = async () => {
    const res = await deleteAvatar();
    const optimistic = applyViewerPatch({ avatarUrl: res.avatar_url });
    optimistic.settled();
    onChanged(res.avatar_url);
    setStamp(Date.now());
    toast({ kind: 'success', title: 'Your profile picture has been reset' });
  };

  return (
    <aside className={styles.avatarCol} aria-label="Profile picture">
      <div className={styles.avatarLabel}>Profile picture</div>
      <div
        className={styles.dropZone}
        data-over={over || undefined}
        onDragOver={(e) => {
          if ([...e.dataTransfer.items].some((i) => i.kind === 'file')) {
            e.preventDefault();
            setOver(true);
          }
        }}
        onDragLeave={() => setOver(false)}
        onDrop={(e) => {
          e.preventDefault();
          setOver(false);
          pick(e.dataTransfer.files[0]);
        }}
      >
        <button type="button" className={styles.avatarButton} onClick={() => inputRef.current?.click()} aria-label="Upload a new profile picture">
          <Avatar user={{ login, name: user?.name ?? fallback?.name ?? null, avatarUrl: shown }} size={200} />
          {busy && (
            <span className={styles.avatarBusy}>
              <Spinner size={24} />
            </span>
          )}
          <span className={styles.avatarEdit}>
            <PencilIcon size={14} /> Edit
          </span>
        </button>
        <p className={styles.dropHint}>{over ? 'Drop the image to upload it' : 'Drag & drop an image, or'}</p>
      </div>
      <ButtonRow>
        <Button size="sm" leadingIcon={UploadIcon} onClick={() => inputRef.current?.click()} disabled={busy}>
          Upload a photo…
        </Button>
        {hasCustom && (
          <Button size="sm" variant="ghost" leadingIcon={TrashIcon} onClick={() => setConfirmRemove(true)} disabled={busy}>
            Remove
          </Button>
        )}
      </ButtonRow>
      {error && (
        <p className={styles.fieldError} role="alert">
          {error}
        </p>
      )}
      <input
        ref={inputRef}
        type="file"
        accept={IMAGE_TYPES.join(',')}
        hidden
        data-testid="avatar-file"
        onChange={(e) => {
          pick(e.target.files?.[0]);
          e.target.value = '';
        }}
      />
      <AvatarCropDialog file={file} onClose={() => setFile(null)} onCropped={upload} />
      <ConfirmDialog
        open={confirmRemove}
        onClose={() => setConfirmRemove(false)}
        title="Remove profile picture?"
        confirmLabel="Remove picture"
        onConfirm={remove}
      >
        <p>Your profile picture will be replaced by a generated identicon.</p>
      </ConfirmDialog>
    </aside>
  );
});

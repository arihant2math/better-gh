import { useId, useState, type FormEvent } from 'react';
import { invalidate } from '@/api/cache';
import {
  KEYS,
  addEmails,
  deleteEmails,
  isValidEmail,
  listEmails,
  resendVerification,
  setEmailVisibility,
  setPrimaryEmail,
  useEditableResource,
} from '@/api/userSettings';
import { Banner, ConfirmDialog, ItemList, ItemRow, PageHeader, Pill, Section, Toggle, apiFieldErrors, errorMessage } from '@/components/settings/kit';
import { Button, IconButton } from '@/ui/Button';
import { Skeleton } from '@/ui/EmptyState';
import { MailIcon, TrashIcon } from '@/ui/icons';
import { Field, Input, Select } from '@/ui/Input';
import { toast } from '@/ui/Toast';
import styles from './userSettings.module.css';
import type { UserEmail } from '@/api/types';

/** Client-side check for the "Add email address" field. */
export function emailError(value: string, existing: readonly UserEmail[]): string | null {
  const v = value.trim();
  if (!v) return 'Enter an email address';
  if (!isValidEmail(v)) return `${v} is not a valid email address`;
  if (existing.some((e) => e.email.toLowerCase() === v.toLowerCase())) return 'This email address is already on your account';
  return null;
}

export default function EmailSettings() {
  const res = useEditableResource(KEYS.emails, listEmails);
  const emails = res.data;
  const [newEmail, setNewEmail] = useState('');
  const [addError, setAddError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<UserEmail | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const addId = useId();
  const primaryId = useId();

  const primary = emails?.find((e) => e.primary);
  const verifiedOthers = (emails ?? []).filter((e) => e.verified);

  const add = async (ev?: FormEvent) => {
    ev?.preventDefault();
    if (!emails || adding) return;
    const err = emailError(newEmail, emails);
    setAddError(err);
    if (err) return;
    const email = newEmail.trim();
    setAdding(true);
    // Optimistic: show it right away as unverified.
    res.update((list) => [...list, { email, primary: false, verified: false, visibility: null }]);
    try {
      const added = await addEmails([email]);
      res.update((list) => list.map((e) => (e.email === email ? (added[0] ?? e) : e)));
      setNewEmail('');
      toast({ kind: 'success', title: `We sent a verification email to ${email}`, description: 'Follow the link in it to verify the address.' });
    } catch (e) {
      res.update((list) => list.filter((x) => x.email !== email));
      const { message, fields } = apiFieldErrors(e);
      setAddError(fields.email ?? fields.emails ?? message);
    } finally {
      setAdding(false);
    }
  };

  const run = async (key: string, fn: () => Promise<unknown>, ok?: string) => {
    setBusy(key);
    try {
      await fn();
      if (ok) toast({ kind: 'success', title: ok });
    } catch (e) {
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      setBusy(null);
    }
  };

  const makePrimary = (email: string) =>
    run(
      `primary:${email}`,
      async () => {
        const prev = emails;
        res.update((list) => {
          const vis = list.find((x) => x.primary)?.visibility ?? 'private';
          return list.map((x) => ({ ...x, primary: x.email === email, visibility: x.email === email ? vis : null }));
        });
        try {
          const next = await setPrimaryEmail(email);
          res.update(() => next);
          invalidate(KEYS.me);
        } catch (e) {
          if (prev) res.update(() => prev);
          throw e;
        }
      },
      `${email} is now your primary email address`,
    );

  const setPrivate = (on: boolean) =>
    run('visibility', async () => {
      const prev = emails;
      res.update((list) => list.map((x) => (x.primary ? { ...x, visibility: on ? 'private' : 'public' } : x)));
      try {
        const next = await setEmailVisibility(on ? 'private' : 'public');
        res.update(() => next);
        invalidate(KEYS.me);
      } catch (e) {
        if (prev) res.update(() => prev);
        throw e;
      }
    });

  return (
    <>
      <PageHeader title="Emails" description="Addresses you can use to sign in, receive notifications and attribute commits." />

      <Section title="Your email addresses">
        {res.error && !emails ? (
          <Banner tone="danger">{errorMessage(res.error)}</Banner>
        ) : !emails ? (
          <Skeleton height={120} />
        ) : (
          <ItemList aria-label="Email addresses">
            {emails.map((e) => (
              <ItemRow
                key={e.email}
                icon={MailIcon}
                title={
                  <>
                    <span data-testid="email-address">{e.email}</span>
                    {e.primary && <Pill tone="accent">Primary</Pill>}
                    {e.verified ? <Pill tone="success">Verified</Pill> : <Pill tone="warning">Unverified</Pill>}
                    {e.primary && <Pill>{e.visibility === 'public' ? 'Public' : 'Private'}</Pill>}
                  </>
                }
                meta={
                  e.primary
                    ? 'This email will be used for account-related notifications and password resets.'
                    : e.verified
                      ? 'Can be used to sign in and for commit attribution.'
                      : 'Unverified email addresses cannot receive notifications or be used to reset your password.'
                }
                actions={
                  <>
                    {!e.verified && (
                      <Button size="sm" loading={busy === `resend:${e.email}`} onClick={() => void run(`resend:${e.email}`, () => resendVerification(e.email), `Verification email sent to ${e.email}`)}>
                        Resend verification
                      </Button>
                    )}
                    {!e.primary && (
                      <IconButton icon={TrashIcon} label={`Remove ${e.email}`} size="sm" variant="ghost" onClick={() => setRemoving(e)} />
                    )}
                  </>
                }
              />
            ))}
          </ItemList>
        )}
        <form className={styles.addRow} onSubmit={(e) => void add(e)} noValidate>
          <Field label="Add email address" htmlFor={addId} error={addError}>
            <div className={styles.inlineRow}>
              <Input
                id={addId}
                type="email"
                className={styles.grow}
                placeholder="you@example.com"
                value={newEmail}
                invalid={!!addError}
                autoComplete="email"
                onChange={(e) => {
                  setNewEmail(e.target.value);
                  if (addError) setAddError(null);
                }}
              />
              <Button type="submit" loading={adding} disabled={!emails}>
                Add
              </Button>
            </div>
          </Field>
        </form>
      </Section>

      <Section title="Primary email address" description="Used for account notifications and password resets. Only verified addresses can be primary.">
        {emails ? (
          <div className={styles.inlineRow}>
            <Select
              id={primaryId}
              aria-label="Primary email address"
              className={styles.grow}
              value={primary?.email ?? ''}
              disabled={busy !== null}
              onChange={(e) => void makePrimary(e.target.value)}
            >
              {verifiedOthers.map((e) => (
                <option key={e.email} value={e.email}>
                  {e.email}
                </option>
              ))}
            </Select>
          </div>
        ) : (
          <Skeleton height={28} />
        )}
      </Section>

      <Section title="Email privacy">
        <Toggle
          checked={primary?.visibility !== 'public'}
          disabled={!primary || busy !== null}
          onChange={(v) => void setPrivate(v)}
          label="Keep my email address private"
          description={
            primary?.visibility === 'public'
              ? `${primary.email} is shown on your public profile.`
              : 'Your primary email address is not shown on your profile. You can still pick a public email in your profile settings.'
          }
        />
      </Section>

      <ConfirmDialog
        open={!!removing}
        onClose={() => setRemoving(null)}
        title="Remove email address?"
        confirmLabel="Remove"
        onConfirm={async () => {
          const target = removing;
          if (!target) return;
          await deleteEmails([target.email]);
          res.update((list) => list.filter((x) => x.email !== target.email));
          invalidate(KEYS.me);
          toast({ kind: 'success', title: `Removed ${target.email}` });
        }}
      >
        <p>
          <strong>{removing?.email}</strong> will be removed from your account. Commits authored with it will no longer link to your profile.
        </p>
      </ConfirmDialog>
    </>
  );
}

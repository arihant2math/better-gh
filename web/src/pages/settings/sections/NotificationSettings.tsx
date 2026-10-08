import { useId, useRef, useState } from 'react';
import { invalidate, useResource } from '@/api/cache';
import {
  getNotificationSettings,
  listUserEmails,
  putNotificationSettings,
  NOTIFICATION_REASONS,
  type NotificationSettings,
  type NotificationSettingsPatch,
} from '@/api/developerSettings';
import { Banner, errorMessage, FormStack, PageHeader, Section, Toggle } from '@/components/settings/kit';
import { Skeleton } from '@/ui/EmptyState';
import { AlertIcon } from '@/ui/icons';
import { Field, Select } from '@/ui/Input';
import { toast } from '@/ui/Toast';
import styles from '../developer/developer.module.css';

const KEY = 'dev:notification-settings';

/** Display metadata for bgh-notify reasons, grouped like github.com. */
const GROUPS: {
  title: string;
  reasons: { id: string; label: string; desc: string }[];
}[] = [
  {
    title: 'Participating',
    reasons: [
      {
        id: 'review_requested',
        label: 'Review requests',
        desc: 'You or a team you belong to were asked to review a pull request',
      },
      {
        id: 'assign',
        label: 'Assignments',
        desc: 'You were assigned to an issue or pull request',
      },
      {
        id: 'mention',
        label: '@mentions',
        desc: 'You were @mentioned in a conversation',
      },
      {
        id: 'team_mention',
        label: 'Team mentions',
        desc: 'A team you belong to was @mentioned',
      },
      {
        id: 'author',
        label: 'Your threads',
        desc: 'Activity on issues and pull requests you opened',
      },
      {
        id: 'comment',
        label: 'Comments',
        desc: 'New comments on conversations you commented on',
      },
      {
        id: 'state_change',
        label: 'State changes',
        desc: 'A thread you changed was closed, reopened or merged',
      },
      {
        id: 'manual',
        label: 'Custom subscriptions',
        desc: 'Threads you subscribed to manually',
      },
    ],
  },
  {
    title: 'Watching',
    reasons: [
      {
        id: 'subscribed',
        label: 'Watched repositories',
        desc: 'All activity in repositories you watch',
      },
    ],
  },
  {
    title: 'Actions',
    reasons: [
      {
        id: 'ci_activity',
        label: 'CI activity',
        desc: 'Workflow runs you triggered completed or failed',
      },
    ],
  },
  {
    title: 'Other',
    reasons: [
      {
        id: 'invitation',
        label: 'Invitations',
        desc: 'You were invited to a repository or organization',
      },
      {
        id: 'security_alert',
        label: 'Security alerts',
        desc: 'Vulnerability alerts for repositories you can access',
      },
    ],
  },
];

// Any reason the server knows but we didn't label still gets a row.
const LABELLED = new Set(GROUPS.flatMap((g) => g.reasons.map((r) => r.id)));
const UNLABELLED = NOTIFICATION_REASONS.filter((r) => !LABELLED.has(r));
if (UNLABELLED.length) GROUPS[GROUPS.length - 1]!.reasons.push(...UNLABELLED.map((id) => ({ id, label: id.replace(/_/g, ' '), desc: '' })));

function applyPatch(s: NotificationSettings, p: NotificationSettingsPatch): NotificationSettings {
  return {
    ...s,
    ...p,
    web: { ...s.web, ...p.web },
    email: { ...s.email, ...p.email },
    notification_email: p.notification_email !== undefined ? p.notification_email : s.notification_email,
  };
}

/** Inverse of `p` against `s` (for rollback of exactly the fields that were changed). */
function inverse(s: NotificationSettings, p: NotificationSettingsPatch): NotificationSettingsPatch {
  const inv: NotificationSettingsPatch = {};
  if (p.web) inv.web = Object.fromEntries(Object.keys(p.web).map((k) => [k, s.web[k] ?? true]));
  if (p.email) inv.email = Object.fromEntries(Object.keys(p.email).map((k) => [k, s.email[k] ?? true]));
  if (p.email_enabled !== undefined) inv.email_enabled = s.email_enabled;
  if (p.own_activity_email !== undefined) inv.own_activity_email = s.own_activity_email;
  if (p.notification_email !== undefined) inv.notification_email = s.notification_email;
  return inv;
}

/** `/settings/notifications`: which reasons reach the inbox and email; routing address. */
export default function NotificationSettingsPage() {
  const res = useResource(KEY, getNotificationSettings);
  const emails = useResource('dev:emails', listUserEmails);
  const [local, setLocal] = useState<NotificationSettings | null>(null);
  const settings = local ?? res.data ?? null;
  const pending = useRef(0);
  const latest = useRef<NotificationSettings | null>(null);
  latest.current = settings;
  const id = useId();

  const save = async (patch: NotificationSettingsPatch, what: string) => {
    const before = latest.current;
    if (!before) return;
    const undo = inverse(before, patch);
    const next = applyPatch(before, patch);
    latest.current = next;
    setLocal(next);
    pending.current++;
    try {
      const server = await putNotificationSettings(patch);
      pending.current--;
      invalidate(KEY);
      // Only adopt the server state when no later toggle is still in flight.
      if (pending.current === 0) setLocal(server);
    } catch (e) {
      pending.current--;
      setLocal((cur) => {
        const rolled = applyPatch(cur ?? before, undo);
        latest.current = rolled;
        return rolled;
      });
      toast({
        kind: 'error',
        title: `Couldn’t save “${what}”`,
        description: errorMessage(e),
      });
    }
  };

  if (!settings) {
    return (
      <>
        <PageHeader title="Notifications" />
        {res.error ? (
          <Banner tone="danger" icon={AlertIcon}>
            Could not load your notification settings: {errorMessage(res.error)}
          </Banner>
        ) : (
          <FormStack wide>
            <Skeleton height={20} width="30%" />
            <Skeleton height={240} />
          </FormStack>
        )}
      </>
    );
  }

  const verified = (emails.data ?? []).filter((e) => e.verified);
  const primary = verified.find((e) => e.primary)?.email;
  const emailOff = !settings.email_enabled;

  return (
    <>
      <PageHeader title="Notifications" description="Choose how you hear about activity on GitHub. Changes are saved automatically." />
      <Section title="Default notifications email" description="Notification emails go to this address. Only verified addresses can be used.">
        <FormStack>
          <Field label="Email address" htmlFor={`${id}-email`}>
            <Select
              id={`${id}-email`}
              className={styles.emailSelect}
              value={settings.notification_email ?? ''}
              disabled={emailOff}
              onChange={(e) => void save({ notification_email: e.target.value || null }, 'Default notifications email')}
            >
              <option value="">{primary ? `${primary} (primary)` : 'Primary email address'}</option>
              {verified
                .filter((e) => !e.primary)
                .map((e) => (
                  <option key={e.email} value={e.email}>
                    {e.email}
                  </option>
                ))}
              {settings.notification_email && !verified.some((e) => e.email.toLowerCase() === settings.notification_email!.toLowerCase()) && (
                <option value={settings.notification_email}>{settings.notification_email}</option>
              )}
            </Select>
          </Field>
          <Toggle
            checked={settings.email_enabled}
            onChange={(v) => void save({ email_enabled: v }, 'Email notifications')}
            label="Email notifications"
            description="Master switch for all notification email. Unsubscribe links in emails turn this off."
          />
          <Toggle
            checked={settings.own_activity_email}
            disabled={emailOff}
            onChange={(v) => void save({ own_activity_email: v }, 'Include your own updates')}
            label="Include your own updates"
            description="Email me about comments, reviews and pushes I make myself."
          />
        </FormStack>
      </Section>
      <Section title="Notification types" description="Pick the channels for each reason you get notified. Web means your notifications inbox.">
        {emailOff && (
          <Banner tone="info" icon={AlertIcon}>
            Email notifications are turned off, so the Email column has no effect until you turn them back on.
          </Banner>
        )}
        <table className={styles.matrix} aria-label="Notification channels by reason">
          <thead>
            <tr>
              <th scope="col">Reason</th>
              <th scope="col" className={styles.channel}>
                Web
              </th>
              <th scope="col" className={styles.channel}>
                Email
              </th>
            </tr>
          </thead>
          {GROUPS.map((g) => (
            <tbody key={g.title}>
              <tr className={styles.groupRow}>
                <th colSpan={3} scope="colgroup">
                  {g.title}
                </th>
              </tr>
              {g.reasons.map((r) => (
                <tr key={r.id}>
                  <th scope="row" style={{ fontWeight: 'normal' }}>
                    <div className={styles.reasonName}>{r.label}</div>
                    {r.desc && <div className={styles.reasonDesc}>{r.desc}</div>}
                  </th>
                  <td className={styles.channel}>
                    <input
                      type="checkbox"
                      aria-label={`${r.label}: Web`}
                      data-reason={r.id}
                      data-channel="web"
                      checked={settings.web[r.id] ?? true}
                      onChange={(e) => void save({ web: { [r.id]: e.target.checked } }, `${r.label} (web)`)}
                    />
                  </td>
                  <td className={styles.channel}>
                    <input
                      type="checkbox"
                      aria-label={`${r.label}: Email`}
                      data-reason={r.id}
                      data-channel="email"
                      checked={settings.email[r.id] ?? true}
                      disabled={emailOff}
                      onChange={(e) => void save({ email: { [r.id]: e.target.checked } }, `${r.label} (email)`)}
                    />
                  </td>
                </tr>
              ))}
            </tbody>
          ))}
        </table>
      </Section>
    </>
  );
}

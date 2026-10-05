import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { KEYS, listIdentities, unlinkIdentity, useEditableResource, type SsoIdentity } from '../../../api/userSettings';
import { session } from '../../../app/session';
import { Banner, ConfirmDialog, ItemList, ItemRow, PageHeader, Section, errorMessage } from '../../../components/settings/kit';
import { Button } from '../../../ui/Button';
import { Skeleton } from '../../../ui/EmptyState';
import { InfoIcon, KeyIcon, LinkIcon } from '../../../ui/icons';
import { RelativeTime } from '../../../ui/RelativeTime';
import { toast } from '../../../ui/Toast';
import { Tooltip } from '../../../ui/Tooltip';
import styles from './userSettings.module.css';

/**
 * Username changes and account deletion have no self-service endpoint in
 * bgh-accounts (only site admins: PATCH/DELETE /admin/users/{u}), so those
 * controls are shown disabled with an explanation.
 */
const RENAME_UNAVAILABLE = 'Username changes are not available on this server yet. A site administrator can rename your account.';
const DELETE_UNAVAILABLE = 'Self-service account deletion is not available on this server yet. Ask a site administrator to delete your account.';

export default observer(function AccountSettings() {
  const login = session.user?.login ?? '';
  return (
    <>
      <PageHeader title="Account" description="Your username, linked sign-in providers and account lifecycle." />

      <Section title="Change username" description="Changing your username can have unintended side effects.">
        <div className={styles.inlineRow}>
          <div className={styles.grow}>
            <div className={styles.muted}>Your username is</div>
            <div className={styles.mono} data-testid="current-login">
              {login}
            </div>
          </div>
          <Tooltip label={RENAME_UNAVAILABLE}>
            <Button disabled aria-describedby="rename-unavailable">
              Change username
            </Button>
          </Tooltip>
        </div>
        <p id="rename-unavailable" className={styles.small}>
          <InfoIcon size={14} /> {RENAME_UNAVAILABLE} Old links to your profile and repositories would redirect for a limited time only.
        </p>
      </Section>

      <IdentitiesSection />

      <Section danger title="Delete account" description="Once you delete your account, there is no going back. Please be certain.">
        <div className={styles.inlineRow}>
          <p className={styles.grow}>Your repositories, issues and comments will be removed or attributed to a ghost user.</p>
          <Tooltip label={DELETE_UNAVAILABLE}>
            <Button variant="danger" disabled aria-describedby="delete-unavailable">
              Delete your account
            </Button>
          </Tooltip>
        </div>
        <p id="delete-unavailable" className={styles.small}>
          <InfoIcon size={14} /> {DELETE_UNAVAILABLE}
        </p>
      </Section>
    </>
  );
});

function IdentitiesSection() {
  const ids = useEditableResource(KEYS.identities, listIdentities);
  const [unlink, setUnlink] = useState<SsoIdentity | null>(null);
  return (
    <Section title="Linked sign-in identities" description="Single sign-on providers you can use to sign in to this account.">
      {ids.error ? (
        <Banner tone="danger">{errorMessage(ids.error)}</Banner>
      ) : !ids.data ? (
        <Skeleton height={56} />
      ) : (
        <ItemList aria-label="Linked identities" empty="No single sign-on identities are linked to this account.">
          {ids.data.map((i) => (
            <ItemRow
              key={i.id}
              icon={KeyIcon}
              title={
                <>
                  <span className={styles.capitalize}>{i.provider}</span>
                  {i.email && <span className={styles.muted}>{i.email}</span>}
                </>
              }
              meta={
                <>
                  Linked <RelativeTime date={i.created_at} /> · Last used <RelativeTime date={i.last_login_at} /> · <span className={styles.mono}>{i.subject}</span>
                </>
              }
              actions={
                <Button size="sm" variant="danger" leadingIcon={LinkIcon} onClick={() => setUnlink(i)}>
                  Unlink
                </Button>
              }
            />
          ))}
        </ItemList>
      )}
      <ConfirmDialog
        open={!!unlink}
        onClose={() => setUnlink(null)}
        title={`Unlink ${unlink?.provider ?? ''}?`}
        confirmLabel="Unlink identity"
        onConfirm={async () => {
          if (!unlink) return;
          await unlinkIdentity(unlink.id);
          ids.update((list) => list.filter((x) => x.id !== unlink.id));
          toast({ kind: 'success', title: `Unlinked ${unlink.provider}` });
        }}
      >
        <p>You will no longer be able to sign in with this {unlink?.provider} account. If you don't have a password, set one first.</p>
      </ConfirmDialog>
    </Section>
  );
}

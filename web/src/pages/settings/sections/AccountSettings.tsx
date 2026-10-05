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
import { DeleteAccountDialog, RenameUserDialog } from './AccountDialogs';
import styles from './userSettings.module.css';

export default observer(function AccountSettings() {
  const login = session.user?.login ?? '';
  const [dialog, setDialog] = useState<'rename' | 'delete' | null>(null);
  const close = () => setDialog(null);
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
          <Button onClick={() => setDialog('rename')} disabled={!login}>
            Change username
          </Button>
        </div>
        <p className={styles.small}>
          <InfoIcon size={14} /> Links to your profile and repositories and git remotes keep working: requests to the old name redirect. The old name stays reserved
          for you for 90 days.
        </p>
      </Section>

      <IdentitiesSection />

      <Section danger title="Delete account" description="Once you delete your account, there is no going back. Please be certain.">
        <div className={styles.inlineRow}>
          <p className={styles.grow}>Your repositories, issues and comments will be removed or attributed to a ghost user.</p>
          <Button variant="danger" onClick={() => setDialog('delete')} disabled={!login}>
            Delete your account
          </Button>
        </div>
      </Section>

      <RenameUserDialog open={dialog === 'rename'} onClose={close} login={login} />
      <DeleteAccountDialog open={dialog === 'delete'} onClose={close} login={login} />
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

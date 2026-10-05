import { useState } from 'react';
import { useResource } from '../../api/cache';
import { acceptRepoInvitation, declineRepoInvitation, INVITE_KEYS, listRepoInvitations } from '../../api/invitations';
import { Banner, errorMessage } from '../../components/settings/kit';
import { Link, navigate, useParams } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { CheckIcon, LockIcon, MailIcon, RepoIcon, XIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import { AvatarPair, CardSkeleton, InvitationCard, invitationStyles as styles, LoadError } from './common';
import { findRepoInvitation, permissionLabel } from './model';

/** `/:owner/:repo/invitations`: accept or decline a repository invitation. */
export default function RepoInvitationPage() {
  const { owner = '', repo = '' } = useParams();
  const res = useResource(INVITE_KEYS.repos, listRepoInvitations, { ttlMs: 5_000 });
  const [busy, setBusy] = useState<'accept' | 'decline' | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (!res.data && res.error) return <InvitationCard label="Invitation"><LoadError error={res.error} /></InvitationCard>;
  if (!res.data) return <CardSkeleton />;
  const inv = findRepoInvitation(res.data, owner, repo);
  if (!inv) {
    return (
      <InvitationCard label="Invitation not found">
        <EmptyState
          icon={MailIcon}
          title="No pending invitation"
          action={
            <span className={styles.actions}>
              <Link to={`/${owner}/${repo}`}>Go to {owner}/{repo}</Link>
              <Link to="/">Your dashboard</Link>
            </span>
          }
        >
          You have no pending invitation to {owner}/{repo}. It may have expired, been cancelled, or already been accepted.
        </EmptyState>
      </InvitationCard>
    );
  }

  const full = inv.repository.full_name;
  const accept = async () => {
    setBusy('accept');
    setError(null);
    try {
      await acceptRepoInvitation(inv.id);
      toast({ kind: 'success', title: `You now have access to ${full}` });
      navigate(`/${full}`);
    } catch (e) {
      setError(errorMessage(e));
      setBusy(null);
    }
  };
  const decline = async () => {
    setBusy('decline');
    setError(null);
    try {
      await declineRepoInvitation(inv.id);
      toast({ kind: 'success', title: `Declined the invitation to ${full}` });
      navigate('/');
    } catch (e) {
      setError(errorMessage(e));
      setBusy(null);
    }
  };

  return (
    <InvitationCard label={`Invitation to ${full}`}>
      <AvatarPair from={inv.inviter} to={inv.repository.owner} square={inv.repository.owner.type === 'Organization'} />
      <h1 className={styles.title}>
        {inv.inviter ? `@${inv.inviter.login} invited you to collaborate on ` : 'You have been invited to collaborate on '}
        <strong>{full}</strong>
      </h1>
      {inv.repository.description && <p className={styles.muted}>{inv.repository.description}</p>}
      <dl className={styles.facts}>
        <dt>Repository</dt>
        <dd>
          {inv.repository.private ? <LockIcon size={14} /> : <RepoIcon size={14} />} {full}
        </dd>
        <dt>Permission</dt>
        <dd data-testid="invitation-permission">{permissionLabel(inv.permissions)}</dd>
        <dt>Invited</dt>
        <dd>
          <RelativeTime date={inv.created_at} />
        </dd>
      </dl>
      {inv.expired && <Banner tone="warning">This invitation has expired. Ask {inv.inviter ? `@${inv.inviter.login}` : 'a repository admin'} to invite you again.</Banner>}
      {error && (
        <div className={styles.error}>
          <Banner tone="danger">{error}</Banner>
        </div>
      )}
      <div className={styles.actions}>
        <Button variant="primary" leadingIcon={CheckIcon} loading={busy === 'accept'} disabled={!!busy || inv.expired} onClick={() => void accept()}>
          Accept invitation
        </Button>
        <Button leadingIcon={XIcon} loading={busy === 'decline'} disabled={!!busy} onClick={() => void decline()}>
          Decline
        </Button>
      </div>
    </InvitationCard>
  );
}

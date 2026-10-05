import { useState } from 'react';
import { useResource } from '../../api/cache';
import { acceptOrgInvitation, declineOrgInvitation, getOrgInvitation, INVITE_KEYS } from '../../api/invitations';
import { session } from '../../app/session';
import { Banner, errorMessage } from '../../components/settings/kit';
import { Link, navigate, useParams } from '../../router';
import { Button } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { CheckIcon, MailIcon, OrganizationIcon, XIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import { AvatarPair, CardSkeleton, InvitationCard, invitationStyles as styles, isNotFound, LoadError } from './common';
import { orgRoleLabel } from './model';

/** `/orgs/:org/invitation`: accept or decline an organization invitation. */
export default function OrgInvitationPage() {
  const { org = '' } = useParams();
  const res = useResource(INVITE_KEYS.org(org), () => getOrgInvitation(org), { ttlMs: 5_000 });
  const [busy, setBusy] = useState<'accept' | 'decline' | null>(null);
  const [error, setError] = useState<string | null>(null);
  const inv = res.data;

  if (!inv && res.error) {
    if (!isNotFound(res.error)) return <InvitationCard label="Invitation"><LoadError error={res.error} /></InvitationCard>;
    return (
      <InvitationCard label="Invitation not found">
        <EmptyState icon={MailIcon} title="No pending invitation" action={<Link to="/">Go to your dashboard</Link>}>
          {session.user ? `@${session.user.login} has` : 'You have'} no pending invitation to the {org} organization. It may have been cancelled or declined. If you were invited by
          email, <Link to="/settings/emails">verify that address</Link> first.
        </EmptyState>
      </InvitationCard>
    );
  }
  if (!inv) return <CardSkeleton />;

  const login = inv.organization.login;
  if (inv.state === 'active') {
    return (
      <InvitationCard label="Already a member">
        <AvatarPair from={null} to={inv.organization} square />
        <h1 className={styles.title}>You’re a member of {login}</h1>
        <p className={styles.muted}>Your role is {orgRoleLabel(inv.role).toLowerCase()}.</p>
        <div className={styles.actions}>
          <Button variant="primary" onClick={() => navigate(`/${login}`)}>
            Go to {login}
          </Button>
        </div>
      </InvitationCard>
    );
  }

  const accept = async () => {
    setBusy('accept');
    setError(null);
    try {
      await acceptOrgInvitation(login);
      toast({ kind: 'success', title: `You joined ${login}` });
      navigate(`/${login}`);
    } catch (e) {
      setError(errorMessage(e));
      setBusy(null);
    }
  };
  const decline = async () => {
    setBusy('decline');
    setError(null);
    try {
      await declineOrgInvitation(login);
      toast({ kind: 'success', title: `Declined the invitation to ${login}` });
      navigate('/');
    } catch (e) {
      setError(errorMessage(e));
      setBusy(null);
    }
  };

  return (
    <InvitationCard label={`Invitation to ${login}`}>
      <AvatarPair from={inv.inviter} to={inv.organization} square />
      <h1 className={styles.title}>
        {inv.inviter ? `@${inv.inviter.login} has invited you to join the ` : 'You have been invited to join the '}
        <strong>{inv.organization_name || login}</strong> organization
      </h1>
      {inv.organization.description && <p className={styles.muted}>{inv.organization.description}</p>}
      <dl className={styles.facts}>
        <dt>Organization</dt>
        <dd>
          <OrganizationIcon size={14} /> {login}
        </dd>
        <dt>Role</dt>
        <dd data-testid="invitation-role">{orgRoleLabel(inv.role)}</dd>
        {inv.teams.length > 0 && (
          <>
            <dt>Teams</dt>
            <dd>{inv.teams.join(', ')}</dd>
          </>
        )}
        {inv.created_at && (
          <>
            <dt>Invited</dt>
            <dd>
              <RelativeTime date={inv.created_at} />
            </dd>
          </>
        )}
      </dl>
      {inv.role === 'admin' && <p className={styles.muted}>Owners have full administrative access to the organization and its repositories.</p>}
      {error && (
        <div className={styles.error}>
          <Banner tone="danger">{error}</Banner>
        </div>
      )}
      <div className={styles.actions}>
        <Button variant="primary" leadingIcon={CheckIcon} loading={busy === 'accept'} disabled={!!busy} onClick={() => void accept()}>
          Join {login}
        </Button>
        <Button leadingIcon={XIcon} loading={busy === 'decline'} disabled={!!busy} onClick={() => void decline()}>
          Decline
        </Button>
      </div>
    </InvitationCard>
  );
}

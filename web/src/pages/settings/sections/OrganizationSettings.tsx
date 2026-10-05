import { useState } from 'react';
import { useResource } from '../../../api/cache';
import {
  INVITE_KEYS,
  leaveOrganization,
  listMyOrganizations,
  listPendingOrgInvitations,
  setMembershipPublic,
  type ViewerOrganization,
} from '../../../api/invitations';
import { useEditableResource } from '../../../api/userSettings';
import { session } from '../../../app/session';
import { Banner, ConfirmDialog, ItemList, ItemRow, PageHeader, Pill, Section, errorMessage } from '../../../components/settings/kit';
import { Link } from '../../../router';
import { Avatar } from '../../../ui/Badge';
import { Button } from '../../../ui/Button';
import { Skeleton } from '../../../ui/EmptyState';
import { EyeClosedIcon, EyeIcon, SignOutIcon } from '../../../ui/icons';
import { toast } from '../../../ui/Toast';
import { leaveBlockReason, orgInvitationHref, orgRoleLabel } from '../../invitations/model';

/** `/settings/organizations`: the viewer's memberships (leave, publicize) and pending invitations. */
export default function OrganizationSettings() {
  const me = session.user!.login;
  const res = useEditableResource(INVITE_KEYS.orgs, listMyOrganizations);
  const pending = useResource(`${INVITE_KEYS.pending}:orgs`, listPendingOrgInvitations, { ttlMs: 10_000 });
  const [leaving, setLeaving] = useState<ViewerOrganization | null>(null);
  const [busy, setBusy] = useState<Set<string>>(new Set());

  const mark = (login: string, on: boolean) =>
    setBusy((b) => {
      const n = new Set(b);
      if (on) n.add(login);
      else n.delete(login);
      return n;
    });

  const togglePublic = async (o: ViewerOrganization) => {
    const login = o.organization.login;
    const next = !o.public;
    mark(login, true);
    res.update((l) => l.map((x) => (x.organization.id === o.organization.id ? { ...x, public: next } : x)));
    try {
      await setMembershipPublic(login, me, next);
      toast({ kind: 'success', title: next ? `Your membership in ${login} is now public` : `Your membership in ${login} is now private` });
    } catch (e) {
      res.update((l) => l.map((x) => (x.organization.id === o.organization.id ? { ...x, public: o.public } : x)));
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      mark(login, false);
    }
  };

  const leave = async (o: ViewerOrganization) => {
    await leaveOrganization(o.organization.login, me);
    res.update((l) => l.filter((x) => x.organization.id !== o.organization.id));
    toast({ kind: 'success', title: `You left ${o.organization.login}` });
  };

  const orgs = res.data ?? [];
  const invites = (pending.data ?? []).filter((m) => m.state === 'pending');

  return (
    <>
      <PageHeader title="Organizations" description="Organizations you belong to. Public memberships appear on your profile and in the organization’s people list." />
      {invites.length > 0 && (
        <Section title="Pending invitations">
          <ItemList aria-label="Pending invitations">
            {invites.map((m) => (
              <ItemRow
                key={m.organization.id}
                leading={<Avatar user={{ login: m.organization.login, avatarUrl: m.organization.avatar_url }} size={32} square />}
                title={m.organization.login}
                meta={`Invited as ${orgRoleLabel(m.role).toLowerCase()}`}
                actions={
                  <Link to={orgInvitationHref(m.organization.login)} data-testid="view-org-invitation">
                    View invitation
                  </Link>
                }
              />
            ))}
          </ItemList>
        </Section>
      )}
      <Section title={`Your organizations${res.data ? ` (${orgs.length})` : ''}`}>
        {res.error && !res.data ? (
          <Banner tone="danger">{errorMessage(res.error)}</Banner>
        ) : !res.data ? (
          <Skeleton height={60} />
        ) : (
          <ItemList
            aria-label="Your organizations"
            empty={
              <>
                You are not a member of any organization. <Link to="/organizations/new">Create one</Link>.
              </>
            }
          >
            {orgs.map((o) => {
              const login = o.organization.login;
              const blocked = leaveBlockReason(o);
              return (
                <ItemRow
                  key={o.organization.id}
                  leading={<Avatar user={{ login, avatarUrl: o.organization.avatar_url, name: o.organization_name }} size={32} square />}
                  title={
                    <>
                      <Link to={`/${login}`} data-testid="org-login">
                        {login}
                      </Link>{' '}
                      <Pill tone={o.role === 'admin' ? 'accent' : 'neutral'}>{orgRoleLabel(o.role)}</Pill>{' '}
                      <Pill tone={o.public ? 'success' : 'neutral'}>{o.public ? 'Public' : 'Private'}</Pill>
                    </>
                  }
                  meta={`${o.members_count} member${o.members_count === 1 ? '' : 's'}${blocked ? ` · ${blocked}` : ''}`}
                  actions={
                    <>
                      <Button size="sm" leadingIcon={o.public ? EyeClosedIcon : EyeIcon} loading={busy.has(login)} onClick={() => void togglePublic(o)}>
                        {o.public ? 'Make private' : 'Make public'}
                      </Button>
                      {o.role === 'admin' && (
                        <Link to={`/organizations/${login}/settings`}>Settings</Link>
                      )}
                      <Button size="sm" variant="danger" leadingIcon={SignOutIcon} disabled={!!blocked} title={blocked ?? undefined} onClick={() => setLeaving(o)}>
                        Leave
                      </Button>
                    </>
                  }
                />
              );
            })}
          </ItemList>
        )}
      </Section>
      <ConfirmDialog
        open={!!leaving}
        onClose={() => setLeaving(null)}
        title={leaving ? `Leave ${leaving.organization.login}?` : 'Leave organization'}
        confirmLabel="Leave organization"
        confirmText={leaving?.organization.login}
        onConfirm={() => leaving && leave(leaving)}
      >
        <p>You will lose access to the organization’s private repositories and be removed from all of its teams. An owner must invite you again to rejoin.</p>
      </ConfirmDialog>
    </>
  );
}

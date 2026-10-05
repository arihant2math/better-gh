import { useState } from 'react';
import { getTwoFactor } from '../../api/userSettings';
import { mutate, useResource } from '../../api/cache';
import { ErrorState, PageHeader, Panel, Switch, errorMessage, useConfirm } from '../../components/admin/kit';
import { Link, useParams } from '../../router';
import { Avatar } from '../../ui/Badge';
import { Banner } from '../../components/settings/kit';
import { Skeleton } from '../../ui/EmptyState';
import { AlertIcon } from '../../ui/icons';
import { toast } from '../../ui/Toast';
import { getOrg, listWithoutTwoFactor, orgKey, updateOrg, type NoTwoFactorAccount, type OrgFull } from './api';
import { OwnerRequired, useOrgAccess } from './common';
import local from './OrgSettings.module.css';

/** `/organizations/:org/settings/security`: require two-factor authentication. */
export default function OrgSecurityPage() {
  const { org = '' } = useParams<{ org: string }>();
  const access = useOrgAccess(org);
  const res = useResource(orgKey(org), () => getOrg(org));
  const mine = useResource('settings:2fa', getTwoFactor);
  const confirm = useConfirm();
  const [saving, setSaving] = useState(false);

  if (!access.loading && !access.isOwner) return <OwnerRequired org={org} what="change authentication security settings" />;
  if (res.error && !res.data) return <ErrorState error={res.error} />;
  const o = res.data;
  const enabled = !!o?.two_factor_requirement_enabled;
  const ownerHas2fa = mine.data?.enabled ?? true;

  const save = async (value: boolean) => {
    setSaving(true);
    try {
      const updated = await updateOrg(org, { two_factor_requirement_enabled: value });
      mutate<OrgFull>(orgKey(org), (prev) => ({
        ...(prev ?? {}),
        ...updated,
        two_factor_requirement_enabled: updated.two_factor_requirement_enabled ?? value,
      }));
      toast({ kind: 'success', title: value ? 'Two-factor authentication is now required' : 'Two-factor requirement turned off' });
    } finally {
      setSaving(false);
    }
  };

  const toggle = async (value: boolean) => {
    if (!value) {
      await save(false).catch((e: unknown) => toast({ kind: 'error', title: errorMessage(e) }));
      return;
    }
    let affected: { members: NoTwoFactorAccount[]; collaborators: NoTwoFactorAccount[] };
    try {
      affected = await listWithoutTwoFactor(org);
    } catch (e) {
      toast({ kind: 'error', title: errorMessage(e) });
      return;
    }
    const all = [...affected.members.map((u) => ({ ...u, kind: 'member' })), ...affected.collaborators.map((u) => ({ ...u, kind: 'outside collaborator' }))];
    confirm({
      title: 'Require two-factor authentication',
      confirmLabel: all.length ? `Remove ${all.length} and require 2FA` : 'Require two-factor authentication',
      danger: all.length > 0,
      body: all.length ? (
        <div>
          <p>
            These accounts don't have two-factor authentication enabled and will be <strong>removed</strong> from {org}. They are emailed, and get their access
            back if they enable 2FA and rejoin.
          </p>
          <ul className={local.affectedList} data-testid="no-2fa-accounts">
            {all.map((u) => (
              <li key={u.login}>
                <Avatar user={{ login: u.login, avatarUrl: u.avatar_url }} size={20} /> {u.login} <span className={local.muted}>({u.kind})</span>
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <p>Everyone in {org} already uses two-factor authentication. New members will need it to join.</p>
      ),
      onConfirm: () => save(true),
    });
  };

  return (
    <>
      <PageHeader title="Authentication security" description="Two-factor authentication requirements for members and outside collaborators." />
      {!o ? (
        <Skeleton height={96} />
      ) : (
        <Panel title="Two-factor authentication">
          <div className={local.stack}>
            {!ownerHas2fa && !enabled && (
              <Banner tone="warning" icon={AlertIcon}>
                You must <Link to="/settings/security">enable two-factor authentication</Link> on your own account before you can require it for the
                organization.
              </Banner>
            )}
            <Switch
              label="Require two-factor authentication for everyone in the organization"
              description="Members, outside collaborators and invitees without 2FA are removed or can't join. Invitations to accounts without 2FA are refused."
              checked={enabled}
              disabled={saving || (!enabled && !ownerHas2fa)}
              onChange={(v) => void toggle(v)}
            />
          </div>
        </Panel>
      )}
      {confirm.dialog}
    </>
  );
}

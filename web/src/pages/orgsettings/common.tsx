/** Pieces shared by the organization settings pages. */
import { useEffect, useRef, useState, type ReactNode } from 'react';
import { mutate, useResource } from '../../api/cache';
import { fieldErrors, type FieldErrors } from '../../api/errors';
import { getBoot } from '../../boot';
import { navigate } from '../../router';
import styles from '../../components/admin/admin.module.css';
import { RadioCards, StatusPill, errorMessage } from '../../components/admin/kit';
import { invalidateLists } from '../../components/admin/usePagedList';
import { Avatar, Tag } from '../../ui/Badge';
import { Button, IconButton } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import { KebabHorizontalIcon, LockIcon } from '../../ui/icons';
import { Field, Input, Select, Textarea } from '../../ui/Input';
import { Menu, SelectPanel, type MenuEntry } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import {
  allTeamsKey,
  createInvitation,
  createTeam,
  getMembership,
  getUser,
  invitationsPrefix,
  listAllTeams,
  membershipKey,
  teamKey,
  viewerLogin,
  type InvitationInput,
  type TeamFull,
  type TeamPrivacy,
} from './api';
import local from './OrgSettings.module.css';
import type { RestTeam, SimpleUser } from '../../api/types';

/** The viewer's standing in `org` (owners manage settings; site admins too). */
export function useOrgAccess(org: string) {
  const me = viewerLogin();
  const res = useResource(me ? membershipKey(org, me) : null, () => getMembership(org, me!));
  const siteAdmin = !!getBoot().user?.siteAdmin;
  const active = res.data?.state === 'active';
  return {
    me,
    loading: res.loading,
    isOwner: (active && res.data?.role === 'admin') || siteAdmin,
    isMember: active || siteAdmin,
  };
}

/** Shown in place of a page that needs owner rights. */
export function OwnerRequired({ org, what }: { org: string; what: string }) {
  return (
    <EmptyState icon={LockIcon} title="You must be an owner">
      Only owners of <strong>{org}</strong> can {what}.
    </EmptyState>
  );
}

export const userCell = (u: Pick<SimpleUser, 'login' | 'avatar_url'> & { name?: string | null }, sub?: ReactNode, me?: string | null) => (
  <>
    <Avatar user={{ login: u.login, avatarUrl: u.avatar_url, name: u.name }} size={24} />
    <span className={styles.cellMain}>
      <strong>
        {u.login} {me === u.login && <Tag>You</Tag>}
      </strong>
      {sub !== undefined && <span className={styles.subtle}>{sub || ' '}</span>}
    </span>
  </>
);

/** Keep loading `next` pages until the list is complete (for client-side search). */
export function useLoadAll(list: { next: string | null; loading: boolean; error: unknown; loadMore: () => unknown }) {
  useEffect(() => {
    if (list.next && !list.loading && !list.error) void list.loadMore();
  }, [list]);
}

/** Every team of the org, cached (pickers, tree, parent selects). */
export function useAllTeams(org: string, enabled = true) {
  return useResource<RestTeam[]>(enabled ? allTeamsKey(org) : null, () => listAllTeams(org));
}

/** Kebab button + menu for a row. Clicks don't reach the row's open handler. */
export function RowMenu({ label, items }: { label: string; items: MenuEntry[] }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <span className={local.rowMenu} onClick={(e) => e.stopPropagation()}>
      <IconButton ref={ref} icon={KebabHorizontalIcon} label={label} size="sm" onClick={() => setOpen((o) => !o)} aria-haspopup="menu" aria-expanded={open} />
      <Menu open={open} onClose={() => setOpen(false)} anchor={ref} items={items} placement="bottom-end" aria-label={label} />
    </span>
  );
}

export const INVITE_ROLES: { value: InvitationInput['role']; label: string; description: string }[] = [
  { value: 'direct_member', label: 'Member', description: 'Can see every member and be granted access to repositories. Can create repositories if allowed.' },
  { value: 'admin', label: 'Owner', description: 'Full administrative rights to the organization: settings, members, teams and every repository.' },
  { value: 'billing_manager', label: 'Billing manager', description: 'Can manage billing settings only; no access to repositories.' },
];

export const INVITE_ROLE_LABEL: Record<string, string> = {
  direct_member: 'Member',
  admin: 'Owner',
  billing_manager: 'Billing manager',
  hiring_manager: 'Hiring manager',
  reinstate: 'Reinstate',
};

/** "Invite member" dialog: by username (resolved to `invitee_id`) or email, role and teams. */
export function InviteDialog({ org, open, onClose, initialLogin = '' }: { org: string; open: boolean; onClose: () => void; initialLogin?: string }) {
  const [who, setWho] = useState(initialLogin);
  const [role, setRole] = useState<InvitationInput['role']>('direct_member');
  const [teamIds, setTeamIds] = useState<number[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [shownFor, setShownFor] = useState<boolean>(false);
  if (open !== shownFor) {
    setShownFor(open);
    if (open) {
      setWho(initialLogin);
      setRole('direct_member');
      setTeamIds([]);
      setError(null);
      setErrors({});
    }
  }
  const teams = useAllTeams(org, open);
  const teamsRef = useRef<HTMLButtonElement>(null);
  const [picking, setPicking] = useState(false);
  const value = who.trim().replace(/^@/, '');
  const isEmail = value.includes('@');
  const valid = isEmail ? /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value) : /^[a-z\d](?:[a-z\d]|-(?=[a-z\d])){0,38}$/i.test(value);

  const submit = async () => {
    if (!valid || busy) return;
    setBusy(true);
    setError(null);
    setErrors({});
    try {
      const body: InvitationInput = { role, team_ids: role !== 'billing_manager' && teamIds.length ? teamIds : undefined };
      if (isEmail) body.email = value;
      else {
        let user: SimpleUser;
        try {
          user = await getUser(value);
        } catch {
          setErrors({ invitee: `No user named “${value}”.` });
          return;
        }
        if (user.type === 'Organization') {
          setErrors({ invitee: `${value} is an organization.` });
          return;
        }
        body.invitee_id = user.id;
      }
      await createInvitation(org, body);
      invalidateLists(invitationsPrefix(org));
      toast({ kind: 'success', title: `Invited ${value}`, description: `${value} will get an email to join ${org}.` });
      onClose();
    } catch (err) {
      const fe = fieldErrors(err);
      const inviteeErr = fe.invitee_id ?? fe.invitee ?? fe.email;
      if (inviteeErr) setErrors({ invitee: inviteeErr });
      else setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  const selectedTeams = (teams.data ?? []).filter((t) => teamIds.includes(t.id));
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={`Invite a member to ${org}`}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!valid} onClick={() => void submit()}>
            Send invitation
          </Button>
        </>
      }
    >
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Field
          label="Username or email"
          htmlFor="inv-who"
          error={errors.invitee ?? (value && !valid ? (isEmail ? 'Enter a valid email address.' : 'Enter a valid username.') : null)}
          hint="Email invitations to an address of an existing account invite that account."
        >
          <Input id="inv-who" value={who} onChange={(e) => setWho(e.target.value)} autoFocus autoComplete="off" spellCheck={false} invalid={!!errors.invitee || (!!value && !valid)} />
        </Field>
        <RadioCards name="inv-role" label="Role" value={role} onChange={setRole} options={INVITE_ROLES} />
        <Field label="Teams (optional)" htmlFor="inv-teams" hint={role === 'billing_manager' ? 'Billing managers can’t be added to teams.' : undefined}>
          <div className={local.chips}>
            {selectedTeams.map((t) => (
              <Tag key={t.id}>{t.name}</Tag>
            ))}
            <Button
              id="inv-teams"
              ref={teamsRef}
              size="sm"
              disabled={role === 'billing_manager' || !teams.data}
              loading={teams.loading}
              onClick={() => setPicking(true)}
              aria-haspopup="dialog"
            >
              {selectedTeams.length ? 'Edit teams' : 'Add to teams'}
            </Button>
          </div>
        </Field>
        <SelectPanel
          open={picking}
          onClose={() => setPicking(false)}
          anchor={teamsRef}
          title="Add to teams"
          placeholder="Filter teams"
          items={(teams.data ?? []).map((t) => ({ id: t.id, text: t.name, description: t.parent ? `in ${t.parent.name}` : undefined, selected: teamIds.includes(t.id) }))}
          onToggle={(id) => setTeamIds((ids) => (ids.includes(Number(id)) ? ids.filter((x) => x !== Number(id)) : [...ids, Number(id)]))}
          emptyText="No teams"
        />
        {error && (
          <div className={styles.formError} role="alert">
            {error}
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}

// ------------------------------------------------------------------ teams

export const teamPath = (org: string, slug: string) => `/organizations/${encodeURIComponent(org)}/settings/teams/${encodeURIComponent(slug)}`;

export interface TreeRow {
  team: RestTeam;
  depth: number;
  /** "Parent / Child" path, shown when searching. */
  path: string;
}

/** Depth-first tree (parents before children, siblings by name). */
export function teamTree(teams: readonly RestTeam[]): TreeRow[] {
  const byParent = new Map<number | null, RestTeam[]>();
  const ids = new Set(teams.map((t) => t.id));
  for (const t of teams) {
    // A parent the viewer can't see (secret) → show at the root.
    const p = t.parent && ids.has(t.parent.id) ? t.parent.id : null;
    const list = byParent.get(p) ?? [];
    list.push(t);
    byParent.set(p, list);
  }
  const out: TreeRow[] = [];
  const walk = (parent: number | null, depth: number, prefix: string) => {
    const kids = (byParent.get(parent) ?? []).sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' }));
    for (const t of kids) {
      const path = prefix ? `${prefix} / ${t.name}` : t.name;
      out.push({ team: t, depth, path });
      if (depth < 32) walk(t.id, depth + 1, path);
    }
  };
  walk(null, 0, '');
  return out;
}

/** IDs of `id` and all its descendants (a team can't be nested under them). */
export function descendants(teams: readonly RestTeam[], id: number): Set<number> {
  const out = new Set([id]);
  let grew = true;
  while (grew) {
    grew = false;
    for (const t of teams) {
      if (t.parent && out.has(t.parent.id) && !out.has(t.id)) {
        out.add(t.id);
        grew = true;
      }
    }
  }
  return out;
}

export const privacyPill = (p: TeamPrivacy) =>
  p === 'secret' ? <StatusPill status="warning">Secret</StatusPill> : <StatusPill status="neutral">Visible</StatusPill>;

/** Create a team (name, description, privacy, parent). */
export function TeamDialog({ org, open, onClose, teams, parentId }: { org: string; open: boolean; onClose: () => void; teams: readonly RestTeam[]; parentId?: number }) {
  const [form, setForm] = useState({ name: '', description: '', privacy: 'closed' as TeamPrivacy, parent: '' });
  const [errors, setErrors] = useState<FieldErrors>({});
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [shownFor, setShownFor] = useState(false);
  if (open !== shownFor) {
    setShownFor(open);
    if (open) {
      setForm({ name: '', description: '', privacy: parentId ? 'closed' : 'closed', parent: parentId ? String(parentId) : '' });
      setErrors({});
      setError(null);
    }
  }
  const set = (p: Partial<typeof form>) => setForm((f) => ({ ...f, ...p }));
  const nested = !!form.parent;
  const parents = teamTree(teams.filter((t) => t.privacy !== 'secret'));
  const nameOk = form.name.trim().length > 0 && form.name.length <= 255;

  const submit = async () => {
    if (!nameOk || busy) return;
    setBusy(true);
    setErrors({});
    setError(null);
    try {
      const t = await createTeam(org, {
        name: form.name.trim(),
        description: form.description.trim() || undefined,
        privacy: nested ? 'closed' : form.privacy,
        parent_team_id: form.parent ? Number(form.parent) : undefined,
      });
      mutate<TeamFull>(teamKey(org, t.slug), () => t);
      mutate<RestTeam[]>(allTeamsKey(org), (prev) => [...(prev ?? []), t]);
      toast({ kind: 'success', title: `Created team ${t.name}` });
      onClose();
      navigate(teamPath(org, t.slug));
    } catch (err) {
      const fe = fieldErrors(err);
      if (fe.parent_team_id) fe.parent = fe.parent_team_id;
      setErrors(fe);
      if (!Object.keys(fe).length) setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Create new team"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!nameOk} onClick={() => void submit()}>
            Create team
          </Button>
        </>
      }
    >
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Field label="Team name" htmlFor="team-name" error={errors.name ?? null} hint="You’ll use this name to mention the team (@org/team-name).">
          <Input id="team-name" value={form.name} onChange={(e) => set({ name: e.target.value })} autoFocus autoComplete="off" invalid={!!errors.name} />
        </Field>
        <Field label="Description (optional)" htmlFor="team-desc" error={errors.description ?? null}>
          <Textarea id="team-desc" rows={2} value={form.description} onChange={(e) => set({ description: e.target.value })} placeholder="What is this team about?" />
        </Field>
        <Field label="Parent team" htmlFor="team-parent" error={errors.parent ?? null} hint="Secret teams can’t be parents.">
          <Select id="team-parent" value={form.parent} onChange={(e) => set({ parent: e.target.value, privacy: e.target.value ? 'closed' : form.privacy })}>
            <option value="">No parent team</option>
            {parents.map((r) => (
              <option key={r.team.id} value={r.team.id}>
                {'  '.repeat(r.depth)}
                {r.team.name}
              </option>
            ))}
          </Select>
        </Field>
        <TeamPrivacyChoice value={nested ? 'closed' : form.privacy} onChange={(v) => set({ privacy: v })} secretDisabled={nested} />
        {errors.privacy && <div className={styles.formError}>{errors.privacy}</div>}
        {error && (
          <div className={styles.formError} role="alert">
            {error}
          </div>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
}

export function TeamPrivacyChoice({ value, onChange, secretDisabled, secretReason = 'Nested teams can’t be secret.' }: { value: TeamPrivacy; onChange: (v: TeamPrivacy) => void; secretDisabled?: boolean; secretReason?: string }) {
  return (
    <div>
      <div className={local.sectionLabel}>Team visibility</div>
      <RadioCards
        name="team-privacy"
        label="Team visibility"
        value={value}
        onChange={(v) => !(v === 'secret' && secretDisabled) && onChange(v)}
        options={[
          { value: 'closed', label: 'Visible', description: 'Every member of the organization can see and mention this team.' },
          { value: 'secret', label: 'Secret', description: secretDisabled ? secretReason : 'Only owners and members of this team can see it.' },
        ]}
      />
    </div>
  );
}

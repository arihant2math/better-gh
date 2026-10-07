import { observer } from 'mobx-react-lite';
import { useEffect, useId, useMemo, useState, type KeyboardEvent } from 'react';
import {
  deleteInvitation,
  getUser,
  listCollaborators,
  listInvitations,
  listRepoTeams,
  putCollaborator,
  putTeamRepo,
  removeCollaborator,
  removeTeamRepo,
  roleName,
  teamsWithAccess,
  updateInvitation,
  type Collaborator,
  type RepoInvitation,
} from '../../../api/repoSettings';
import type { RestUser } from '../../../api/types';
import { Banner, ConfirmDialog, ItemList, ItemRow, PageHeader, Pill, RadioCards, Section, errorMessage, useDebounced } from '../../../components/settings/kit';
import { store } from '../../../sync';
import type { Permission, Repo, Team } from '../../../sync/models';
import { Avatar } from '../../../ui/Badge';
import { Button } from '../../../ui/Button';
import { Dialog } from '../../../ui/Dialog';
import { GlobeIcon, LockIcon, OrganizationIcon, PeopleIcon, PersonAddIcon, PersonIcon, SearchIcon } from '../../../ui/icons';
import { Field, Input, Select } from '../../../ui/Input';
import { toast } from '../../../ui/Toast';
import { ROLES, roleLabel } from '../model';
import styles from '../RepoSettings.module.css';
import { ListSkeleton, LoadError, repoKey, useLocalResource, type SectionProps } from '../shared';

export default observer(function AccessSettings({ repo }: SectionProps) {
  const isOrg = !!store().get('org', repo.ownerId);
  const collabs = useLocalResource(repoKey(repo, 'collaborators'), () => listCollaborators(repo.owner, repo.name));
  const invites = useLocalResource(repoKey(repo, 'invitations'), () => listInvitations(repo.owner, repo.name));
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<{ kind: 'user'; login: string } | { kind: 'invite'; inv: RepoInvitation } | null>(null);
  const directCount = (collabs.data?.filter((c) => c.login.toLowerCase() !== repo.owner.toLowerCase()).length ?? 0) + (invites.data?.length ?? 0);

  const refreshCollaborators = () =>
    listCollaborators(repo.owner, repo.name).then(
      (list) => collabs.update(() => list),
      () => undefined,
    );

  const changeRole = (c: Collaborator, role: Permission) => {
    const prev = c.role_name;
    collabs.update((l) => l.map((x) => (x.login === c.login ? { ...x, role_name: role } : x)));
    putCollaborator(repo.owner, repo.name, c.login, role).then(
      () => toast({ kind: 'success', title: `${c.login} is now ${roleLabel(role).toLowerCase()}` }),
      (e: unknown) => {
        collabs.update((l) => l.map((x) => (x.login === c.login ? { ...x, role_name: prev } : x)));
        toast({ kind: 'error', title: `Couldn't change ${c.login}'s role`, description: errorMessage(e) });
      },
    );
  };

  const changeInviteRole = (inv: RepoInvitation, role: Permission) => {
    const prev = inv.permissions;
    invites.update((l) => l.map((x) => (x.id === inv.id ? { ...x, permissions: role } : x)));
    updateInvitation(repo.owner, repo.name, inv.id, role).catch((e: unknown) => {
      invites.update((l) => l.map((x) => (x.id === inv.id ? { ...x, permissions: prev } : x)));
      toast({ kind: 'error', title: "Couldn't update the invitation", description: errorMessage(e) });
    });
  };

  return (
    <>
      <PageHeader title="Collaborators and teams" description="Manage who can see and change this repository." />
      <div className={styles.box} style={{ marginBottom: 24 }}>
        <div className={styles.boxRow}>
          {repo.visibility === 'internal' ? <OrganizationIcon size={16} /> : repo.private ? <LockIcon size={16} /> : <GlobeIcon size={16} />}
          <span className={styles.boxText}>
            <span className={styles.boxTitle}>
              {repo.visibility === 'internal' ? 'Internal repository' : repo.private ? 'Private repository' : 'Public repository'}
            </span>
            <span className={styles.small}>
              {repo.visibility === 'internal'
                ? 'Everyone signed in to this site can view it; only those with access can change it.'
                : repo.private
                  ? 'Only those with access to this repository can view it.'
                  : 'This repository is public and visible to anyone.'}
            </span>
          </span>
        </div>
        <div className={styles.boxRow}>
          <PersonIcon size={16} />
          <span className={styles.boxText}>
            <span className={styles.boxTitle}>Direct access</span>
            <span className={styles.small}>
              {collabs.data ? `${directCount} ${directCount === 1 ? 'person has' : 'people have'} direct access to this repository.` : 'Loading…'}
            </span>
          </span>
        </div>
      </div>

      <Section
        title="Manage access"
        actions={
          <Button variant="primary" size="sm" leadingIcon={PersonAddIcon} onClick={() => setAdding(true)} disabled={repo.archived}>
            Add people
          </Button>
        }
      >
        {collabs.error ? <LoadError error={collabs.error} /> : null}
        {!collabs.data ? (
          <ListSkeleton />
        ) : (
          <ItemList aria-label="Collaborators" empty="You haven't invited any collaborators yet.">
            {invites.data?.map((inv) => (
              <ItemRow
                key={`inv-${inv.id}`}
                leading={<Avatar user={inv.invitee ? { login: inv.invitee.login, avatarUrl: inv.invitee.avatar_url } : null} size={32} />}
                title={inv.invitee?.login ?? 'Unknown user'}
                meta={
                  <>
                    <Pill tone="warning">Pending invite</Pill> {inv.inviter ? `Invited by ${inv.inviter.login}` : null}
                  </>
                }
                actions={
                  <>
                    <RoleSelect value={inv.permissions} onChange={(r) => changeInviteRole(inv, r)} label={`Role for ${inv.invitee?.login ?? 'invitee'}`} />
                    <Button size="sm" variant="ghost" onClick={() => setRemoving({ kind: 'invite', inv })}>
                      Cancel invite
                    </Button>
                  </>
                }
              />
            ))}
            {collabs.data.map((c) => {
              const isOwner = c.login.toLowerCase() === repo.owner.toLowerCase();
              return (
                <ItemRow
                  key={c.login}
                  leading={<Avatar user={{ login: c.login, avatarUrl: c.avatar_url, name: c.name }} size={32} />}
                  title={
                    <span className={styles.row}>
                      {c.login}
                      {c.name && <span className={styles.small}>{c.name}</span>}
                    </span>
                  }
                  meta={isOwner ? 'Owner' : `${roleLabel(c.role_name)} access`}
                  actions={
                    isOwner ? (
                      <Pill tone="accent">Owner</Pill>
                    ) : (
                      <>
                        <RoleSelect value={c.role_name} onChange={(r) => changeRole(c, r)} label={`Role for ${c.login}`} />
                        <Button size="sm" variant="ghost" onClick={() => setRemoving({ kind: 'user', login: c.login })}>
                          Remove
                        </Button>
                      </>
                    )
                  }
                />
              );
            })}
          </ItemList>
        )}
      </Section>

      {isOrg && <TeamsSection repo={repo} />}

      <AddPersonDialog
        open={adding}
        onClose={() => setAdding(false)}
        repo={repo}
        exclude={[repo.owner, ...(collabs.data ?? []).map((c) => c.login), ...(invites.data ?? []).map((i) => i.invitee?.login ?? '')]}
        onAdded={(inv) => {
          if (inv) invites.update((l) => [...l.filter((x) => x.id !== inv.id), inv]);
          else void refreshCollaborators();
        }}
      />

      <ConfirmDialog
        open={!!removing}
        onClose={() => setRemoving(null)}
        title={removing?.kind === 'invite' ? 'Cancel invitation' : `Remove ${removing?.login ?? ''}`}
        confirmLabel={removing?.kind === 'invite' ? 'Cancel invitation' : 'Remove from this repository'}
        onConfirm={async () => {
          if (!removing) return;
          if (removing.kind === 'invite') {
            await deleteInvitation(repo.owner, repo.name, removing.inv.id);
            invites.update((l) => l.filter((x) => x.id !== removing.inv.id));
            toast({ kind: 'success', title: 'Invitation cancelled' });
          } else {
            await removeCollaborator(repo.owner, repo.name, removing.login);
            collabs.update((l) => l.filter((x) => x.login !== removing.login));
            toast({ kind: 'success', title: `Removed ${removing.login} from ${repo.name}` });
          }
        }}
      >
        <p className={styles.muted}>
          {removing?.kind === 'invite'
            ? `${removing.inv.invitee?.login ?? 'This user'} will no longer be able to accept the invitation to ${repo.owner}/${repo.name}.`
            : `${removing?.login ?? ''} will lose direct access to ${repo.owner}/${repo.name}. Access through teams or organization membership is not affected.`}
        </p>
      </ConfirmDialog>
    </>
  );
});

function RoleSelect({ value, onChange, label, disabled }: { value: string; onChange: (r: Permission) => void; label: string; disabled?: boolean }) {
  return (
    <Select className={styles.roleSelect} aria-label={label} value={roleName(value)} disabled={disabled} onChange={(e) => onChange(e.target.value as Permission)}>
      {ROLES.map((r) => (
        <option key={r.value} value={r.value}>
          {r.label}
        </option>
      ))}
    </Select>
  );
}

// ------------------------------------------------------------------ add people

interface Candidate {
  login: string;
  name?: string | null;
  avatarUrl: string;
}

const AddPersonDialog = observer(function AddPersonDialog({
  open,
  onClose,
  repo,
  exclude,
  onAdded,
}: {
  open: boolean;
  onClose: () => void;
  repo: Repo;
  exclude: string[];
  onAdded: (inv: RepoInvitation | null) => void;
}) {
  const inputId = useId();
  const listId = useId();
  const [query, setQuery] = useState('');
  const [picked, setPicked] = useState<Candidate | null>(null);
  const [role, setRole] = useState<Permission>('write');
  const [active, setActive] = useState(0);
  const [remote, setRemote] = useState<{ q: string; user: RestUser | null } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const q = query.trim().replace(/^@/, '');
  const debounced = useDebounced(q, 250);
  const excluded = useMemo(() => new Set(exclude.map((e) => e.toLowerCase())), [exclude]);

  useEffect(() => {
    if (open) {
      setQuery('');
      setPicked(null);
      setRole('write');
      setError(null);
      setActive(0);
    }
  }, [open]);

  const local: Candidate[] = useMemo(() => {
    if (!q) return [];
    const l = q.toLowerCase();
    return store()
      .all('user')
      .filter((u) => u.type === 'User' && (u.login.toLowerCase().includes(l) || (u.name ?? '').toLowerCase().includes(l)))
      .sort((a, b) => Number(b.login.toLowerCase().startsWith(l)) - Number(a.login.toLowerCase().startsWith(l)) || a.login.localeCompare(b.login))
      .slice(0, 8)
      .map((u) => ({ login: u.login, name: u.name, avatarUrl: u.avatarUrl }));
  }, [q]);

  // Exact lookup for people not in the local store.
  const exactLocal = local.some((c) => c.login.toLowerCase() === debounced.toLowerCase());
  useEffect(() => {
    if (!debounced || exactLocal || !/^[A-Za-z0-9-]+$/.test(debounced)) return;
    let cancelled = false;
    getUser(debounced).then(
      (u) => !cancelled && setRemote({ q: debounced, user: u.type === 'Organization' ? null : u }),
      () => !cancelled && setRemote({ q: debounced, user: null }),
    );
    return () => {
      cancelled = true;
    };
  }, [debounced, exactLocal]);

  const remoteUser = remote?.q === q && remote.user ? { login: remote.user.login, name: remote.user.name, avatarUrl: remote.user.avatar_url } : null;
  const candidates = [...local, ...(remoteUser && !local.some((c) => c.login === remoteUser.login) ? [remoteUser] : [])];
  const noMatch = !!q && remote?.q === q && !remote.user && !local.length;

  const pick = (c: Candidate) => {
    if (excluded.has(c.login.toLowerCase())) {
      setError(`${c.login} already has access to this repository (or a pending invitation).`);
      return;
    }
    setError(null);
    setPicked(c);
  };

  const submit = async () => {
    if (!picked || busy) return;
    setBusy(true);
    setError(null);
    try {
      const inv = await putCollaborator(repo.owner, repo.name, picked.login, role);
      onAdded(inv);
      toast({ kind: 'success', title: inv ? `Invited ${picked.login} to ${repo.name}` : `Added ${picked.login} to ${repo.name}` });
      onClose();
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (!candidates.length) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActive((a) => Math.min(candidates.length - 1, a + 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActive((a) => Math.max(0, a - 1));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const c = candidates[Math.min(active, candidates.length - 1)];
      if (c) pick(c);
    }
  };

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={`Add people to ${repo.name}`}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!picked} loading={busy} onClick={() => void submit()}>
            {picked ? `Add ${picked.login} to this repository` : 'Select a person'}
          </Button>
        </>
      }
    >
      <form
        className={styles.group}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        {!picked ? (
          <>
            <Field label="Search by username or full name" htmlFor={inputId} error={error}>
              <Input
                id={inputId}
                leadingIcon={SearchIcon}
                value={query}
                autoFocus
                autoComplete="off"
                spellCheck={false}
                role="combobox"
                aria-expanded={candidates.length > 0}
                aria-controls={listId}
                aria-activedescendant={candidates.length ? `${listId}-${Math.min(active, candidates.length - 1)}` : undefined}
                onChange={(e) => {
                  setQuery(e.target.value);
                  setActive(0);
                  setError(null);
                }}
                onKeyDown={onKeyDown}
              />
            </Field>
            {candidates.length > 0 && (
              <ul className={styles.suggestions} role="listbox" id={listId} aria-label="Matching people">
                {candidates.map((c, i) => (
                  <li key={c.login} id={`${listId}-${i}`} role="option" aria-selected={i === Math.min(active, candidates.length - 1)}>
                    <button
                      type="button"
                      tabIndex={-1}
                      className={styles.suggestion}
                      aria-selected={i === Math.min(active, candidates.length - 1)}
                      onMouseEnter={() => setActive(i)}
                      onClick={() => pick(c)}
                    >
                      <Avatar user={c} size={20} />
                      <strong>{c.login}</strong>
                      {c.name && <span className={styles.small}>{c.name}</span>}
                      {excluded.has(c.login.toLowerCase()) && <span className={styles.small}>· already has access</span>}
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {noMatch && <p className={styles.small}>No user named “{q}”.</p>}
          </>
        ) : (
          <>
            <div className={styles.box}>
              <div className={styles.boxRow}>
                <Avatar user={picked} size={32} />
                <span className={styles.boxText}>
                  <span className={styles.boxTitle}>{picked.login}</span>
                  {picked.name && <span className={styles.small}>{picked.name}</span>}
                </span>
                <Button size="sm" variant="ghost" onClick={() => setPicked(null)}>
                  Change
                </Button>
              </div>
            </div>
            <RadioCards aria-label="Choose a role" value={role} onChange={setRole} options={ROLES.map((r) => ({ value: r.value, label: r.label, description: r.description }))} />
            {error && <Banner tone="danger">{error}</Banner>}
          </>
        )}
      </form>
    </Dialog>
  );
});

// ------------------------------------------------------------------ teams

const TeamsSection = observer(function TeamsSection({ repo }: { repo: Repo }) {
  const org = store().get('org', repo.ownerId)!;
  const teams = teamsWithAccess(repo.id, repo.ownerId);
  const grants = useLocalResource(repoKey(repo, 'teams'), () => listRepoTeams(repo.owner, repo.name));
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<Team | null>(null);
  const roleOf = (t: Team): Permission => roleName(grants.data?.find((g) => g.id === t.id || g.slug === t.slug)?.permission);
  const setGrant = (t: Team, role: Permission) =>
    grants.update((l) => [...l.filter((g) => g.id !== t.id), { id: t.id, slug: t.slug, name: t.name, description: t.description, permission: role }]);

  const changeRole = (t: Team, role: Permission) => {
    const prev = roleOf(t);
    setGrant(t, role);
    putTeamRepo(t, org.login, repo, role).catch(() => setGrant(t, prev));
  };

  return (
    <Section
      title="Teams"
      description={`Teams in ${org.login} with access to this repository.`}
      actions={
        <Button size="sm" leadingIcon={PeopleIcon} onClick={() => setAdding(true)} disabled={repo.archived}>
          Add teams
        </Button>
      }
    >
      <ItemList aria-label="Teams" empty="No teams have access to this repository yet.">
        {teams.map((t) => (
          <ItemRow
            key={t.id}
            icon={PeopleIcon}
            title={t.name}
            meta={`@${org.login}/${t.slug} · ${t.memberIds.length} member${t.memberIds.length === 1 ? '' : 's'}`}
            actions={
              <>
                <RoleSelect value={roleOf(t)} onChange={(r) => changeRole(t, r)} label={`Role for team ${t.name}`} disabled={!grants.data} />
                <Button size="sm" variant="ghost" onClick={() => setRemoving(t)}>
                  Remove
                </Button>
              </>
            }
          />
        ))}
      </ItemList>
      <AddTeamDialog
        open={adding}
        onClose={() => setAdding(false)}
        repo={repo}
        orgLogin={org.login}
        onAdded={(t, role) => setGrant(t, role)}
      />
      <ConfirmDialog
        open={!!removing}
        onClose={() => setRemoving(null)}
        title={`Remove ${removing?.name ?? ''}`}
        confirmLabel="Remove team"
        onConfirm={() => {
          if (removing) removeTeamRepo(removing, org.login, repo).catch(() => undefined);
        }}
      >
        <p className={styles.muted}>Members of {removing?.name} will lose the access granted through this team.</p>
      </ConfirmDialog>
    </Section>
  );
});

const AddTeamDialog = observer(function AddTeamDialog({
  open,
  onClose,
  repo,
  orgLogin,
  onAdded,
}: {
  open: boolean;
  onClose: () => void;
  repo: Repo;
  orgLogin: string;
  onAdded: (t: Team, role: Permission) => void;
}) {
  const id = useId();
  const available = store()
    .byIndex('team', 'orgId', repo.ownerId)
    .filter((t) => !t.repoIds.includes(repo.id))
    .sort((a, b) => a.name.localeCompare(b.name));
  const [teamId, setTeamId] = useState<string>('');
  const [role, setRole] = useState<Permission>('read');
  useEffect(() => {
    if (open) {
      setTeamId('');
      setRole('read');
    }
  }, [open]);
  const team = available.find((t) => String(t.id) === teamId);
  const submit = () => {
    if (!team) return;
    onAdded(team, role);
    putTeamRepo(team, orgLogin, repo, role).then(
      () => toast({ kind: 'success', title: `${team.name} now has ${roleLabel(role).toLowerCase()} access` }),
      () => undefined,
    );
    onClose();
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={`Add teams to ${repo.name}`}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" disabled={!team} onClick={submit}>
            {team ? `Add ${team.name} to this repository` : 'Select a team'}
          </Button>
        </>
      }
    >
      <form
        className={styles.group}
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        {available.length === 0 ? (
          <p className={styles.muted}>Every team in {orgLogin} already has access to this repository.</p>
        ) : (
          <>
            <Field label="Team" htmlFor={id}>
              <Select id={id} value={teamId} autoFocus onChange={(e) => setTeamId(e.target.value)}>
                <option value="">Choose a team…</option>
                {available.map((t) => (
                  <option key={t.id} value={t.id}>
                    {t.name} (@{orgLogin}/{t.slug})
                  </option>
                ))}
              </Select>
            </Field>
            <RadioCards aria-label="Choose a role" value={role} onChange={setRole} options={ROLES.map((r) => ({ value: r.value, label: r.label, description: r.description }))} />
          </>
        )}
      </form>
    </Dialog>
  );
});

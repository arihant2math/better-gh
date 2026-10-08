import { observer } from 'mobx-react-lite';
import { useId, useRef, useState } from 'react';
import { invalidate } from '../../api/cache';
import { validationErrors } from '../../api/errors';
import { createFork } from '../../api/endpoints';
import { session } from '../../app/session';
import { navigate } from '../../router';
import { store } from '../../sync';
import type { Repo } from '../../sync/models';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { ChevronDownIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { Menu } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import { normalizeRepoName, repoNameError } from '../new/names';
import styles from './RepoNav.module.css';

interface OwnerOption {
  login: string;
  avatarUrl: string;
  isOrg: boolean;
}

/**
 * Accounts the viewer can fork into: themself plus organizations where they
 * are an owner (members' create rights are checked by the server and shown
 * as an error). The repository's own owner is excluded.
 */
function useForkOwners(repo: Repo): OwnerOption[] {
  const me = session.user;
  if (!me) return [];
  const s = store();
  const orgs = s
    .byIndex('membership', 'userId', me.id)
    .map((m) => s.get('org', m.orgId))
    .filter((o): o is NonNullable<typeof o> => !!o)
    .map((o) => ({ login: o.login, avatarUrl: o.avatarUrl, isOrg: true }))
    .sort((a, b) => a.login.localeCompare(b.login));
  return [{ login: me.login, avatarUrl: s.get('user', me.id)?.avatarUrl ?? me.avatarUrl ?? '', isOrg: false }, ...orgs].filter(
    (o) => o.login.toLowerCase() !== repo.owner.toLowerCase(),
  );
}

/** "Create a new fork": owner, name, description, default branch only → `POST /forks`. */
export default observer(function ForkDialog({ repo, onClose }: { repo: Repo; onClose: () => void }) {
  const owners = useForkOwners(repo);
  const [ownerLogin, setOwnerLogin] = useState(owners[0]?.login ?? '');
  const owner = owners.find((o) => o.login === ownerLogin) ?? owners[0];
  const [rawName, setRawName] = useState(repo.name);
  const [description, setDescription] = useState(repo.description ?? '');
  const [defaultOnly, setDefaultOnly] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState(false);
  const ownerBtn = useRef<HTMLButtonElement>(null);
  const ids = { name: useId(), desc: useId(), owner: useId() };
  const name = normalizeRepoName(rawName);
  const nameErr = rawName.trim() ? repoNameError(name) : 'Repository name is required';

  const submit = async () => {
    if (!owner || nameErr || busy) return;
    setBusy(true);
    setError(null);
    try {
      const fork = await createFork(repo.owner, repo.name, {
        organization: owner.isOrg ? owner.login : undefined,
        name,
        description: description.trim() || undefined,
        default_branch_only: defaultOnly,
      });
      invalidate(`forks:${repo.owner}/${repo.name}`);
      toast({ kind: 'success', title: `Forked ${repo.owner}/${repo.name}`, description: fork.full_name });
      onClose();
      navigate(`/${fork.full_name}`);
    } catch (e) {
      setBusy(false);
      setError(validationErrors(e)[0]?.message ?? (e instanceof Error ? e.message : 'Could not create the fork.'));
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title="Create a new fork"
      className={styles.forkDialog}
      footer={
        <>
          {busy && <span className={styles.muted}>Forking {repo.owner}/{repo.name}…</span>}
          <span style={{ flex: 1 }} />
          <Button onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" loading={busy} disabled={!owner || !!nameErr} onClick={() => void submit()}>
            {busy ? 'Forking…' : 'Create fork'}
          </Button>
        </>
      }
    >
      <form
        className={styles.forkForm}
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <p className={styles.muted}>A fork is a copy of a repository. Forking lets you freely experiment with changes without affecting the original project.</p>
        {owners.length === 0 ? (
          <p className={styles.error} role="alert">
            You have no account to fork this repository into.
          </p>
        ) : (
          <div className={styles.ownerName}>
            <div className={styles.ownerCol}>
              <span className={styles.label} id={ids.owner}>
                Owner
              </span>
              <Button ref={ownerBtn} trailingIcon={ChevronDownIcon} onClick={() => setMenu((v) => !v)} aria-haspopup="menu" aria-expanded={menu} aria-labelledby={ids.owner} disabled={busy}>
                <Avatar user={{ login: owner!.login, avatarUrl: owner!.avatarUrl }} size={20} square={owner!.isOrg} /> {owner!.login}
              </Button>
              <Menu
                open={menu}
                onClose={() => setMenu(false)}
                anchor={ownerBtn}
                aria-label="Choose an owner"
                items={owners.map((o) => ({
                  id: o.login,
                  label: o.login,
                  leading: <Avatar user={{ login: o.login, avatarUrl: o.avatarUrl }} size={20} square={o.isOrg} />,
                  onSelect: () => setOwnerLogin(o.login),
                }))}
              />
            </div>
            <span className={styles.slash}>/</span>
            <div className={styles.nameCol}>
              <Field label="Repository name" htmlFor={ids.name} error={rawName && nameErr ? nameErr : undefined}>
                <Input id={ids.name} value={rawName} onChange={(e) => setRawName(e.target.value)} disabled={busy} data-autofocus autoComplete="off" />
              </Field>
            </div>
          </div>
        )}
        {name !== rawName.trim() && !nameErr && (
          <p className={styles.muted}>
            Your new repository will be created as <strong>{name}</strong>.
          </p>
        )}
        <Field label="Description (optional)" htmlFor={ids.desc}>
          <Input id={ids.desc} value={description} onChange={(e) => setDescription(e.target.value)} disabled={busy} />
        </Field>
        <label className={styles.check}>
          <input type="checkbox" checked={defaultOnly} onChange={(e) => setDefaultOnly(e.target.checked)} disabled={busy} />
          <span>
            Copy the <code>{repo.defaultBranch}</code> branch only
            <span className={styles.muted}> — contribute back to {repo.owner}/{repo.name} by adding your own branch.</span>
          </span>
        </label>
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
});

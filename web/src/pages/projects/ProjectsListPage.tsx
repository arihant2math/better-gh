import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { useResource } from '../../api/cache';
import { listOwnerProjects, ownerProjectsKey } from '../../api/projects';
import { NotFound } from '../../app/NotFound';
import { Link, setQuery, useLocation, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { projectsForOwner } from '../../sync/projects';
import { orgByLogin, userByLogin } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { CheckIcon, PlusIcon, SearchIcon, TableIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { Spinner } from '../../ui/Spinner';
import { canCreateFor, mergeProjects, NewProjectDialog, ProjectListBody } from './ProjectList';
import styles from './Projects.module.css';

/** `/orgs/:owner/projects` and `/users/:owner/projects`. */
export default observer(function ProjectsListPage() {
  const { owner } = useParams<{ owner: string }>();
  const { pathname } = useLocation();
  const ownerKind = pathname.toLowerCase().startsWith('/users/') ? 'users' : 'orgs';
  const query = useQuery();
  const state = query.get('state') === 'closed' ? 'closed' : 'open';
  const q = query.get('q') ?? '';
  const [draft, setDraft] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const ownerRow = orgByLogin(owner) ?? userByLogin(owner);
  const remote = useResource(ownerProjectsKey(owner), () => listOwnerProjects(owner, 'all'), { ttlMs: 15_000 });
  const ownerId = ownerRow?.id ?? remote.data?.projects[0]?.ownerId;

  useShortcuts('Projects', {
    c: { handler: () => (ownerId != null && canCreateFor(ownerId) ? setCreating(true) : false), description: 'New project', group: 'Projects' },
  });

  if (!ownerRow && !remote.data) {
    return remote.loading ? (
      <div className={styles.loading}>
        <Spinner />
      </div>
    ) : (
      <NotFound what="account" />
    );
  }
  const all = mergeProjects(ownerId != null ? projectsForOwner(ownerId) : [], remote.data?.projects);
  const words = q.toLowerCase().split(/\s+/).filter(Boolean);
  const matches = all.filter((p) => words.every((w) => `${p.title} ${p.shortDescription ?? ''}`.toLowerCase().includes(w)));
  const open = matches.filter((p) => !p.closed);
  const closed = matches.filter((p) => p.closed);
  const shown = state === 'closed' ? closed : open;
  const login = ownerRow?.login ?? owner;

  return (
    <div className={styles.listPage}>
      <header className={styles.listHeader}>
        {ownerRow && <Avatar user={ownerRow} size={32} square={ownerKind === 'orgs'} />}
        <div>
          <h1 className={styles.listH1}>
            <Link to={`/${login}`}>{login}</Link> · Projects
          </h1>
        </div>
        {ownerId != null && canCreateFor(ownerId) && (
          <Button variant="primary" leadingIcon={PlusIcon} kbd="C" onClick={() => setCreating(true)} className={styles.pushRight}>
            New project
          </Button>
        )}
      </header>
      <div className={styles.listToolbar}>
        <Input
          className={styles.filter}
          leadingIcon={SearchIcon}
          value={draft ?? q}
          placeholder="Search all projects"
          aria-label="Search projects"
          onChange={(e) => {
            setDraft(e.target.value);
            setQuery({ q: e.target.value || null });
          }}
          onBlur={() => setDraft(null)}
        />
      </div>
      <div className={styles.stateTabs} role="tablist">
        <button type="button" role="tab" aria-selected={state === 'open'} className={styles.stateTab} onClick={() => setQuery({ state: null })}>
          <TableIcon size={16} /> {open.length} Open
        </button>
        <button type="button" role="tab" aria-selected={state === 'closed'} className={styles.stateTab} onClick={() => setQuery({ state: 'closed' })}>
          <CheckIcon size={16} /> {closed.length} Closed
        </button>
      </div>
      <ProjectListBody
        projects={shown}
        hrefOf={(p) => `/${ownerKind}/${login}/projects/${p.number}`}
        empty={q ? 'No projects match your search' : state === 'closed' ? 'No closed projects' : 'No open projects'}
      />
      {ownerId != null && <NewProjectDialog open={creating} onClose={() => setCreating(false)} owner={{ id: ownerId, login }} ownerKind={ownerKind} />}
    </div>
  );
});

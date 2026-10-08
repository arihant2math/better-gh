import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { invalidate, useResource } from '../../api/cache';
import { listRepoProjects, repoProjectsKey } from '../../api/projects';
import { useParams } from '../../router';
import { store } from '../../sync';
import { projectsForOwner, projectsLinkedToRepo, setRepoLinked } from '../../sync/projects';
import { canWrite } from '../../sync/selectors';
import { Button } from '../../ui/Button';
import { LinkIcon, PlusIcon } from '../../ui/icons';
import { SelectPanel } from '../../ui/Menu';
import { canCreateFor, mergeProjects, NewProjectDialog, ProjectListBody, projectHref } from './ProjectList';
import styles from './Projects.module.css';
import { useRouteRepo } from '../repo/useRouteRepo';

/** Repo "Projects" tab: projects linked to the repo (or containing its issues). */
export default observer(function RepoProjectsPage() {
  const { owner, repo: name } = useParams<{ owner: string; repo: string }>();
  const repo = useRouteRepo();
  const remote = useResource(repoProjectsKey(owner, name), () => listRepoProjects(owner, name), { ttlMs: 15_000 });
  const [linking, setLinking] = useState(false);
  const [creating, setCreating] = useState(false);
  const linkBtn = useRef<HTMLButtonElement>(null);
  const owners = new Map((remote.data?.owners ?? []).map((o) => [o.id, o]));
  const projects = mergeProjects(projectsLinkedToRepo(repo.id), remote.data?.projects);
  const candidates = projectsForOwner(repo.ownerId).filter((p) => p.id > 0);
  const ownerKind = store().get('org', repo.ownerId) ? 'orgs' : 'users';
  const mayLink = canWrite(repo.id);

  return (
    <div className={styles.repoProjects}>
      <div className={styles.listToolbar}>
        <h2 className={styles.listH2}>Projects</h2>
        <div className={styles.pushRight}>
          {mayLink && candidates.length > 0 && (
            <>
              <Button ref={linkBtn} leadingIcon={LinkIcon} onClick={() => setLinking(true)}>
                Link a project
              </Button>
              <SelectPanel
                open={linking}
                onClose={() => setLinking(false)}
                anchor={linkBtn}
                placement="bottom-end"
                title={`Link projects of ${repo.owner}`}
                items={candidates.map((p) => ({ id: p.id, text: p.title, description: `#${p.number}`, selected: p.linkedRepoIds.includes(repo.id) }))}
                onToggle={(id) => {
                  const p = candidates.find((x) => x.id === Number(id));
                  if (!p) return;
                  setRepoLinked(p, repo.id, !p.linkedRepoIds.includes(repo.id)).done.then(
                    () => invalidate(repoProjectsKey(owner, name)),
                    () => undefined,
                  );
                }}
              />
            </>
          )}
          {canCreateFor(repo.ownerId) && (
            <Button variant="primary" leadingIcon={PlusIcon} onClick={() => setCreating(true)}>
              New project
            </Button>
          )}
        </div>
      </div>
      <ProjectListBody
        projects={projects}
        hrefOf={(p) => projectHref(p, owners.get(p.ownerId)?.login)}
        ownerOf={(p) => (p.ownerId !== repo.ownerId ? (owners.get(p.ownerId)?.login ?? store().get('user', p.ownerId)?.login) : undefined)}
        empty={remote.loading && !projects.length ? 'Loading projects…' : 'No projects linked to this repository'}
      />
      <NewProjectDialog open={creating} onClose={() => setCreating(false)} owner={{ id: repo.ownerId, login: repo.owner }} ownerKind={ownerKind} />
    </div>
  );
});

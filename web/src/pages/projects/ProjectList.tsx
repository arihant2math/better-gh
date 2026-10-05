import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { session } from '../../app/session';
import { Link, navigate } from '../../router';
import { store } from '../../sync';
import type { ID, Project } from '../../sync/models';
import { createProject } from '../../sync/projects';
import { Tag } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState } from '../../ui/EmptyState';
import { TableIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './Projects.module.css';

/** Where a project lives in the URL space. */
export function projectHref(p: Pick<Project, 'ownerId' | 'number'>, ownerLogin?: string): string {
  const org = store().get('org', p.ownerId);
  if (org) return `/orgs/${org.login}/projects/${p.number}`;
  const login = ownerLogin ?? store().get('user', p.ownerId)?.login;
  return login ? `/users/${login}/projects/${p.number}` : '#';
}

export const ProjectRow = observer(function ProjectRow({ project, href, showOwner }: { project: Project; href: string; showOwner?: string }) {
  const pending = project.id < 0;
  return (
    <li className={styles.listRow} data-pending={pending || undefined}>
      <TableIcon size={16} className={project.closed ? styles.closedIcon : styles.openIcon} />
      <div className={styles.listMain}>
        <div className={styles.listTitle}>
          {pending ? <span>{project.title}</span> : <Link to={href}>{project.title}</Link>}
          {!project.public && <Tag>Private</Tag>}
          {project.closed && <Tag>Closed</Tag>}
        </div>
        <div className={styles.listMeta}>
          {showOwner && <span>{showOwner} · </span>}#{project.number} · updated <RelativeTime date={project.updatedAt} />
          {project.shortDescription && <span className={styles.listDesc}> — {project.shortDescription}</span>}
        </div>
      </div>
    </li>
  );
});

export function ProjectListBody({
  projects,
  hrefOf,
  empty,
  ownerOf,
}: {
  projects: Project[];
  hrefOf: (p: Project) => string;
  empty: string;
  ownerOf?: (p: Project) => string | undefined;
}) {
  if (!projects.length) return <EmptyState icon={TableIcon} title={empty} />;
  return (
    <ul className={styles.list} aria-label="Projects">
      {projects.map((p) => (
        <ProjectRow key={p.id} project={p} href={hrefOf(p)} showOwner={ownerOf?.(p)} />
      ))}
    </ul>
  );
}

/** Merge store rows (authoritative) with API rows. */
export function mergeProjects(local: readonly Project[], remote: readonly Project[] | undefined): Project[] {
  const byId = new Map<ID, Project>();
  for (const p of remote ?? []) byId.set(p.id, p);
  for (const p of local) byId.set(p.id, p);
  return [...byId.values()].sort((a, b) => Number(a.closed) - Number(b.closed) || (a.updatedAt < b.updatedAt ? 1 : -1));
}

export const NewProjectDialog = observer(function NewProjectDialog({
  open,
  onClose,
  owner,
  ownerKind,
}: {
  open: boolean;
  onClose: () => void;
  owner: { id: ID; login: string };
  ownerKind: 'orgs' | 'users';
}) {
  const [title, setTitle] = useState('');
  const [desc, setDesc] = useState('');
  const [pub, setPub] = useState(false);
  const [busy, setBusy] = useState(false);
  const submit = () => {
    if (!title.trim()) return;
    setBusy(true);
    const { done } = createProject(owner, { title: title.trim(), shortDescription: desc.trim() || undefined, public: pub });
    done.then(
      (r) => {
        setBusy(false);
        onClose();
        setTitle('');
        setDesc('');
        const n = (r.data as { number?: number } | null)?.number;
        if (n) navigate(`/${ownerKind}/${owner.login}/projects/${n}`);
      },
      () => setBusy(false),
    );
  };
  return (
    <Dialog
      open={open}
      onClose={onClose}
      title="Create project"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!title.trim()} onClick={submit}>
            Create project
          </Button>
        </>
      }
    >
      <form
        className={styles.form}
        onSubmit={(e) => {
          e.preventDefault();
          submit();
        }}
      >
        <Field label="Project name" htmlFor="np-title">
          <Input id="np-title" autoFocus value={title} onChange={(e) => setTitle(e.target.value)} placeholder="e.g. Q4 Roadmap" />
        </Field>
        <Field label="Short description (optional)" htmlFor="np-desc">
          <Input id="np-desc" value={desc} onChange={(e) => setDesc(e.target.value)} />
        </Field>
        <label className={styles.check}>
          <input type="checkbox" checked={pub} onChange={(e) => setPub(e.target.checked)} /> Public — anyone can see this project
        </label>
        <p className={styles.muted}>Starts with Title, Assignees, Status, Labels, Repository and Milestone fields and a table view.</p>
        <button type="submit" hidden />
      </form>
    </Dialog>
  );
});

/** Can the viewer create projects for this owner (self or org member)? */
export function canCreateFor(ownerId: ID): boolean {
  const me = session.user?.id;
  if (me == null) return false;
  if (me === ownerId) return true;
  return store()
    .byIndex('membership', 'orgId', ownerId)
    .some((m) => m.userId === me);
}

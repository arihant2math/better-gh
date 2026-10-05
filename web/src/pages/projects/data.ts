/**
 * Project data loading: the synced store is the source of truth; the
 * snapshot endpoint fills what the store lacks (projects outside synced
 * scopes, issues/repos/users referenced by items in other repositories).
 */
import { runInAction } from 'mobx';
import { useMemo } from 'react';
import { prefetch, useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { getProjectSnapshot, projectSnapshotKey, type ProjectRole, type ProjectSnapshot } from '../../api/projects';
import { session } from '../../app/session';
import { hasSync, store } from '../../sync';
import type { ID, Issue, Label, Milestone, ModelName, Project, Repo, User } from '../../sync/models';
import { isProjectSynced, projectByNumber } from '../../sync/projects';
import { orgByLogin, userByLogin } from '../../sync/selectors';

const CHILD_MODELS = ['projectField', 'projectView', 'projectItem', 'projectWorkflow'] as const;

/**
 * Merge a snapshot into the pool. Project rows are only merged when the
 * project is not streamed to this client (or not in the store yet); they are
 * kept in memory only (never persisted). Stale rows of such projects are
 * dropped. Users missing from the store are added.
 */
export function mergeSnapshot(snap: ProjectSnapshot): void {
  if (!hasSync()) return;
  const pool = store();
  runInAction(() => {
    const users = (snap.users ?? []).filter((u) => !pool.get('user', u.id));
    if (users.length) pool.loadRows({ user: users }, { persist: false });
    const synced = isProjectSynced(snap.project) && !!pool.get('project', snap.project.id);
    if (synced) return;
    pool.loadRows(
      {
        project: [snap.project],
        projectField: snap.fields ?? [],
        projectView: snap.views ?? [],
        projectItem: snap.items ?? [],
        projectWorkflow: snap.workflows ?? [],
      },
      { persist: false },
    );
    const present: Record<string, ProjectSnapshot[keyof ProjectSnapshot]> = {
      projectField: snap.fields,
      projectView: snap.views,
      projectItem: snap.items,
      projectWorkflow: snap.workflows,
    };
    for (const m of CHILD_MODELS) {
      const keep = new Set(((present[m] ?? []) as { id: ID }[]).map((r) => r.id));
      const stale = pool
        .byIndex(m, 'projectId', snap.project.id)
        .filter((r) => r.id > 0 && !keep.has(r.id) && !pool.isPending(m as ModelName, r.id))
        .map((r) => r.id);
      pool.removeRows(m, stale);
    }
  });
}

export function loadSnapshot(owner: string, number: number): Promise<ProjectSnapshot> {
  return getProjectSnapshot(owner, number).then((s) => {
    mergeSnapshot(s);
    return s;
  });
}

export function prefetchProject(owner: string, number: number): void {
  prefetch(projectSnapshotKey(owner, number), () => loadSnapshot(owner, number));
}

export interface ProjectCtx {
  project: Project;
  owner: string;
  ownerKind: 'orgs' | 'users';
  /** `/orgs/acme/projects/1` */
  base: string;
  role: ProjectRole;
  canWrite: boolean;
  canAdmin: boolean;
  issue(id: ID | null | undefined): Issue | undefined;
  repo(id: ID | null | undefined): Repo | undefined;
  user(id: ID | null | undefined): User | undefined;
  label(id: ID): Label | undefined;
  milestone(id: ID | null | undefined): Milestone | undefined;
  labelsForRepo(repoId: ID): Label[];
  /** Draft body (store, else snapshot). `undefined` = unknown. */
  draftBody(itemId: ID): string | null | undefined;
  /** People that can be assigned to drafts / shown in pickers. */
  people(): User[];
  /** Whether issue mutations can run (repo + issue are in the synced store). */
  issueInStore(id: ID | null | undefined): boolean;
}

function deriveRole(project: Project): ProjectRole {
  const viewer = session.user?.id;
  if (viewer == null) return 'read';
  if (project.ownerId === viewer) return 'admin';
  const m = store()
    .byIndex('membership', 'orgId', project.ownerId)
    .find((x) => x.userId === viewer);
  if (m) return m.role === 'admin' ? 'admin' : 'write';
  return 'read';
}

export type ProjectState = { status: 'loading' } | { status: 'missing'; error: unknown } | { status: 'ok'; ctx: ProjectCtx; snapshot?: ProjectSnapshot };

/** Resolve a project by owner login + number. Call inside an `observer`. */
export function useProject(owner: string, number: number, ownerKind: 'orgs' | 'users'): ProjectState {
  const key = projectSnapshotKey(owner, number);
  const res = useResource(Number.isFinite(number) ? key : null, () => loadSnapshot(owner, number), { ttlMs: 20_000 });
  const snap = res.data;
  const maps = useMemo(() => {
    const by = <T extends { id: ID }>(rows: T[] | undefined) => new Map((rows ?? []).map((r) => [r.id, r]));
    return {
      issues: by(snap?.issues),
      repos: by(snap?.repos),
      users: by(snap?.users),
      labels: by(snap?.labels),
      milestones: by(snap?.milestones),
      items: by(snap?.items),
    };
  }, [snap]);

  const s = store();
  const ownerRow = orgByLogin(owner) ?? userByLogin(owner);
  const project = (ownerRow ? projectByNumber(ownerRow.id, number) : undefined) ?? (snap ? s.get('project', snap.project.id) : undefined) ?? snap?.project;
  if (!project) {
    if (res.error && !res.loading) return { status: 'missing', error: res.error };
    return { status: 'loading' };
  }
  const role = snap?.role ?? deriveRole(project);
  const base = `/${ownerKind}/${owner}/projects/${project.number}`;
  const ctx: ProjectCtx = {
    project,
    owner,
    ownerKind,
    base,
    role,
    canWrite: role !== 'read' && project.id > 0,
    canAdmin: role === 'admin' && project.id > 0,
    issue: (id) => (id == null ? undefined : (s.get('issue', id) ?? maps.issues.get(id))),
    repo: (id) => (id == null ? undefined : (s.get('repo', id) ?? maps.repos.get(id))),
    user: (id) => (id == null ? undefined : (s.get('user', id) ?? maps.users.get(id))),
    label: (id) => s.get('label', id) ?? maps.labels.get(id),
    milestone: (id) => (id == null ? undefined : (s.get('milestone', id) ?? maps.milestones.get(id))),
    labelsForRepo: (repoId) => {
      const local = s.byIndex('label', 'repoId', repoId);
      const list = local.length ? local : (snap?.labels ?? []).filter((l) => l.repoId === repoId);
      return [...list].sort((a, b) => a.name.localeCompare(b.name));
    },
    draftBody: (itemId) => {
      const live = s.get('projectItem', itemId)?.body;
      return live !== undefined ? live : maps.items.get(itemId)?.body;
    },
    people: () => {
      const ids = new Set<ID>();
      for (const m of s.byIndex('membership', 'orgId', project.ownerId)) ids.add(m.userId);
      if (ids.size === 0) ids.add(project.ownerId);
      for (const u of snap?.users ?? []) ids.add(u.id);
      if (session.user) ids.add(session.user.id);
      return [...ids]
        .map((id) => s.get('user', id) ?? maps.users.get(id))
        .filter((u): u is User => !!u && u.type === 'User')
        .sort((a, b) => a.login.localeCompare(b.login));
    },
    issueInStore: (id) => id != null && !!s.get('issue', id) && !!s.get('repo', s.get('issue', id)!.repoId),
  };
  return { status: 'ok', ctx, snapshot: snap };
}

export function isNotFound(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 404 || e.status === 403);
}

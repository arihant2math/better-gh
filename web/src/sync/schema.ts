import type { ID, ModelMap, ModelName } from './models';

/** Per-model metadata used by the object pool, persistence and the sync client. */
export interface ModelSchema<M extends ModelName> {
  /** Fields with a secondary index (`pool.byIndex`). Array fields index every element. */
  indexes?: readonly (keyof ModelMap[M] & string)[];
  /** Unique derived keys (`pool.byKey`), e.g. repo by "owner/name". */
  keys?: Record<string, (row: ModelMap[M]) => string>;
  /**
   * Scope the row belongs to (`null` for referenced-only models like `user`).
   * `lookup` reads other rows (e.g. a project's children derive their scope
   * from the project row).
   */
  scope: ((row: ModelMap[M], viewerId: ID, lookup?: ScopeLookup) => string) | null;
  /** Lazy fields: absent in bootstrap, loaded via partial sync. Initialized to `undefined`. */
  lazyFields?: readonly (keyof ModelMap[M] & string)[];
  /** Lazy model: not in bootstrap, loaded per parent via partial sync. */
  lazy?: boolean;
  /** Child models removed locally when a row of this model is deleted (child field → this id). */
  cascade?: readonly { model: ModelName; field: string }[];
}

/** Read another row while computing a scope (base/server rows). */
export type ScopeLookup = <M extends ModelName>(model: M, id: ID) => ModelMap[M] | undefined;

type SchemaMap = { [M in ModelName]: ModelSchema<M> };

const repoScope = (r: { repoId: ID }) => `repo:${r.repoId}`;
const orgScope = (r: { orgId: ID }) => `org:${r.orgId}`;
const viewerScope = (_r: unknown, viewerId: ID) => `user:${viewerId}`;

/** A project lives in its owner's scope: `org:{id}` for organizations, else `user:{id}`. */
export function projectScope(p: { ownerId: ID }, lookup?: ScopeLookup): string {
  return lookup?.('org', p.ownerId) ? `org:${p.ownerId}` : `user:${p.ownerId}`;
}

/**
 * Project children only carry `projectId`; their scope is the project's.
 * Without the project row the placeholder `project:{id}` is returned (such
 * rows still go away with their project through `cascade`).
 */
const projectChildScope = (r: { projectId: ID }, _v: ID, lookup?: ScopeLookup) => {
  const p = lookup?.('project', r.projectId);
  return p ? projectScope(p, lookup) : `project:${r.projectId}`;
};

export const SCHEMA: SchemaMap = {
  user: { scope: null, keys: { login: (u) => u.login.toLowerCase() } },
  org: { scope: (o) => `org:${o.id}`, keys: { login: (o) => o.login.toLowerCase() } },
  membership: { scope: orgScope, indexes: ['orgId', 'userId'] },
  team: { scope: orgScope, indexes: ['orgId'] },
  repo: {
    scope: (r) => `repo:${r.id}`,
    indexes: ['ownerId'],
    keys: { fullName: (r) => `${r.owner}/${r.name}`.toLowerCase() },
  },
  viewerRepo: { scope: viewerScope },
  label: { scope: repoScope, indexes: ['repoId'] },
  milestone: { scope: repoScope, indexes: ['repoId'] },
  issue: {
    scope: repoScope,
    indexes: ['repoId', 'assigneeIds', 'authorId'],
    keys: { number: (i) => `${i.repoId}#${i.number}` },
    lazyFields: ['body'],
    cascade: [
      { model: 'comment', field: 'issueId' },
      { model: 'review', field: 'issueId' },
      { model: 'issueEvent', field: 'issueId' },
      // The DB cascades project items of a deleted issue without a delta.
      { model: 'projectItem', field: 'issueId' },
    ],
  },
  comment: { scope: repoScope, indexes: ['issueId'], lazy: true },
  review: { scope: repoScope, indexes: ['issueId'], lazy: true },
  issueEvent: { scope: repoScope, indexes: ['issueId'], lazy: true },
  notification: { scope: viewerScope, indexes: ['repoId'] },
  project: {
    scope: (p, _v, lookup) => projectScope(p, lookup),
    indexes: ['ownerId', 'linkedRepoIds'],
    keys: { number: (p) => `${p.ownerId}#${p.number}` },
    cascade: [
      { model: 'projectField', field: 'projectId' },
      { model: 'projectView', field: 'projectId' },
      { model: 'projectItem', field: 'projectId' },
      { model: 'projectWorkflow', field: 'projectId' },
    ],
  },
  projectField: { scope: projectChildScope, indexes: ['projectId'] },
  projectView: { scope: projectChildScope, indexes: ['projectId'] },
  projectItem: { scope: projectChildScope, indexes: ['projectId', 'issueId'], lazyFields: ['body'] },
  projectWorkflow: { scope: projectChildScope, indexes: ['projectId'] },
};

export const MODEL_NAMES = Object.keys(SCHEMA) as ModelName[];

/** Bump when the client-side persisted shape changes; old IndexedDB data is discarded. */
export const CLIENT_SCHEMA_VERSION = 2;

import type { ID, ModelMap, ModelName } from './models';

/** Per-model metadata used by the object pool, persistence and the sync client. */
export interface ModelSchema<M extends ModelName> {
  /** Fields with a secondary index (`pool.byIndex`). Array fields index every element. */
  indexes?: readonly (keyof ModelMap[M] & string)[];
  /** Unique derived keys (`pool.byKey`), e.g. repo by "owner/name". */
  keys?: Record<string, (row: ModelMap[M]) => string>;
  /** Scope the row belongs to (`null` for referenced-only models like `user`). */
  scope: ((row: ModelMap[M], viewerId: ID) => string) | null;
  /** Lazy fields: absent in bootstrap, loaded via partial sync. Initialized to `undefined`. */
  lazyFields?: readonly (keyof ModelMap[M] & string)[];
  /** Lazy model: not in bootstrap, loaded per parent via partial sync. */
  lazy?: boolean;
  /** Child models removed locally when a row of this model is deleted (child field → this id). */
  cascade?: readonly { model: ModelName; field: string }[];
}

type SchemaMap = { [M in ModelName]: ModelSchema<M> };

const repoScope = (r: { repoId: ID }) => `repo:${r.repoId}`;
const orgScope = (r: { orgId: ID }) => `org:${r.orgId}`;
const viewerScope = (_r: unknown, viewerId: ID) => `user:${viewerId}`;

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
    ],
  },
  comment: { scope: repoScope, indexes: ['issueId'], lazy: true },
  review: { scope: repoScope, indexes: ['issueId'], lazy: true },
  issueEvent: { scope: repoScope, indexes: ['issueId'], lazy: true },
  notification: { scope: viewerScope, indexes: ['repoId'] },
};

export const MODEL_NAMES = Object.keys(SCHEMA) as ModelName[];

/** Bump when the client-side persisted shape changes; old IndexedDB data is discarded. */
export const CLIENT_SCHEMA_VERSION = 1;

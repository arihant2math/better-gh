/**
 * Projects: selectors and optimistic mutations over the private
 * `/_bgh/projects/...` endpoints (docs/packages/projects-wiki.md).
 *
 * Lives outside `selectors.ts` / `mutations.ts` so the code only ships in the
 * (lazy) project page chunks, not in the initial bundle.
 *
 * Projects outside the synced scopes (e.g. a public project of an org you
 * are not a member of) are merged into the pool from the snapshot endpoint
 * without persistence. They receive no deltas, so for those the mutation
 * response row is applied to the base when the request succeeds.
 */
import { runInAction } from 'mobx';
import { compareKeys, generateKeyBetween } from './fractional';
import { store, sync } from './index';
import type {
  ID,
  ModelName,
  Project,
  ProjectField,
  ProjectFieldOption,
  ProjectFieldType,
  ProjectItem,
  ProjectIterationConfig,
  ProjectLayout,
  ProjectValue,
  ProjectView,
  ProjectWorkflow,
  ProjectWorkflowKind,
} from './models';
import { commit, nowIso } from './mutations';
import { ops, tempId, type OverlayOp } from './overlay';
import { projectScope } from './schema';
import type { TxRequest } from './transactions';

// ------------------------------------------------------------------ selectors

export function projectByNumber(ownerId: ID, number: number): Project | undefined {
  return store().byKey('project', 'number', `${ownerId}#${number}`);
}

export function projectsForOwner(ownerId: ID): Project[] {
  return store().byIndex('project', 'ownerId', ownerId);
}

export function projectsLinkedToRepo(repoId: ID): Project[] {
  return store().byIndex('project', 'linkedRepoIds', repoId);
}

export function fieldsForProject(projectId: ID): ProjectField[] {
  return store()
    .byIndex('projectField', 'projectId', projectId)
    .sort((a, b) => a.position - b.position || a.id - b.id);
}

export function viewsForProject(projectId: ID): ProjectView[] {
  return store()
    .byIndex('projectView', 'projectId', projectId)
    .sort((a, b) => a.position - b.position || a.number - b.number);
}

export function itemsForProject(projectId: ID): ProjectItem[] {
  return store().byIndex('projectItem', 'projectId', projectId);
}

export function workflowsForProject(projectId: ID): ProjectWorkflow[] {
  return store().byIndex('projectWorkflow', 'projectId', projectId);
}

/** Manual order key of an item in a view. */
export function itemOrderKey(item: Pick<ProjectItem, 'position' | 'viewPositions'>, viewId: ID | null | undefined): string {
  return (viewId != null ? item.viewPositions[String(viewId)] : undefined) ?? item.position;
}

export function compareItems(viewId: ID | null | undefined) {
  return (a: ProjectItem, b: ProjectItem) => compareKeys(itemOrderKey(a, viewId), itemOrderKey(b, viewId)) || a.id - b.id;
}

/** Is the project's scope streamed to this client (deltas arrive)? */
export function isProjectSynced(project: Pick<Project, 'ownerId'>): boolean {
  const s = sync();
  return s.scopes.has(projectScope(project, (m, id) => s.pool.get(m, id) as never));
}

/**
 * Key that places an item between `before` and `after` (either may be
 * missing). Falls back gracefully when neighbours share or invert keys.
 */
export function keyBetween(before: string | null, after: string | null): string {
  if (before !== null && after !== null && compareKeys(before, after) >= 0) return generateKeyBetween(before, null);
  return generateKeyBetween(before, after);
}

// ------------------------------------------------------------------ plumbing

const P = (projectId: ID, suffix = '') => `/_bgh/projects/${projectId}${suffix}`;

type Apply = { model: ModelName } | { remove: [ModelName, ID] } | null;

function projectCommit(project: Pick<Project, 'id' | 'ownerId'>, label: string, opsList: OverlayOp[], request: TxRequest, apply: Apply) {
  const res = commit(label, opsList, request);
  if (apply && !isProjectSynced(project)) {
    res.done.then(
      (r) =>
        runInAction(() => {
          const pool = store();
          if ('model' in apply) {
            const row = r.data as { id?: unknown } | null;
            if (row && typeof row === 'object' && typeof row.id === 'number') {
              pool.loadRows({ [apply.model]: [row] } as never, { persist: false });
            }
          } else {
            pool.removeRows(apply.remove[0], [apply.remove[1]]);
          }
          // No delta will echo this tx: drop the overlay now, in the same action.
          sync().queue.confirm(res.tx);
        }),
      () => undefined,
    );
  }
  return res;
}

const isTempOption = (id: string) => id.startsWith('tmp');

/** Option payload for the server: new options are sent without `id`. */
function optionsPayload(options: ProjectFieldOption[] | null | undefined) {
  return options?.map((o) => (isTempOption(o.id) ? { name: o.name, color: o.color, description: o.description } : o));
}

export function tempOptionId(): string {
  return `tmp${Math.random().toString(16).slice(2, 10)}`;
}

// ------------------------------------------------------------------ projects

export function createProject(owner: { id: ID; login: string }, input: { title: string; shortDescription?: string; public?: boolean }) {
  const now = nowIso();
  const existing = projectsForOwner(owner.id);
  const row: Project = {
    id: tempId(),
    ownerId: owner.id,
    number: existing.reduce((m, p) => Math.max(m, p.number), 0) + 1,
    title: input.title,
    shortDescription: input.shortDescription ?? null,
    readme: null,
    public: !!input.public,
    closed: false,
    closedAt: null,
    creatorId: store().viewerId,
    linkedRepoIds: [],
    createdAt: now,
    updatedAt: now,
  };
  return commit(`Create project ${input.title}`, [ops.insert('project', row)], {
    method: 'POST',
    path: '/_bgh/projects',
    body: { owner: owner.login, title: input.title, shortDescription: input.shortDescription, public: !!input.public },
  });
}

export interface ProjectPatch {
  title?: string;
  shortDescription?: string | null;
  readme?: string | null;
  public?: boolean;
  closed?: boolean;
}

export function updateProject(project: Project, patch: ProjectPatch) {
  const local: Partial<Project> = { ...patch, updatedAt: nowIso() };
  if (patch.closed !== undefined) local.closedAt = patch.closed ? nowIso() : null;
  return projectCommit(
    project,
    `Update project ${project.title}`,
    [ops.update('project', project.id, local)],
    { method: 'PATCH', path: P(project.id), body: patch },
    { model: 'project' },
  );
}

export function deleteProject(project: Project) {
  return projectCommit(
    project,
    `Delete project ${project.title}`,
    [ops.delete('project', project.id)],
    { method: 'DELETE', path: P(project.id) },
    { remove: ['project', project.id] },
  );
}

export function setRepoLinked(project: Project, repoId: ID, linked: boolean) {
  return projectCommit(
    project,
    linked ? 'Link repository' : 'Unlink repository',
    [ops.update('project', project.id, { linkedRepoIds: linked ? { $add: [repoId] } : { $remove: [repoId] } })],
    { method: linked ? 'PUT' : 'DELETE', path: P(project.id, `/repos/${repoId}`) },
    { model: 'project' },
  );
}

// ------------------------------------------------------------------ fields

export interface FieldInput {
  name: string;
  dataType: ProjectFieldType;
  options?: ProjectFieldOption[] | null;
  iterations?: ProjectIterationConfig | null;
}

export function createField(project: Project, input: FieldInput) {
  const now = nowIso();
  const fields = fieldsForProject(project.id);
  const row: ProjectField = {
    id: tempId(),
    projectId: project.id,
    name: input.name,
    dataType: input.dataType,
    position: fields.reduce((m, f) => Math.max(m, f.position), 0) + 1,
    options: input.options ?? null,
    iterations: input.iterations ?? null,
    createdAt: now,
    updatedAt: now,
  };
  return projectCommit(
    project,
    `Create field ${input.name}`,
    [ops.insert('projectField', row)],
    {
      method: 'POST',
      path: P(project.id, '/fields'),
      body: { name: input.name, dataType: input.dataType, options: optionsPayload(input.options), iterations: input.iterations ?? undefined },
    },
    { model: 'projectField' },
  );
}

export function updateField(project: Project, field: ProjectField, patch: Partial<Pick<ProjectField, 'name' | 'options' | 'iterations' | 'position'>>) {
  const body: Record<string, unknown> = { ...patch };
  if (patch.options) body.options = optionsPayload(patch.options);
  return projectCommit(
    project,
    `Update field ${field.name}`,
    [ops.update('projectField', field.id, { ...patch, updatedAt: nowIso() })],
    { method: 'PATCH', path: P(project.id, `/fields/${field.id}`), body },
    { model: 'projectField' },
  );
}

export function deleteField(project: Project, field: ProjectField) {
  return projectCommit(
    project,
    `Delete field ${field.name}`,
    [ops.delete('projectField', field.id)],
    { method: 'DELETE', path: P(project.id, `/fields/${field.id}`) },
    { remove: ['projectField', field.id] },
  );
}

// ------------------------------------------------------------------ items

function lastKey(projectId: ID): string | null {
  let max: string | null = null;
  for (const i of itemsForProject(projectId)) if (max === null || compareKeys(i.position, max) > 0) max = i.position;
  return max;
}

function newItem(project: Project, patch: Partial<ProjectItem>): ProjectItem {
  const now = nowIso();
  return {
    id: tempId(),
    projectId: project.id,
    contentType: 'DraftIssue',
    issueId: null,
    title: null,
    body: null,
    assigneeIds: [],
    archived: false,
    position: generateKeyBetween(lastKey(project.id), null),
    viewPositions: {},
    values: {},
    creatorId: store().viewerId,
    createdAt: now,
    updatedAt: now,
    ...patch,
  };
}

export function addDraftItem(project: Project, title: string, values: Record<string, ProjectValue> = {}) {
  const row = newItem(project, { title, values });
  return projectCommit(
    project,
    'Add draft',
    [ops.insert('projectItem', row)],
    { method: 'POST', path: P(project.id, '/items'), body: { draft: { title }, position: row.position } },
    { model: 'projectItem' },
  );
}

/** Add an issue/PR by id (when known locally) or by `owner/repo#number`. */
export function addIssueItem(project: Project, ref: { issueId?: ID; isPr?: boolean; owner?: string; repo?: string; number?: number }) {
  const existing = ref.issueId != null ? itemsForProject(project.id).find((i) => i.issueId === ref.issueId) : undefined;
  const row = newItem(project, { contentType: ref.isPr ? 'PullRequest' : 'Issue', issueId: ref.issueId ?? null });
  const body =
    ref.issueId != null ? { issueId: ref.issueId, position: row.position } : { owner: ref.owner, repo: ref.repo, number: ref.number, position: row.position };
  // An issue that already is an item is not duplicated (server answers 200 with it).
  const list = existing ? [] : ref.issueId != null ? [ops.insert('projectItem', row)] : [];
  return projectCommit(project, 'Add item', list, { method: 'POST', path: P(project.id, '/items'), body }, { model: 'projectItem' });
}

export interface ItemPatch {
  archived?: boolean;
  title?: string;
  body?: string | null;
  assigneeIds?: ID[];
  /** `null` clears a value. */
  values?: Record<string, ProjectValue | null>;
}

export function updateItem(project: Project, item: ProjectItem, patch: ItemPatch, label = 'Update item') {
  const { values, ...rest } = patch;
  const local: Record<string, unknown> = { ...rest, updatedAt: nowIso() };
  if (values) local.values = { $merge: values };
  return projectCommit(
    project,
    label,
    [ops.update('projectItem', item.id, local as never)],
    { method: 'PATCH', path: P(project.id, `/items/${item.id}`), body: patch },
    { model: 'projectItem' },
  );
}

export function setItemValue(project: Project, item: ProjectItem, fieldId: ID, value: ProjectValue | null) {
  return updateItem(project, item, { values: { [String(fieldId)]: value } }, 'Set field');
}

/**
 * Move an item within a view (and optionally into another board column /
 * group by setting field values) in one request.
 */
export function moveItem(project: Project, item: ProjectItem, viewId: ID, key: string, values?: Record<string, ProjectValue | null>) {
  const local: Record<string, unknown> = { viewPositions: { $merge: { [String(viewId)]: key } }, updatedAt: nowIso() };
  if (values && Object.keys(values).length) local.values = { $merge: values };
  const body: Record<string, unknown> = { viewId, viewPosition: key };
  if (values && Object.keys(values).length) body.values = values;
  return projectCommit(
    project,
    'Move item',
    [ops.update('projectItem', item.id, local as never)],
    { method: 'PATCH', path: P(project.id, `/items/${item.id}`), body },
    { model: 'projectItem' },
  );
}

export function deleteItem(project: Project, item: ProjectItem) {
  return projectCommit(
    project,
    'Delete item',
    [ops.delete('projectItem', item.id)],
    { method: 'DELETE', path: P(project.id, `/items/${item.id}`) },
    { remove: ['projectItem', item.id] },
  );
}

// ------------------------------------------------------------------ views

export type ViewInput = Partial<Omit<ProjectView, 'id' | 'projectId' | 'number' | 'createdAt' | 'updatedAt'>>;

export function createView(project: Project, input: ViewInput) {
  const now = nowIso();
  const views = viewsForProject(project.id);
  const fields = fieldsForProject(project.id);
  const status = fields.find((f) => f.dataType === 'status');
  const layout: ProjectLayout = input.layout ?? 'table';
  const row: ProjectView = {
    id: tempId(),
    projectId: project.id,
    number: views.reduce((m, v) => Math.max(m, v.number), 0) + 1,
    name: input.name ?? `View ${views.length + 1}`,
    layout,
    position: views.reduce((m, v) => Math.max(m, v.position), 0) + 1,
    filter: '',
    groupByFieldId: null,
    columnFieldId: layout === 'board' ? (status?.id ?? null) : null,
    sortBy: [],
    visibleFieldIds: fields.filter((f) => ['title', 'assignees', 'status'].includes(f.dataType)).map((f) => f.id),
    hiddenColumnIds: [],
    dateFieldId: null,
    createdAt: now,
    updatedAt: now,
    ...input,
  };
  const { id: _i, projectId: _p, number: _n, createdAt: _c, updatedAt: _u, ...body } = row;
  return projectCommit(
    project,
    `Create view ${row.name}`,
    [ops.insert('projectView', row)],
    { method: 'POST', path: P(project.id, '/views'), body },
    { model: 'projectView' },
  );
}

export function duplicateView(project: Project, view: ProjectView) {
  const { id: _i, projectId: _p, number: _n, createdAt: _c, updatedAt: _u, position: _pos, name, ...rest } = view;
  return createView(project, { ...rest, name: `${name} (copy)` });
}

export function updateView(project: Project, view: ProjectView, patch: ViewInput) {
  return projectCommit(
    project,
    `Update view ${view.name}`,
    [ops.update('projectView', view.id, { ...patch, updatedAt: nowIso() } as never)],
    { method: 'PATCH', path: P(project.id, `/views/${view.id}`), body: patch },
    { model: 'projectView' },
  );
}

export function deleteView(project: Project, view: ProjectView) {
  return projectCommit(
    project,
    `Delete view ${view.name}`,
    [ops.delete('projectView', view.id)],
    { method: 'DELETE', path: P(project.id, `/views/${view.id}`) },
    { remove: ['projectView', view.id] },
  );
}

// ------------------------------------------------------------------ workflows

export function setWorkflow(project: Project, kind: ProjectWorkflowKind, enabled: boolean, config: ProjectWorkflow['config'] = {}) {
  const existing = workflowsForProject(project.id).find((w) => w.kind === kind);
  const now = nowIso();
  const op = existing
    ? ops.update('projectWorkflow', existing.id, { enabled, config, updatedAt: now })
    : ops.insert('projectWorkflow', { id: tempId(), projectId: project.id, kind, enabled, config, updatedAt: now });
  return projectCommit(
    project,
    `Workflow ${kind}`,
    [op],
    { method: 'PUT', path: P(project.id, `/workflows/${kind}`), body: { enabled, config } },
    { model: 'projectWorkflow' },
  );
}

/* Mock implementation of the bgh-projects private API (docs/packages/projects-wiki.md). */
import { compareKeys, generateKeyBetween, generateNKeysBetween } from '../sync/fractional';
import type {
  ID,
  Issue,
  Label,
  Milestone,
  Project,
  ProjectField,
  ProjectFieldOption,
  ProjectFieldType,
  ProjectItem,
  ProjectIterationConfig,
  ProjectValue,
  ProjectView,
  ProjectWorkflow,
  ProjectWorkflowKind,
  Repo,
  User,
} from '../sync/models';
import { Rng, fakeSha, iso } from './rng';
import type { MockDb } from './seed';
import type { Ctx, MockServer, Resp, RouteFn } from './server';

const BUILTIN: ProjectFieldType[] = ['title', 'assignees', 'status', 'labels', 'repository', 'milestone'];
const WORKFLOW_KINDS: ProjectWorkflowKind[] = ['item_added', 'item_reopened', 'item_closed', 'pr_merged', 'auto_add', 'auto_archive'];
const DAY = 86_400_000;

let optCounter = 0;
function optionId(salt: string): string {
  optCounter += 1;
  return fakeSha(`${salt}:${optCounter}:${Date.now()}`).slice(0, 8);
}

function ymd(ms: number): string {
  return new Date(ms).toISOString().slice(0, 10);
}

interface Ids {
  next(): ID;
}

/** Default fields, view and workflows of a new project (same as the server). */
function scaffold(ids: Ids, project: Project, now: string) {
  const mk = (name: string, dataType: ProjectFieldType, position: number, options: ProjectFieldOption[] | null = null): ProjectField => ({
    id: ids.next(),
    projectId: project.id,
    name,
    dataType,
    position,
    options,
    iterations: null,
    createdAt: now,
    updatedAt: now,
  });
  const status = mk('Status', 'status', 3, [
    { id: optionId('todo'), name: 'Todo', color: 'GRAY', description: 'This item has not been started' },
    { id: optionId('prog'), name: 'In Progress', color: 'YELLOW', description: 'This is actively being worked on' },
    { id: optionId('done'), name: 'Done', color: 'PURPLE', description: 'This has been completed' },
  ]);
  const fields = [
    mk('Title', 'title', 1),
    mk('Assignees', 'assignees', 2),
    status,
    mk('Labels', 'labels', 4),
    mk('Repository', 'repository', 5),
    mk('Milestone', 'milestone', 6),
  ];
  const view: ProjectView = {
    id: ids.next(),
    projectId: project.id,
    number: 1,
    name: 'View 1',
    layout: 'table',
    position: 1,
    filter: '',
    groupByFieldId: null,
    columnFieldId: null,
    sortBy: [],
    visibleFieldIds: [fields[0]!.id, fields[1]!.id, status.id],
    hiddenColumnIds: [],
    dateFieldId: null,
    createdAt: now,
    updatedAt: now,
  };
  const todo = status.options![0]!.id;
  const done = status.options![2]!.id;
  // Same defaults as bgh-projects (service.rs).
  const config = (kind: ProjectWorkflowKind): ProjectWorkflow['config'] =>
    kind === 'item_closed' || kind === 'pr_merged'
      ? { statusOptionId: done }
      : kind === 'item_added' || kind === 'item_reopened'
        ? { statusOptionId: todo }
        : kind === 'auto_add'
          ? { repoIds: [], filter: 'is:issue,pr is:open' }
          : {};
  const workflows: ProjectWorkflow[] = WORKFLOW_KINDS.map((kind) => ({
    id: ids.next(),
    projectId: project.id,
    kind,
    enabled: kind === 'item_closed' || kind === 'pr_merged',
    config: config(kind),
    updatedAt: now,
  }));
  return { fields, status, view, workflows };
}

// ------------------------------------------------------------------ seed

export function seedProjects(db: MockDb, now: number): void {
  const t = db.tables;
  const rng = new Rng(4242);
  const ids: Ids = { next: () => db.nextId++ };
  const at = iso(now - 40 * DAY);
  const orgByLogin = (l: string) => [...t.org.values()].find((o) => o.login === l)!;
  const repoBy = (o: string, n: string) => [...t.repo.values()].find((r) => r.owner === o && r.name === n)!;
  const issuesOf = (r: Repo) => [...t.issue.values()].filter((i) => i.repoId === r.id);

  const make = (ownerId: ID, number: number, title: string, desc: string, extra: Partial<Project> = {}) => {
    const p: Project = {
      id: ids.next(),
      ownerId,
      number,
      title,
      shortDescription: desc,
      readme: null,
      public: false,
      closed: false,
      closedAt: null,
      creatorId: db.viewerId,
      linkedRepoIds: [],
      createdAt: at,
      updatedAt: at,
      ...extra,
    };
    t.project.set(p.id, p);
    const s = scaffold(ids, p, at);
    for (const f of s.fields) t.projectField.set(f.id, f);
    t.projectView.set(s.view.id, s.view);
    for (const w of s.workflows) t.projectWorkflow.set(w.id, w);
    return { project: p, ...s };
  };
  const field = (projectId: ID, name: string, dataType: ProjectFieldType, position: number, more: Partial<ProjectField> = {}): ProjectField => {
    const f: ProjectField = { id: ids.next(), projectId, name, dataType, position, options: null, iterations: null, createdAt: at, updatedAt: at, ...more };
    t.projectField.set(f.id, f);
    return f;
  };
  const view = (base: ProjectView, patch: Partial<ProjectView>): ProjectView => {
    const v: ProjectView = { ...base, id: ids.next(), ...patch };
    t.projectView.set(v.id, v);
    return v;
  };
  const addItems = (projectId: ID, rows: Partial<ProjectItem>[]) => {
    const keys = generateNKeysBetween(null, null, rows.length);
    rows.forEach((r, i) => {
      const item: ProjectItem = {
        id: ids.next(),
        projectId,
        contentType: 'DraftIssue',
        issueId: null,
        title: null,
        body: null,
        assigneeIds: [],
        archived: false,
        position: keys[i]!,
        viewPositions: {},
        values: {},
        creatorId: db.viewerId,
        createdAt: at,
        updatedAt: at,
        ...r,
      };
      t.projectItem.set(item.id, item);
    });
  };
  const itemFor = (i: Issue): Partial<ProjectItem> => ({ contentType: i.isPr ? 'PullRequest' : 'Issue', issueId: i.id });

  // ---- acme #1: roadmap with custom fields and several views
  const acme = orgByLogin('acme');
  const api = repoBy('acme', 'api');
  const web = repoBy('acme', 'web');
  const p1 = make(acme.id, 1, 'Acme Roadmap', 'What we are building this quarter', {
    public: true,
    linkedRepoIds: [api.id, web.id],
    readme:
      '## Acme Roadmap\n\nPlanning board for the **API** and **web** teams.\n\n- Triage new items into *Todo*\n- Keep *In Progress* small\n- Use the **Sprint** field for planning',
  });
  const [todo, prog, done] = p1.status.options!;
  const blocked: ProjectFieldOption = { id: optionId('blocked'), name: 'Blocked', color: 'RED', description: 'Waiting on something' };
  p1.status.options = [todo!, prog!, blocked, done!];
  const priority = field(p1.project.id, 'Priority', 'single_select', 7, {
    options: [
      { id: optionId('p0'), name: 'P0', color: 'RED', description: 'Drop everything' },
      { id: optionId('p1'), name: 'P1', color: 'ORANGE', description: 'This quarter' },
      { id: optionId('p2'), name: 'P2', color: 'BLUE', description: 'Nice to have' },
    ],
  });
  const estimate = field(p1.project.id, 'Estimate', 'number', 8);
  const target = field(p1.project.id, 'Target date', 'date', 9);
  const sprintStart = now - 28 * DAY - ((now / DAY) % 7) * DAY;
  const iterations: ProjectIterationConfig = {
    startDate: ymd(sprintStart),
    duration: 14,
    iterations: Array.from({ length: 6 }, (_, i) => ({
      id: optionId(`it${i}`),
      title: `Sprint ${i + 1}`,
      startDate: ymd(sprintStart + i * 14 * DAY),
      duration: 14,
    })),
  };
  const sprint = field(p1.project.id, 'Sprint', 'iteration', 10, { iterations });
  const notes = field(p1.project.id, 'Notes', 'text', 11);
  const [fTitle, fAssignees, , fLabels, fRepo] = p1.fields;
  t.projectView.set(p1.view.id, {
    ...p1.view,
    name: 'Backlog',
    visibleFieldIds: [fTitle!.id, fAssignees!.id, p1.status.id, priority.id, estimate.id, sprint.id, fRepo!.id, fLabels!.id, target.id],
  });
  view(p1.view, {
    number: 2,
    name: 'Board',
    layout: 'board',
    position: 2,
    columnFieldId: p1.status.id,
    visibleFieldIds: [fTitle!.id, fAssignees!.id, priority.id, fLabels!.id, estimate.id],
  });
  view(p1.view, {
    number: 3,
    name: 'By priority',
    position: 3,
    groupByFieldId: priority.id,
    sortBy: [{ fieldId: estimate.id, direction: 'desc' }],
    visibleFieldIds: [fTitle!.id, fAssignees!.id, p1.status.id, estimate.id, sprint.id],
  });
  view(p1.view, { number: 4, name: 'Roadmap', layout: 'roadmap', position: 4, dateFieldId: sprint.id, visibleFieldIds: [fTitle!.id, p1.status.id] });
  const statusOpts = [todo!.id, todo!.id, prog!.id, prog!.id, blocked.id, done!.id];
  const p1Issues = [
    ...rng.sample(
      issuesOf(api).filter((i) => i.state === 'open'),
      22,
    ),
    ...rng.sample(
      issuesOf(web).filter((i) => i.state === 'open'),
      12,
    ),
    ...rng.sample(
      issuesOf(api).filter((i) => i.state === 'closed'),
      4,
    ),
  ];
  const drafts = [
    'Write the Q4 launch blog post',
    'Decide on the pricing page experiment',
    'Plan the API deprecation timeline',
    'Collect feedback from design partners',
    'Update the onboarding checklist',
    'Spike: evaluate HTTP/3 support',
  ];
  addItems(p1.project.id, [
    ...p1Issues.map((i) => {
      const values: Record<string, ProjectValue> = { [p1.status.id]: i.state === 'closed' ? done!.id : rng.pick(statusOpts) };
      if (rng.chance(0.8)) values[priority.id] = rng.pick(priority.options!).id;
      if (rng.chance(0.7)) values[estimate.id] = rng.pick([1, 2, 3, 5, 8, 13]);
      if (rng.chance(0.75)) values[sprint.id] = rng.pick(iterations.iterations.slice(1, 5)).id;
      if (rng.chance(0.3)) values[target.id] = ymd(now + rng.int(-10, 60) * DAY);
      if (rng.chance(0.15)) values[notes.id] = rng.pick(['Needs design review', 'Customer ask', 'Blocked on infra', 'Good onboarding task']);
      return { ...itemFor(i), values };
    }),
    ...drafts.map((title, k) => ({
      title,
      body: k % 2 === 0 ? `Draft notes for **${title.toLowerCase()}**.\n\n- [ ] outline\n- [ ] review` : null,
      assigneeIds: k % 3 === 0 ? [db.viewerId] : [],
      values: { [p1.status.id]: k < 4 ? todo!.id : prog!.id, [priority.id]: priority.options![k % 3]!.id, [sprint.id]: iterations.iterations[2 + (k % 3)]!.id },
    })),
  ]);
  for (const w of p1.workflows) if (w.kind === 'item_added') t.projectWorkflow.set(w.id, { ...w, enabled: true, config: { statusOptionId: todo!.id } });

  // ---- acme #2: big triage table (virtualized), grouped by repository
  const p2 = make(acme.id, 2, 'Bug triage', 'Every open bug across Acme repositories');
  const bugIssues = [...t.issue.values()].filter((i) => {
    const r = t.repo.get(i.repoId)!;
    return r.ownerId === acme.id && i.state === 'open' && !i.isPr;
  });
  addItems(
    p2.project.id,
    bugIssues.map((i) => ({ ...itemFor(i), values: { [p2.status.id]: rng.pick(p2.status.options!).id } })),
  );
  t.projectView.set(p2.view.id, {
    ...p2.view,
    name: 'All bugs',
    visibleFieldIds: [p2.fields[0]!.id, p2.fields[1]!.id, p2.status.id, p2.fields[3]!.id, p2.fields[4]!.id],
  });
  view(p2.view, { number: 2, name: 'Triage board', layout: 'board', position: 2, columnFieldId: p2.status.id });

  // ---- acme #3: closed
  make(acme.id, 3, 'Q2 Planning', 'Archived planning board', { closed: true, closedAt: iso(now - 20 * DAY) });

  // ---- nebula-labs #1
  const nebula = orgByLogin('nebula-labs');
  const quark = repoBy('nebula-labs', 'quark');
  const p4 = make(nebula.id, 1, 'Quark 1.0', 'Release checklist for the first stable version', { linkedRepoIds: [quark.id] });
  addItems(
    p4.project.id,
    rng
      .sample(
        issuesOf(quark).filter((i) => i.state === 'open'),
        14,
      )
      .map((i) => ({ ...itemFor(i), values: { [p4.status.id]: rng.pick(p4.status.options!).id } })),
  );
  view(p4.view, { number: 2, name: 'Board', layout: 'board', position: 2, columnFieldId: p4.status.id });

  // ---- user project (ada)
  const ada = t.user.get(db.viewerId)!;
  const aoc = repoBy(ada.login, 'advent-of-code');
  const dot = repoBy(ada.login, 'dotfiles');
  const p5 = make(ada.id, 1, 'Personal', 'Side projects and reading list', { linkedRepoIds: [aoc.id] });
  addItems(p5.project.id, [
    ...[...issuesOf(aoc), ...issuesOf(dot)].map((i) => ({ ...itemFor(i), values: { [p5.status.id]: p5.status.options![i.state === 'closed' ? 2 : 0]!.id } })),
    { title: 'Read "Designing Data-Intensive Applications"', values: { [p5.status.id]: p5.status.options![1]!.id } },
    { title: 'Try out a new keyboard layout', values: { [p5.status.id]: p5.status.options![0]!.id } },
    { title: 'Migrate dotfiles to chezmoi', values: {} },
  ]);
  view(p5.view, { number: 2, name: 'Board', layout: 'board', position: 2, columnFieldId: p5.status.id });
}

// ------------------------------------------------------------------ routes

const err = (status: number, message: string, errors?: unknown[]): Resp => ({ status, body: errors ? { message, errors } : { message } });

export function installProjectRoutes(R: RouteFn, s: MockServer): void {
  const t = () => s.db.tables;
  const ids: Ids = { next: () => s.nextId() };
  const ownerByLogin = (login: string): { id: ID; login: string; isOrg: boolean } | undefined => {
    const l = login.toLowerCase();
    for (const o of t().org.values()) if (o.login.toLowerCase() === l) return { id: o.id, login: o.login, isOrg: true };
    const u = s.userByLogin(login);
    return u ? { id: u.id, login: u.login, isOrg: false } : undefined;
  };
  const projectById = (ctx: Ctx, i = 1): Project | Resp => t().project.get(Number(ctx.m[i])) ?? err(404, 'Not Found');
  const isResp = (x: unknown): x is Resp => typeof x === 'object' && x !== null && 'status' in x && !('ownerId' in x) && !('projectId' in x);
  const rows = <T extends { projectId: ID }>(m: Map<ID, T>, pid: ID) => [...m.values()].filter((r) => r.projectId === pid);
  const touch = (p: Project) => s.put('project', { ...p, updatedAt: s.now() });

  const snapshot = (p: Project) => {
    const items = rows(t().projectItem, p.id);
    const issueIds = new Set(items.map((i) => i.issueId).filter((x): x is ID => x != null));
    const issues = [...issueIds]
      .map((id) => t().issue.get(id))
      .filter((i): i is Issue => !!i)
      .map(({ body: _b, ...rest }) => rest as Issue);
    const repoIds = new Set([...issues.map((i) => i.repoId), ...p.linkedRepoIds]);
    const repos = [...repoIds].map((id) => t().repo.get(id)).filter((r): r is Repo => !!r);
    const labels: Label[] = [...t().label.values()].filter((l) => repoIds.has(l.repoId));
    const milestones: Milestone[] = [...t().milestone.values()].filter((m) => repoIds.has(m.repoId));
    const userIds = new Set<ID>([s.db.viewerId]);
    if (p.creatorId) userIds.add(p.creatorId);
    for (const i of issues) [i.authorId, ...i.assigneeIds].forEach((u) => userIds.add(u));
    for (const i of items) i.assigneeIds.forEach((u) => userIds.add(u));
    for (const m of t().membership.values()) if (m.orgId === p.ownerId) userIds.add(m.userId);
    const users = [...userIds].map((id) => t().user.get(id)).filter((u): u is User => !!u);
    const org = t().org.get(p.ownerId);
    const ou = t().user.get(p.ownerId);
    return {
      project: p,
      owner: org
        ? { id: org.id, login: org.login, name: org.name, type: 'Organization', avatarUrl: org.avatarUrl }
        : { id: ou!.id, login: ou!.login, name: ou!.name, type: 'User', avatarUrl: ou!.avatarUrl },
      fields: rows(t().projectField, p.id),
      views: rows(t().projectView, p.id),
      items,
      workflows: rows(t().projectWorkflow, p.id),
      issues,
      repos,
      users,
      labels,
      milestones,
      role: 'admin',
    };
  };

  // ---------------- reads
  R('GET', '/_bgh/owners/:owner/projects', (ctx) => {
    const owner = ownerByLogin(decodeURIComponent(ctx.m[1]!));
    if (!owner) return err(404, 'Not Found');
    const state = ctx.url.searchParams.get('state') ?? 'open';
    const q = (ctx.url.searchParams.get('q') ?? '').toLowerCase();
    const projects = [...t().project.values()].filter(
      (p) => p.ownerId === owner.id && (state === 'all' || (state === 'closed') === p.closed) && (!q || p.title.toLowerCase().includes(q)),
    );
    const users = [...new Set(projects.map((p) => p.creatorId).filter((x): x is ID => x != null))].map((id) => t().user.get(id)).filter(Boolean);
    return { status: 200, body: { projects, users } };
  });
  R('GET', '/_bgh/owners/:owner/projects/:number', (ctx) => {
    const owner = ownerByLogin(decodeURIComponent(ctx.m[1]!));
    const p = owner && [...t().project.values()].find((x) => x.ownerId === owner.id && x.number === Number(ctx.m[2]));
    return p ? { status: 200, body: snapshot(p) } : err(404, 'Not Found');
  });
  R('GET', '/_bgh/repos/:owner/:repo/projects', (ctx) => {
    const repo = s.repo(decodeURIComponent(ctx.m[1]!), decodeURIComponent(ctx.m[2]!));
    if (!repo) return err(404, 'Not Found');
    const withItems = new Set(
      [...t().projectItem.values()].filter((i) => i.issueId != null && t().issue.get(i.issueId)?.repoId === repo.id).map((i) => i.projectId),
    );
    const projects = [...t().project.values()].filter((p) => p.linkedRepoIds.includes(repo.id) || withItems.has(p.id));
    const owners = [...new Set(projects.map((p) => p.ownerId))].map((id) => {
      const o = t().org.get(id);
      return o ? { id, login: o.login, type: 'Organization' } : { id, login: t().user.get(id)?.login ?? '?', type: 'User' };
    });
    return { status: 200, body: { projects, owners } };
  });
  R('GET', '/_bgh/projects/:id', (ctx) => {
    const p = projectById(ctx);
    return isResp(p) ? p : { status: 200, body: snapshot(p) };
  });

  // ---------------- projects
  R('POST', '/_bgh/projects', (ctx) => {
    const owner = ownerByLogin(String(ctx.body.owner ?? ''));
    if (!owner) return err(422, 'Validation Failed', [{ resource: 'Project', field: 'owner', code: 'invalid' }]);
    if (!owner.isOrg && owner.id !== s.db.viewerId) return err(403, 'Must have admin rights to create projects for this owner');
    const title = String(ctx.body.title ?? '').trim();
    if (!title || title.includes('fail!')) return err(422, 'Validation Failed', [{ resource: 'Project', field: 'title', code: 'missing_field' }]);
    const now = s.now();
    const number = [...t().project.values()].filter((p) => p.ownerId === owner.id).reduce((m, p) => Math.max(m, p.number), 0) + 1;
    const p: Project = {
      id: s.nextId(),
      ownerId: owner.id,
      number,
      title,
      shortDescription: (ctx.body.shortDescription as string | undefined) || null,
      readme: null,
      public: !!ctx.body.public,
      closed: false,
      closedAt: null,
      creatorId: s.db.viewerId,
      linkedRepoIds: [],
      createdAt: now,
      updatedAt: now,
    };
    s.put('project', p);
    const sc = scaffold(ids, p, now);
    for (const f of sc.fields) s.put('projectField', f);
    s.put('projectView', sc.view);
    for (const w of sc.workflows) s.put('projectWorkflow', w);
    return { status: 201, body: p };
  });
  R('PATCH', '/_bgh/projects/:id', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const b = ctx.body;
    if (typeof b.title === 'string' && (!b.title.trim() || b.title.includes('fail!')))
      return err(422, 'Validation Failed', [{ resource: 'Project', field: 'title', code: 'invalid' }]);
    const next: Project = { ...p, updatedAt: s.now() };
    if (typeof b.title === 'string') next.title = b.title.trim();
    if ('shortDescription' in b) next.shortDescription = (b.shortDescription as string | null) || null;
    if ('readme' in b) next.readme = (b.readme as string | null) ?? null;
    if (typeof b.public === 'boolean') next.public = b.public;
    if (typeof b.closed === 'boolean' && b.closed !== p.closed) {
      next.closed = b.closed;
      next.closedAt = b.closed ? s.now() : null;
    }
    s.put('project', next);
    return { status: 200, body: next };
  });
  R('DELETE', '/_bgh/projects/:id', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    for (const m of ['projectItem', 'projectView', 'projectField', 'projectWorkflow'] as const) {
      for (const r of rows(t()[m] as Map<ID, { id: ID; projectId: ID }>, p.id)) s.remove(m, r.id);
    }
    s.remove('project', p.id);
    return { status: 204 };
  });
  const link = (ctx: Ctx, on: boolean) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const repoId = Number(ctx.m[2]);
    if (!t().repo.has(repoId)) return err(404, 'Not Found');
    const next = { ...p, linkedRepoIds: on ? [...new Set([...p.linkedRepoIds, repoId])] : p.linkedRepoIds.filter((x) => x !== repoId), updatedAt: s.now() };
    s.put('project', next);
    return { status: 200, body: next };
  };
  R('PUT', '/_bgh/projects/:id/repos/:repoId', (ctx) => link(ctx, true));
  R('DELETE', '/_bgh/projects/:id/repos/:repoId', (ctx) => link(ctx, false));

  // ---------------- fields
  const normalizeOptions = (raw: unknown, salt: string): ProjectFieldOption[] | null => {
    if (!Array.isArray(raw)) return null;
    return raw.map((o: Partial<ProjectFieldOption>) => ({
      id: typeof o.id === 'string' && o.id ? o.id : optionId(salt),
      name: String(o.name ?? ''),
      color: String(o.color ?? 'GRAY'),
      description: String(o.description ?? ''),
    }));
  };
  const normalizeIterations = (raw: unknown): ProjectIterationConfig | null => {
    if (!raw || typeof raw !== 'object') return null;
    const r = raw as Partial<ProjectIterationConfig>;
    const duration = Number(r.duration ?? 14);
    const startDate = String(r.startDate ?? ymd(Date.now()));
    let list = (r.iterations ?? []).map((it) => ({
      id: it.id || optionId('it'),
      title: String(it.title),
      startDate: String(it.startDate),
      duration: Number(it.duration ?? duration),
    }));
    if (!list.length) {
      const start = Date.parse(`${startDate}T00:00:00Z`);
      list = [0, 1, 2].map((i) => ({ id: optionId('it'), title: `Iteration ${i + 1}`, startDate: ymd(start + i * duration * DAY), duration }));
    }
    return { startDate, duration, iterations: list };
  };
  R('POST', '/_bgh/projects/:id/fields', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const name = String(ctx.body.name ?? '').trim();
    const dataType = ctx.body.dataType as ProjectFieldType;
    if (!name || name.includes('fail!')) return err(422, 'Validation Failed', [{ resource: 'ProjectField', field: 'name', code: 'missing_field' }]);
    if (!['text', 'number', 'date', 'single_select', 'iteration'].includes(dataType))
      return err(422, 'Validation Failed', [{ resource: 'ProjectField', field: 'dataType', code: 'invalid' }]);
    const existing = rows(t().projectField, p.id);
    if (existing.some((f) => f.name.toLowerCase() === name.toLowerCase()))
      return err(422, 'Validation Failed', [{ resource: 'ProjectField', field: 'name', code: 'already_exists' }]);
    const now = s.now();
    const f: ProjectField = {
      id: s.nextId(),
      projectId: p.id,
      name,
      dataType,
      position: existing.reduce((m, x) => Math.max(m, x.position), 0) + 1,
      options: dataType === 'single_select' ? (normalizeOptions(ctx.body.options, name) ?? []) : null,
      iterations: dataType === 'iteration' ? normalizeIterations(ctx.body.iterations ?? {}) : null,
      createdAt: now,
      updatedAt: now,
    };
    s.put('projectField', f);
    touch(p);
    return { status: 201, body: f };
  });
  R('PATCH', '/_bgh/projects/:id/fields/:fieldId', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const f = t().projectField.get(Number(ctx.m[2]));
    if (!f || f.projectId !== p.id) return err(404, 'Not Found');
    const next: ProjectField = { ...f, updatedAt: s.now() };
    if (typeof ctx.body.name === 'string') {
      if (!ctx.body.name.trim() || ctx.body.name.includes('fail!'))
        return err(422, 'Validation Failed', [{ resource: 'ProjectField', field: 'name', code: 'invalid' }]);
      next.name = ctx.body.name.trim();
    }
    if (ctx.body.options && (f.dataType === 'single_select' || f.dataType === 'status')) next.options = normalizeOptions(ctx.body.options, f.name);
    if (ctx.body.iterations && f.dataType === 'iteration') next.iterations = normalizeIterations(ctx.body.iterations);
    if (typeof ctx.body.position === 'number') next.position = ctx.body.position;
    s.put('projectField', next);
    // Values pointing at removed options/iterations are cleared.
    const valid = new Set([...(next.options ?? []).map((o) => o.id), ...(next.iterations?.iterations ?? []).map((i) => i.id)]);
    if (next.options || next.iterations) {
      for (const item of rows(t().projectItem, p.id)) {
        const v = item.values[f.id];
        if (v !== undefined && !valid.has(String(v))) {
          const values = { ...item.values };
          delete values[f.id];
          s.put('projectItem', { ...item, values });
        }
      }
    }
    return { status: 200, body: next };
  });
  R('DELETE', '/_bgh/projects/:id/fields/:fieldId', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const f = t().projectField.get(Number(ctx.m[2]));
    if (!f || f.projectId !== p.id) return err(404, 'Not Found');
    if (BUILTIN.includes(f.dataType)) return err(422, 'Built-in fields cannot be deleted');
    s.remove('projectField', f.id);
    for (const item of rows(t().projectItem, p.id)) {
      if (item.values[f.id] === undefined) continue;
      const values = { ...item.values };
      delete values[f.id];
      s.put('projectItem', { ...item, values });
    }
    for (const v of rows(t().projectView, p.id)) {
      if (
        v.visibleFieldIds.includes(f.id) ||
        v.groupByFieldId === f.id ||
        v.columnFieldId === f.id ||
        v.dateFieldId === f.id ||
        v.sortBy.some((x) => x.fieldId === f.id)
      ) {
        s.put('projectView', {
          ...v,
          visibleFieldIds: v.visibleFieldIds.filter((x) => x !== f.id),
          groupByFieldId: v.groupByFieldId === f.id ? null : v.groupByFieldId,
          columnFieldId: v.columnFieldId === f.id ? null : v.columnFieldId,
          dateFieldId: v.dateFieldId === f.id ? null : v.dateFieldId,
          sortBy: v.sortBy.filter((x) => x.fieldId !== f.id),
        });
      }
    }
    return { status: 204 };
  });

  // ---------------- items
  const applyWorkflowStatus = (p: Project, item: ProjectItem, kind: ProjectWorkflowKind): ProjectItem => {
    const wf = rows(t().projectWorkflow, p.id).find((w) => w.kind === kind && w.enabled);
    const status = rows(t().projectField, p.id).find((f) => f.dataType === 'status');
    if (!wf?.config.statusOptionId || !status || item.values[status.id] !== undefined) return item;
    return { ...item, values: { ...item.values, [status.id]: wf.config.statusOptionId } };
  };
  const validateValues = (p: Project, values: Record<string, unknown>): string | null => {
    const fields = rows(t().projectField, p.id);
    for (const [fid, v] of Object.entries(values)) {
      const f = fields.find((x) => String(x.id) === fid);
      if (!f) return `Unknown field ${fid}`;
      if (v === null) continue;
      if (f.dataType === 'text' && (typeof v !== 'string' || v.includes('fail!'))) return 'Invalid text value';
      if (f.dataType === 'number' && typeof v !== 'number') return 'Invalid number';
      if (f.dataType === 'date' && !/^\d{4}-\d{2}-\d{2}$/.test(String(v))) return 'Invalid date';
      if ((f.dataType === 'single_select' || f.dataType === 'status') && !f.options?.some((o) => o.id === v)) return 'Invalid option';
      if (f.dataType === 'iteration' && !f.iterations?.iterations.some((i) => i.id === v)) return 'Invalid iteration';
      if (BUILTIN.includes(f.dataType) && f.dataType !== 'status') return `${f.name} cannot be set on items`;
    }
    return null;
  };
  R('POST', '/_bgh/projects/:id/items', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const b = ctx.body;
    const items = rows(t().projectItem, p.id);
    const last = items.reduce<string | null>((m, i) => (m === null || compareKeys(i.position, m) > 0 ? i.position : m), null);
    const position = typeof b.position === 'string' && b.position ? b.position : generateKeyBetween(last, null);
    const now = s.now();
    let issue: Issue | undefined;
    if (b.issueId != null) issue = t().issue.get(Number(b.issueId));
    else if (b.owner && b.repo && b.number) {
      const repo = s.repo(String(b.owner), String(b.repo));
      issue = repo && s.issue(repo, Number(b.number));
    }
    const draft = b.draft as { title?: string; body?: string } | undefined;
    if (!issue && !draft) return err(422, 'Validation Failed', [{ resource: 'ProjectItem', field: 'content', code: 'invalid', message: 'Issue not found' }]);
    if (issue) {
      const existing = items.find((i) => i.issueId === issue.id);
      if (existing) return { status: 200, body: existing };
    }
    if (draft && (!String(draft.title ?? '').trim() || String(draft.title).includes('fail!')))
      return err(422, 'Validation Failed', [{ resource: 'ProjectItem', field: 'title', code: 'missing_field' }]);
    let item: ProjectItem = {
      id: s.nextId(),
      projectId: p.id,
      contentType: issue ? (issue.isPr ? 'PullRequest' : 'Issue') : 'DraftIssue',
      issueId: issue?.id ?? null,
      title: issue ? null : String(draft!.title).trim(),
      body: issue ? null : (draft!.body ?? null),
      assigneeIds: [],
      archived: false,
      position,
      viewPositions: {},
      values: {},
      creatorId: s.db.viewerId,
      createdAt: now,
      updatedAt: now,
    };
    item = applyWorkflowStatus(p, item, 'item_added');
    s.put('projectItem', item, { includeLazy: true });
    touch(p);
    return { status: 201, body: item };
  });
  R('PATCH', '/_bgh/projects/:id/items/:itemId', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const item = t().projectItem.get(Number(ctx.m[2]));
    if (!item || item.projectId !== p.id) return err(404, 'Not Found');
    const b = ctx.body;
    const next: ProjectItem = { ...item, updatedAt: s.now() };
    if (typeof b.archived === 'boolean') next.archived = b.archived;
    if (typeof b.position === 'string') next.position = b.position;
    if (b.viewId != null && typeof b.viewPosition === 'string') next.viewPositions = { ...item.viewPositions, [String(b.viewId)]: b.viewPosition };
    const isDraft = item.contentType === 'DraftIssue';
    if (typeof b.title === 'string') {
      if (!isDraft) return err(422, 'Only drafts have a title on the item');
      if (!b.title.trim() || b.title.includes('fail!')) return err(422, 'Validation Failed', [{ resource: 'ProjectItem', field: 'title', code: 'invalid' }]);
      next.title = b.title.trim();
    }
    const bodyChanged = 'body' in b && isDraft;
    if (bodyChanged) next.body = (b.body as string | null) ?? null;
    if (Array.isArray(b.assigneeIds) && isDraft) next.assigneeIds = b.assigneeIds as ID[];
    if (b.values && typeof b.values === 'object') {
      const problem = validateValues(p, b.values as Record<string, unknown>);
      if (problem) return err(422, problem);
      const values = { ...item.values };
      for (const [k, v] of Object.entries(b.values as Record<string, ProjectValue | null>)) {
        if (v === null) delete values[k];
        else values[k] = v;
      }
      next.values = values;
    }
    s.put('projectItem', next, { includeLazy: bodyChanged });
    return { status: 200, body: next };
  });
  R('DELETE', '/_bgh/projects/:id/items/:itemId', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const item = t().projectItem.get(Number(ctx.m[2]));
    if (!item || item.projectId !== p.id) return err(404, 'Not Found');
    s.remove('projectItem', item.id);
    return { status: 204 };
  });

  // ---------------- views
  const VIEW_KEYS = [
    'name',
    'layout',
    'position',
    'filter',
    'groupByFieldId',
    'columnFieldId',
    'sortBy',
    'visibleFieldIds',
    'hiddenColumnIds',
    'dateFieldId',
  ] as const;
  R('POST', '/_bgh/projects/:id/views', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const views = rows(t().projectView, p.id);
    const fields = rows(t().projectField, p.id);
    const now = s.now();
    const v: ProjectView = {
      id: s.nextId(),
      projectId: p.id,
      number: views.reduce((m, x) => Math.max(m, x.number), 0) + 1,
      name: `View ${views.length + 1}`,
      layout: 'table',
      position: views.reduce((m, x) => Math.max(m, x.position), 0) + 1,
      filter: '',
      groupByFieldId: null,
      columnFieldId: null,
      sortBy: [],
      visibleFieldIds: fields.filter((f) => ['title', 'assignees', 'status'].includes(f.dataType)).map((f) => f.id),
      hiddenColumnIds: [],
      dateFieldId: null,
      createdAt: now,
      updatedAt: now,
    };
    for (const k of VIEW_KEYS) if (k in ctx.body) (v as unknown as Record<string, unknown>)[k] = ctx.body[k];
    if (String(v.name).includes('fail!')) return err(422, 'Validation Failed', [{ resource: 'ProjectView', field: 'name', code: 'invalid' }]);
    if (v.layout === 'board' && v.columnFieldId == null) v.columnFieldId = fields.find((f) => f.dataType === 'status')?.id ?? null;
    s.put('projectView', v);
    return { status: 201, body: v };
  });
  R('PATCH', '/_bgh/projects/:id/views/:viewId', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const v = t().projectView.get(Number(ctx.m[2]));
    if (!v || v.projectId !== p.id) return err(404, 'Not Found');
    const next = { ...v, updatedAt: s.now() } as ProjectView;
    for (const k of VIEW_KEYS) if (k in ctx.body) (next as unknown as Record<string, unknown>)[k] = ctx.body[k];
    if (!String(next.name).trim() || String(next.name).includes('fail!'))
      return err(422, 'Validation Failed', [{ resource: 'ProjectView', field: 'name', code: 'invalid' }]);
    if (next.layout === 'board' && next.columnFieldId == null)
      next.columnFieldId = rows(t().projectField, p.id).find((f) => f.dataType === 'status')?.id ?? null;
    s.put('projectView', next);
    return { status: 200, body: next };
  });
  R('DELETE', '/_bgh/projects/:id/views/:viewId', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const v = t().projectView.get(Number(ctx.m[2]));
    if (!v || v.projectId !== p.id) return err(404, 'Not Found');
    if (rows(t().projectView, p.id).length <= 1) return err(422, 'A project needs at least one view');
    s.remove('projectView', v.id);
    return { status: 204 };
  });

  // ---------------- workflows
  R('PUT', '/_bgh/projects/:id/workflows/:kind', (ctx) => {
    const p = projectById(ctx);
    if (isResp(p)) return p;
    const kind = ctx.m[2] as ProjectWorkflowKind;
    if (!WORKFLOW_KINDS.includes(kind)) return err(404, 'Not Found');
    const existing = rows(t().projectWorkflow, p.id).find((w) => w.kind === kind);
    const w: ProjectWorkflow = {
      id: existing?.id ?? s.nextId(),
      projectId: p.id,
      kind,
      enabled: !!ctx.body.enabled,
      config: (ctx.body.config as ProjectWorkflow['config'] | undefined) ?? existing?.config ?? {},
      updatedAt: s.now(),
    };
    s.put('projectWorkflow', w);
    return { status: 200, body: w };
  });
}

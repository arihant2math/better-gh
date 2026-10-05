/* Deterministic, realistic seed data for the mock backend. */
import type {
  Comment,
  ID,
  Issue,
  IssueEvent,
  Label,
  Membership,
  Milestone,
  ModelMap,
  ModelName,
  Notification,
  Org,
  Repo,
  Review,
  Team,
  User,
  ViewerRepo,
} from '../sync/models';
import { MODEL_NAMES } from '../sync/schema';
import { seedProjects } from './projects';
import { Rng, fakeSha, iso } from './rng';

export type Tables = { [M in ModelName]: Map<ID, ModelMap[M]> };

export interface MockDb {
  viewerId: ID;
  tables: Tables;
  nextId: number;
  nextNumber: Record<ID, number>;
}

export function emptyTables(): Tables {
  const t = {} as Record<ModelName, Map<ID, unknown>>;
  for (const m of MODEL_NAMES) t[m] = new Map();
  return t as Tables;
}

const PEOPLE: [string, string][] = [
  ['ada', 'Ada Lovelace'],
  ['grace', 'Grace Hopper'],
  ['linus', 'Linus Torvalds'],
  ['margaret', 'Margaret Hamilton'],
  ['alan', 'Alan Turing'],
  ['barbara', 'Barbara Liskov'],
  ['ken', 'Ken Thompson'],
  ['dennis', 'Dennis Ritchie'],
  ['guido', 'Guido van Rossum'],
  ['brendan', 'Brendan Eich'],
  ['matz', 'Yukihiro Matsumoto'],
  ['rich', 'Rich Hickey'],
  ['donald', 'Donald Knuth'],
  ['edsger', 'Edsger Dijkstra'],
  ['carmack', 'John Carmack'],
  ['sophie', 'Sophie Wilson'],
  ['radia', 'Radia Perlman'],
  ['frances', 'Frances Allen'],
  ['lamport', 'Leslie Lamport'],
  ['timbl', 'Tim Berners-Lee'],
  ['hedy', 'Hedy Lamarr'],
  ['katherine', 'Katherine Johnson'],
  ['annie', 'Annie Easley'],
];

const ORGS: [string, string, string][] = [
  ['acme', 'Acme Corp', 'Building the boring infrastructure behind exciting products.'],
  ['nebula-labs', 'Nebula Labs', 'Research-grade systems software, production-grade quality.'],
  ['openfield', 'Openfield Collective', 'Open source UI primitives, maintained in the open.'],
];

const REPOS: { owner: string; name: string; lang: string; desc: string; priv?: boolean; issues: number }[] = [
  { owner: 'acme', name: 'api', lang: 'Rust', desc: 'Core HTTP API for Acme services', issues: 150 },
  { owner: 'acme', name: 'web', lang: 'TypeScript', desc: 'Customer-facing web application', issues: 120 },
  { owner: 'acme', name: 'infra', lang: 'Go', desc: 'Deploy tooling, Terraform modules and runbooks', priv: true, issues: 30 },
  { owner: 'acme', name: 'design-system', lang: 'TypeScript', desc: 'Tokens, components and guidelines for Acme UIs', issues: 45 },
  { owner: 'nebula-labs', name: 'quark', lang: 'Rust', desc: 'A tiny, fast embedded key-value store', issues: 110 },
  { owner: 'nebula-labs', name: 'orbit', lang: 'Go', desc: 'Distributed job scheduler with exactly-once semantics', issues: 80 },
  { owner: 'nebula-labs', name: 'notebooks', lang: 'Python', desc: 'Research notebooks and analysis scripts', priv: true, issues: 12 },
  { owner: 'openfield', name: 'fieldkit', lang: 'TypeScript', desc: 'Composable, accessible form primitives for React', issues: 75 },
  { owner: 'openfield', name: 'docs', lang: 'TypeScript', desc: 'Documentation site for Openfield projects', issues: 22 },
  { owner: 'ada', name: 'dotfiles', lang: 'Shell', desc: 'zsh, git and tmux configuration', issues: 4 },
  { owner: 'ada', name: 'advent-of-code', lang: 'Rust', desc: 'Solutions, mostly fast, occasionally clever', issues: 3 },
];

const DEFAULT_LABELS: [string, string, string][] = [
  ['bug', 'd73a4a', "Something isn't working"],
  ['documentation', '0075ca', 'Improvements or additions to documentation'],
  ['duplicate', 'cfd3d7', 'This issue or pull request already exists'],
  ['enhancement', 'a2eeef', 'New feature or request'],
  ['good first issue', '7057ff', 'Good for newcomers'],
  ['help wanted', '008672', 'Extra attention is needed'],
  ['question', 'd876e3', 'Further information is requested'],
  ['wontfix', 'ffffff', 'This will not be worked on'],
  ['performance', 'fbca04', 'Latency, throughput or memory'],
  ['security', 'b60205', 'Security-sensitive change'],
  ['priority: high', 'e99695', null as unknown as string],
  ['needs triage', 'ededed', 'Not looked at yet'],
  ['dependencies', '0366d6', 'Pull requests that update a dependency file'],
  ['area: api', '1d76db', null as unknown as string],
  ['area: ui', '5319e7', null as unknown as string],
  ['breaking change', 'f9d0c4', 'Requires a major version bump'],
];

const COMPONENTS = ['router', 'session store', 'auth middleware', 'query planner', 'cache layer', 'webhook dispatcher', 'rate limiter', 'scheduler', 'file watcher', 'diff renderer', 'search index', 'migration runner', 'connection pool', 'theme provider', 'date picker', 'command palette', 'tokenizer', 'job queue', 'blob store', 'notification service', 'CLI', 'config loader', 'retry policy', 'metrics exporter'];
const FEATURES = ['pagination cursors', 'dark mode', 'OAuth device flow', 'SAML single sign-on', 'keyboard shortcuts', 'bulk editing', 'CSV export', 'webhook retries', 'ETag caching', 'streaming responses', 'HTTP/3', 'brotli compression', 'partial indexes', 'saved filters', 'audit logging', 'multi-region failover', 'i18n', 'drag and drop reordering', 'structured logging', 'graceful shutdown', 'read replicas', 'WebAuthn'];
const CONDITIONS = ['the cache is cold', 'the request has no body', 'two clients reconnect at once', 'the config file is missing', 'running on ARM', 'the locale is tr-TR', 'the payload exceeds 1 MB', 'the clock skews backwards', 'the user has no email', 'a migration is interrupted'];
const DEPS = ['serde', 'tokio', 'react', 'vite', 'typescript', 'axum', 'eslint', 'golang.org/x/net', 'hyper', 'rustls', 'zod', 'vitest'];

const BUG_TITLES = [
  (r: Rng) => `${cap(r.pick(COMPONENTS))} panics when ${r.pick(CONDITIONS)}`,
  (r: Rng) => `Memory leak in ${r.pick(COMPONENTS)}`,
  (r: Rng) => `Race condition in ${r.pick(COMPONENTS)} under load`,
  (r: Rng) => `500 error from /v1/${r.pick(['items', 'users', 'search', 'events', 'billing'])} when ${r.pick(CONDITIONS)}`,
  (r: Rng) => `${cap(r.pick(COMPONENTS))} ignores timeout setting`,
  (r: Rng) => `Flaky test: ${r.pick(COMPONENTS).replace(/ /g, '_')}::handles_${r.pick(['reconnect', 'shutdown', 'overflow', 'unicode'])}`,
  (r: Rng) => `Wrong timezone in ${r.pick(['digest emails', 'audit log', 'CSV export', 'activity feed'])}`,
  (r: Rng) => `Focus is lost after closing the ${r.pick(['dialog', 'dropdown', 'command palette', 'date picker'])}`,
];
const FEATURE_TITLES = [
  (r: Rng) => `Add support for ${r.pick(FEATURES)}`,
  (r: Rng) => `Implement ${r.pick(FEATURES)} in the ${r.pick(COMPONENTS)}`,
  (r: Rng) => `Expose ${r.pick(COMPONENTS)} metrics via Prometheus`,
  (r: Rng) => `Allow configuring ${r.pick(COMPONENTS)} per tenant`,
  (r: Rng) => `RFC: ${r.pick(FEATURES)}`,
  (r: Rng) => `Make ${r.pick(COMPONENTS)} pluggable`,
];
const PR_TITLES = [
  (r: Rng) => `Fix ${r.pick(COMPONENTS)} deadlock on shutdown`,
  (r: Rng) => `Add ${r.pick(FEATURES)}`,
  (r: Rng) => `Refactor ${r.pick(COMPONENTS)} to avoid extra allocations`,
  (r: Rng) => `Speed up ${r.pick(COMPONENTS)} by ${r.int(12, 70)}%`,
  (r: Rng) => `docs: explain ${r.pick(FEATURES)}`,
  (r: Rng) => `ci: cache ${r.pick(['cargo registry', 'node_modules', 'go build cache'])} between runs`,
  (r: Rng) => `Remove deprecated ${r.pick(COMPONENTS)} options`,
];

const COMMENT_BODIES = [
  'I can reproduce this on `main` as of this morning.',
  'Thanks for the report! Could you share the full stack trace?',
  'This looks related to #{n}.',
  'LGTM :rocket:',
  "I'll take a look at this later this week.",
  'Bumping the priority — two customers hit this yesterday.',
  "Here's a minimal repro:\n\n```rust\nlet pool = Pool::new(cfg);\nlet a = pool.get().await?;\ndrop(pool); // panics here\n```",
  'Would a feature flag be acceptable for the first iteration?',
  'Benchmarks before/after:\n\n| case | before | after |\n|------|-------:|------:|\n| cold | 41 ms | 12 ms |\n| warm | 3.1 ms | 0.9 ms |',
  '> Could we avoid the extra allocation?\n\nYes — pushed a fixup.',
  'Closing in favor of the approach in #{n}.',
  'Agreed. Let us keep the scope small and follow up separately.',
  'Tested locally, works for me. :+1:',
  '- [x] tests\n- [x] docs\n- [ ] changelog entry',
];

const BODY_TEMPLATES = [
  (t: string) => `### Describe the bug\n\n${t}.\n\n### Steps to reproduce\n\n1. Start the server with default config\n2. Send a few concurrent requests\n3. Observe the logs\n\n### Expected behavior\n\nNo errors, requests succeed.\n\n### Environment\n\n- OS: Linux 6.8\n- Version: \`0.4.2\``,
  (t: string) => `## Motivation\n\n${t} would remove a lot of boilerplate for users.\n\n## Proposal\n\n- Add a new option to the config\n- Keep the old behavior as default\n- Document the migration\n\n\`\`\`ts\nconst client = createClient({ retries: 3 });\n\`\`\``,
  (t: string) => `${t}.\n\nThis came up while working on the Q3 roadmap. See the discussion in the design doc.`,
  (t: string) => `## Summary\n\n${t}.\n\n## Changes\n\n- Split the hot path into a separate function\n- Added tests for edge cases\n- Updated the docs\n\n## Test plan\n\n- [x] Unit tests\n- [x] Ran the benchmark suite\n- [ ] Tested on staging`,
];

function cap(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

export function seed(now = Date.now()): MockDb {
  const rng = new Rng(20261005);
  const t = emptyTables();
  let nextId = 1;
  const id = () => nextId++;
  const DAY = 86_400_000;
  const ago = (maxDays: number, minDays = 0) => now - Math.floor((minDays + rng.next() * (maxDays - minDays)) * DAY);

  // users
  const users: User[] = PEOPLE.map(([login, name]) => ({ id: id(), login, name, avatarUrl: '', type: 'User' }));
  const bots: User[] = ['dependabot[bot]', 'github-actions[bot]'].map((login) => ({ id: id(), login, name: null, avatarUrl: '', type: 'Bot' }));
  for (const u of [...users, ...bots]) t.user.set(u.id, u);
  const viewer = users[0]!;
  const humans = users;

  // orgs, memberships, teams
  const orgs: Org[] = ORGS.map(([login, name, description]) => ({ id: id(), login, name, avatarUrl: '', description }));
  for (const o of orgs) {
    t.org.set(o.id, o);
    const members = [viewer, ...rng.sample(humans.slice(1), rng.int(8, 14))];
    for (const [i, m] of members.entries()) {
      const ms: Membership = { id: id(), orgId: o.id, userId: m.id, role: i < 2 ? 'admin' : 'member' };
      t.membership.set(ms.id, ms);
    }
    const teamNames = o.login === 'acme' ? ['Core', 'Frontend', 'Infra'] : o.login === 'nebula-labs' ? ['Research', 'Platform'] : ['Maintainers'];
    for (const tn of teamNames) {
      const team: Team = {
        id: id(),
        orgId: o.id,
        slug: tn.toLowerCase(),
        name: tn,
        description: `${tn} team`,
        privacy: 'closed',
        parentId: null,
        memberIds: rng.sample(members, rng.int(3, 6)).map((m) => m.id),
        repoIds: [],
      };
      t.team.set(team.id, team);
    }
  }

  const ownerOf = (login: string) => orgs.find((o) => o.login === login) ?? users.find((u) => u.login === login)!;
  const membersOf = (ownerId: ID): User[] => {
    const ids = [...t.membership.values()].filter((m) => m.orgId === ownerId).map((m) => m.userId);
    return ids.length ? ids.map((i) => t.user.get(i)!) : [viewer];
  };

  const nextNumber: Record<ID, number> = {};
  let notifCount = 0;

  for (const spec of REPOS) {
    const owner = ownerOf(spec.owner);
    const createdAt = ago(900, 420);
    const repo: Repo = {
      id: id(),
      ownerId: owner.id,
      owner: owner.login,
      name: spec.name,
      description: spec.desc,
      private: !!spec.priv,
      fork: false,
      archived: false,
      defaultBranch: 'main',
      language: spec.lang,
      topics: spec.lang === 'Rust' ? ['rust', 'performance'] : spec.lang === 'TypeScript' ? ['typescript', 'react'] : [spec.lang.toLowerCase()],
      stars: spec.priv ? rng.int(0, 5) : rng.int(40, 4800),
      forks: rng.int(0, 300),
      watchers: rng.int(3, 120),
      openIssues: 0,
      openPulls: 0,
      hasIssues: true,
      hasProjects: true,
      hasWiki: !spec.priv,
      pushedAt: iso(ago(3)),
      createdAt: iso(createdAt),
      updatedAt: iso(ago(5)),
    };
    t.repo.set(repo.id, repo);
    const isOwn = owner.id === viewer.id;
    const vr: ViewerRepo = {
      id: repo.id,
      permission: isOwn || owner.login === 'acme' ? 'admin' : owner.login === 'nebula-labs' ? 'write' : 'triage',
      starred: rng.chance(0.4),
      watching: 'subscribed',
    };
    t.viewerRepo.set(vr.id, vr);

    // labels & milestones
    const labels: Label[] = DEFAULT_LABELS.slice(0, isOwn ? 9 : DEFAULT_LABELS.length).map(([name, color, description]) => ({
      id: id(),
      repoId: repo.id,
      name,
      color,
      description: description ?? null,
    }));
    for (const l of labels) t.label.set(l.id, l);
    const byName = (n: string) => labels.find((l) => l.name === n);
    const milestones: Milestone[] = (isOwn ? [] : ['v1.0', 'v1.1', 'v2.0', 'Q4 2026']).map((title, i) => ({
      id: id(),
      repoId: repo.id,
      number: i + 1,
      title,
      description: i === 3 ? 'Quarterly goals' : `Release ${title}`,
      state: i === 0 ? 'closed' : 'open',
      dueOn: iso(now + (i - 1) * 30 * DAY),
      openIssues: 0,
      closedIssues: 0,
      createdAt: iso(ago(300, 200)),
      updatedAt: iso(ago(20)),
      closedAt: i === 0 ? iso(ago(60, 30)) : null,
    }));
    for (const m of milestones) t.milestone.set(m.id, m);

    const team = membersOf(owner.id);
    let number = 0;
    for (let k = 0; k < spec.issues; k++) {
      number += 1;
      const isPr = rng.chance(0.32);
      const isDep = isPr && rng.chance(0.15);
      const kind = isPr ? 'pr' : rng.chance(0.5) ? 'bug' : 'feature';
      const title = isDep
        ? `Bump ${rng.pick(DEPS)} from ${rng.int(1, 4)}.${rng.int(0, 20)}.${rng.int(0, 9)} to ${rng.int(4, 6)}.${rng.int(0, 9)}.0`
        : kind === 'pr'
          ? rng.pick(PR_TITLES)(rng)
          : kind === 'bug'
            ? rng.pick(BUG_TITLES)(rng)
            : rng.pick(FEATURE_TITLES)(rng);
      // newer issues have higher numbers: spread createdAt ascending
      const created = createdAt + ((now - createdAt) * (k + rng.next() * 0.8)) / spec.issues;
      const author = isDep ? bots[0]! : rng.chance(0.12) ? viewer : rng.pick(team);
      const closed = rng.chance(number < spec.issues * 0.6 ? 0.65 : 0.25);
      const closedAtMs = closed ? Math.min(now - 3600_000, created + rng.next() * 40 * DAY) : null;
      const updated = Math.max(created, Math.min(now - rng.int(60, 3600) * 1000, (closedAtMs ?? created) + rng.next() * 10 * DAY));
      const labelIds: ID[] = [];
      if (isDep) labelIds.push(byName('dependencies')!.id);
      else if (kind === 'bug') labelIds.push(byName('bug')!.id);
      else if (kind === 'feature') labelIds.push(byName('enhancement')!.id);
      for (const l of rng.sample(labels.slice(4), rng.int(0, 2))) if (!labelIds.includes(l.id)) labelIds.push(l.id);
      const assigneeIds: ID[] = [];
      if (!closed && rng.chance(0.2)) assigneeIds.push(viewer.id);
      if (rng.chance(0.5)) {
        const u = rng.pick(team).id;
        if (!assigneeIds.includes(u)) assigneeIds.push(u);
      }
      const milestone = milestones.length && rng.chance(0.35) ? rng.pick(milestones) : null;
      const issue: Issue = {
        id: id(),
        repoId: repo.id,
        number,
        title,
        body: isPr ? BODY_TEMPLATES[3]!(title) : rng.pick(BODY_TEMPLATES.slice(0, 3))(title),
        state: closed ? 'closed' : 'open',
        stateReason: closed ? (rng.chance(0.85) ? 'completed' : 'not_planned') : null,
        authorId: author.id,
        assigneeIds,
        labelIds,
        milestoneId: milestone?.id ?? null,
        comments: 0,
        locked: false,
        createdAt: iso(created),
        updatedAt: iso(updated),
        closedAt: closedAtMs ? iso(closedAtMs) : null,
        isPr,
      };
      if (milestone) {
        if (closed) milestone.closedIssues++;
        else milestone.openIssues++;
      }
      if (isPr) {
        const merged = closed && rng.chance(0.75);
        const reviewers = rng.sample(team.filter((u) => u.id !== author.id), rng.int(0, 2)).map((u) => u.id);
        if (!closed && author.id !== viewer.id && rng.chance(0.3) && !reviewers.includes(viewer.id)) reviewers.push(viewer.id);
        const branch = isDep ? `dependabot/${spec.lang.toLowerCase()}/${title.split(' ')[1]}` : `${author.login}/${title.toLowerCase().replace(/[^a-z0-9]+/g, '-').slice(0, 32).replace(/-$/, '')}`;
        Object.assign(issue, {
          draft: !closed && rng.chance(0.15),
          merged,
          mergedAt: merged ? issue.closedAt : null,
          mergedById: merged ? rng.pick(team).id : null,
          headRef: branch,
          headRepoId: repo.id,
          headSha: fakeSha(`${repo.id}:${number}:head`),
          baseRef: 'main',
          baseSha: fakeSha(`${repo.id}:${number}:base`),
          mergeable: closed ? null : rng.chance(0.85),
          mergeableState: closed ? 'unknown' : rng.pick(['clean', 'clean', 'clean', 'blocked', 'dirty', 'unstable'] as const),
          reviewDecision: closed ? (merged ? 'approved' : null) : rng.pick(['approved', 'review_required', 'review_required', 'changes_requested'] as const),
          requestedReviewerIds: closed ? [] : reviewers,
          requestedTeamIds: [],
          checks: closed ? 'success' : rng.pick(['success', 'success', 'success', 'failure', 'pending'] as const),
          additions: rng.int(1, 600),
          deletions: rng.int(0, 300),
          changedFiles: rng.int(1, 9),
          commits: rng.int(1, 8),
        } satisfies Partial<Issue>);
      }
      t.issue.set(issue.id, issue);

      // conversation
      const nComments = rng.int(0, rng.chance(0.2) ? 12 : 4);
      let ts = created;
      const participants = [author, ...rng.sample(team, 4)];
      for (let c = 0; c < nComments; c++) {
        ts = Math.min(updated, ts + rng.next() * 3 * DAY);
        const by = rng.pick(participants);
        const comment: Comment = {
          id: id(),
          repoId: repo.id,
          issueId: issue.id,
          authorId: by.id,
          body: rng.pick(COMMENT_BODIES).replace('{n}', String(rng.int(1, Math.max(1, number)))),
          authorAssociation: by.id === author.id ? 'CONTRIBUTOR' : 'MEMBER',
          reactions: rng.chance(0.3) ? { '+1': rng.int(1, 6), ...(rng.chance(0.3) ? { heart: rng.int(1, 3) } : {}) } : undefined,
          createdAt: iso(ts),
          updatedAt: iso(ts),
        };
        t.comment.set(comment.id, comment);
      }
      issue.comments = nComments;
      // timeline events
      const ev = (event: IssueEvent['event'], at: number, actor: ID | null, data: IssueEvent['data'] = {}) => {
        const e: IssueEvent = { id: id(), repoId: repo.id, issueId: issue.id, actorId: actor, event, data, createdAt: iso(at) };
        t.issueEvent.set(e.id, e);
      };
      for (const lid of labelIds.slice(0, 2)) {
        const l = t.label.get(lid)!;
        ev('labeled', created + 60_000, author.id, { labelId: lid, labelName: l.name, labelColor: l.color });
      }
      for (const a of assigneeIds) ev('assigned', created + 120_000, rng.pick(team).id, { assigneeId: a });
      if (issue.isPr && issue.merged) ev('merged', closedAtMs!, issue.mergedById!, { commitId: fakeSha(`m${issue.id}`) });
      else if (closedAtMs) ev('closed', closedAtMs, rng.pick(team).id, { stateReason: issue.stateReason ?? 'completed' });
      if (isPr) {
        const nReviews = closed ? rng.int(1, 2) : rng.int(0, 2);
        for (let r = 0; r < nReviews; r++) {
          const by = rng.pick(team.filter((u) => u.id !== author.id)) ?? viewer;
          const review: Review = {
            id: id(),
            repoId: repo.id,
            issueId: issue.id,
            authorId: by.id,
            state: issue.merged ? 'APPROVED' : rng.pick(['APPROVED', 'COMMENTED', 'CHANGES_REQUESTED'] as const),
            body: rng.chance(0.5) ? rng.pick(['Looks good to me.', 'A couple of nits, otherwise great.', 'Please add a test for the error path.', 'Nice cleanup!']) : '',
            commitId: issue.headSha!,
            submittedAt: iso(Math.min(updated, created + rng.next() * 2 * DAY)),
          };
          t.review.set(review.id, review);
        }
      }
      if (!closed) {
        if (isPr) repo.openPulls++;
        else repo.openIssues++;
      }

      // notifications for the viewer
      const involved = issue.assigneeIds.includes(viewer.id) || issue.requestedReviewerIds?.includes(viewer.id) || author.id === viewer.id;
      if ((involved && rng.chance(0.7)) || (notifCount < 60 && rng.chance(0.04))) {
        notifCount++;
        const n: Notification = {
          id: id(),
          repoId: repo.id,
          subjectType: isPr ? 'PullRequest' : 'Issue',
          subjectId: issue.id,
          title: issue.title,
          reason: issue.requestedReviewerIds?.includes(viewer.id)
            ? 'review_requested'
            : issue.assigneeIds.includes(viewer.id)
              ? 'assign'
              : author.id === viewer.id
                ? 'author'
                : rng.pick(['mention', 'subscribed', 'comment', 'team_mention'] as const),
          unread: rng.chance(0.55),
          updatedAt: issue.updatedAt,
          lastReadAt: null,
        };
        t.notification.set(n.id, n);
      }
    }
    nextNumber[repo.id] = number + 1;
  }

  const db: MockDb = { viewerId: viewer.id, tables: t, nextId, nextNumber };
  seedProjects(db, now);
  return db;
}

/** Pure data and form ⇄ API conversions for the repository settings pages. */
import type {
  BranchProtection,
  MergeMessage,
  MergeTitle,
  ProtectionInput,
  SquashMessage,
  SquashTitle,
} from '../../api/repoSettings';
import type { Permission } from '../../sync/models';

// ------------------------------------------------------------------ roles

export const ROLES: { value: Permission; label: string; description: string }[] = [
  { value: 'read', label: 'Read', description: 'Can read and clone this repository. Can also open and comment on issues and pull requests.' },
  { value: 'triage', label: 'Triage', description: 'Can read and clone this repository. Can also manage issues and pull requests.' },
  { value: 'write', label: 'Write', description: 'Can read, clone, and push to this repository. Can also manage issues and pull requests.' },
  { value: 'maintain', label: 'Maintain', description: 'Can read, clone, and push to this repository. They can also manage issues, pull requests, and some repository settings.' },
  { value: 'admin', label: 'Admin', description: 'Can read, clone, and push to this repository. Can also manage issues, pull requests, and repository settings, including adding collaborators.' },
];

export const roleLabel = (p: string) => ROLES.find((r) => r.value === p)?.label ?? p;

// ------------------------------------------------------------------ merge commit messages

export interface MessageOption<T, M> {
  id: string;
  label: string;
  title: T;
  message: M;
}

export const MERGE_MESSAGE_OPTIONS: MessageOption<MergeTitle, MergeMessage>[] = [
  { id: 'default', label: 'Default message', title: 'MERGE_MESSAGE', message: 'PR_TITLE' },
  { id: 'title', label: 'Pull request title', title: 'PR_TITLE', message: 'BLANK' },
  { id: 'title_body', label: 'Pull request title and description', title: 'PR_TITLE', message: 'PR_BODY' },
];

export const SQUASH_MESSAGE_OPTIONS: MessageOption<SquashTitle, SquashMessage>[] = [
  { id: 'default', label: 'Default message', title: 'COMMIT_OR_PR_TITLE', message: 'COMMIT_MESSAGES' },
  { id: 'title', label: 'Pull request title', title: 'PR_TITLE', message: 'BLANK' },
  { id: 'title_commits', label: 'Pull request title and commit details', title: 'PR_TITLE', message: 'COMMIT_MESSAGES' },
  { id: 'title_body', label: 'Pull request title and description', title: 'PR_TITLE', message: 'PR_BODY' },
];

export function messageOptionId<T, M>(opts: MessageOption<T, M>[], title: T, message: M): string {
  return opts.find((o) => o.title === title && o.message === message)?.id ?? opts[0]!.id;
}

// ------------------------------------------------------------------ webhooks

export const HOOK_EVENTS: { id: string; label: string; description: string }[] = [
  { id: 'branch_protection_rule', label: 'Branch protection rules', description: 'Branch protection rule created, deleted or edited.' },
  { id: 'check_run', label: 'Check runs', description: 'Check run is created, requested, rerequested, or completed.' },
  { id: 'check_suite', label: 'Check suites', description: 'Check suite is requested, rerequested, or completed.' },
  { id: 'commit_comment', label: 'Commit comments', description: 'Commit or diff commented on.' },
  { id: 'create', label: 'Branch or tag creation', description: 'Branch or tag created.' },
  { id: 'delete', label: 'Branch or tag deletion', description: 'Branch or tag deleted.' },
  { id: 'deploy_key', label: 'Deploy keys', description: 'A deploy key is created or deleted from a repository.' },
  { id: 'deployment', label: 'Deployments', description: 'Repository was deployed or a deployment was deleted.' },
  { id: 'deployment_status', label: 'Deployment statuses', description: 'Deployment status updated from the API.' },
  { id: 'discussion', label: 'Discussions', description: 'Discussion created, edited, pinned, unpinned, locked, unlocked, transferred, answered, unanswered, labeled, unlabeled, had its category changed, or was deleted.' },
  { id: 'discussion_comment', label: 'Discussion comments', description: 'Discussion comment created, edited, or deleted.' },
  { id: 'fork', label: 'Forks', description: 'Repository forked.' },
  { id: 'gollum', label: 'Wiki', description: 'Wiki page updated.' },
  { id: 'issue_comment', label: 'Issue comments', description: 'Issue comment created, edited, or deleted.' },
  { id: 'issues', label: 'Issues', description: 'Issue opened, edited, deleted, transferred, pinned, unpinned, closed, reopened, assigned, unassigned, labeled, unlabeled, milestoned, demilestoned, locked, or unlocked.' },
  { id: 'label', label: 'Labels', description: 'Label created, edited or deleted.' },
  { id: 'member', label: 'Collaborator add, remove, or changed', description: 'Collaborator added to, removed from, or has changed permissions for a repository.' },
  { id: 'milestone', label: 'Milestones', description: 'Milestone created, closed, opened, edited, or deleted.' },
  { id: 'public', label: 'Visibility changes', description: 'Repository changes from private to public.' },
  { id: 'pull_request', label: 'Pull requests', description: 'Pull request assigned, auto merge disabled, auto merge enabled, closed, converted to draft, demilestoned, dequeued, edited, enqueued, labeled, locked, milestoned, opened, ready for review, reopened, review request removed, review requested, synchronized, unassigned, unlabeled, or unlocked.' },
  { id: 'pull_request_review', label: 'Pull request reviews', description: 'Pull request review submitted, edited, or dismissed.' },
  { id: 'pull_request_review_comment', label: 'Pull request review comments', description: 'Pull request diff comment created, edited, or deleted.' },
  { id: 'pull_request_review_thread', label: 'Pull request review threads', description: 'A pull request review thread was resolved or unresolved.' },
  { id: 'push', label: 'Pushes', description: 'Git push to a repository.' },
  { id: 'release', label: 'Releases', description: 'Release created, edited, published, unpublished, or deleted.' },
  { id: 'repository', label: 'Repositories', description: 'Repository created, deleted, archived, unarchived, publicized, privatized, edited, renamed, or transferred.' },
  { id: 'repository_ruleset', label: 'Repository rulesets', description: 'Repository ruleset created, deleted or edited.' },
  { id: 'star', label: 'Stars', description: 'A star is created or deleted from a repository.' },
  { id: 'status', label: 'Statuses', description: "Commit status updated from the API." },
  { id: 'team_add', label: 'Team adds', description: 'Team added or modified on a repository.' },
  { id: 'watch', label: 'Watches', description: 'User stars a repository.' },
  { id: 'workflow_job', label: 'Workflow jobs', description: 'Workflow job queued, waiting, in progress, or completed on a repository.' },
  { id: 'workflow_run', label: 'Workflow runs', description: 'Workflow run requested or completed on a repository.' },
];

export type EventsMode = 'push' | 'all' | 'custom';

export function eventsMode(events: string[]): EventsMode {
  if (events.includes('*')) return 'all';
  if (events.length === 1 && events[0] === 'push') return 'push';
  return 'custom';
}

export function eventsFor(mode: EventsMode, custom: string[]): string[] {
  return mode === 'all' ? ['*'] : mode === 'push' ? ['push'] : custom;
}

export function eventsSummary(events: string[]): string {
  const mode = eventsMode(events);
  if (mode === 'all') return 'all events';
  if (mode === 'push') return 'the push event';
  if (events.length <= 3) return events.join(', ');
  return `${events.slice(0, 2).join(', ')} and ${events.length - 2} more`;
}

export const deliveryOk = (status: string) => status === 'OK';

// ------------------------------------------------------------------ branch protection

export interface ProtectionForm {
  requirePr: boolean;
  approvals: number;
  dismissStale: boolean;
  codeOwners: boolean;
  lastPush: boolean;
  requireChecks: boolean;
  strict: boolean;
  contexts: string[];
  conversationResolution: boolean;
  linearHistory: boolean;
  enforceAdmins: boolean;
  restrictPushes: boolean;
  pushUsers: string[];
  pushTeams: string[];
  allowForcePushes: boolean;
  allowDeletions: boolean;
  lockBranch: boolean;
}

export const EMPTY_PROTECTION: ProtectionForm = {
  requirePr: true,
  approvals: 1,
  dismissStale: false,
  codeOwners: false,
  lastPush: false,
  requireChecks: false,
  strict: false,
  contexts: [],
  conversationResolution: false,
  linearHistory: false,
  enforceAdmins: false,
  restrictPushes: false,
  pushUsers: [],
  pushTeams: [],
  allowForcePushes: false,
  allowDeletions: false,
  lockBranch: false,
};

export function fromProtection(p: BranchProtection): ProtectionForm {
  const rpr = p.required_pull_request_reviews;
  const sc = p.required_status_checks;
  return {
    requirePr: !!rpr,
    approvals: rpr?.required_approving_review_count ?? 1,
    dismissStale: !!rpr?.dismiss_stale_reviews,
    codeOwners: !!rpr?.require_code_owner_reviews,
    lastPush: !!rpr?.require_last_push_approval,
    requireChecks: !!sc,
    strict: !!sc?.strict,
    contexts: sc?.contexts ?? [],
    conversationResolution: !!p.required_conversation_resolution?.enabled,
    linearHistory: !!p.required_linear_history?.enabled,
    enforceAdmins: !!p.enforce_admins?.enabled,
    restrictPushes: !!p.restrictions,
    pushUsers: p.restrictions?.users.map((u) => u.login) ?? [],
    pushTeams: p.restrictions?.teams.map((t) => t.slug) ?? [],
    allowForcePushes: !!p.allow_force_pushes?.enabled,
    allowDeletions: !!p.allow_deletions?.enabled,
    lockBranch: !!p.lock_branch?.enabled,
  };
}

export function toProtectionInput(f: ProtectionForm, isOrg: boolean): ProtectionInput {
  return {
    required_status_checks: f.requireChecks ? { strict: f.strict, contexts: f.contexts } : null,
    enforce_admins: f.enforceAdmins,
    required_pull_request_reviews: f.requirePr
      ? {
          dismiss_stale_reviews: f.dismissStale,
          require_code_owner_reviews: f.codeOwners,
          required_approving_review_count: f.approvals,
          require_last_push_approval: f.lastPush,
        }
      : null,
    restrictions: isOrg && f.restrictPushes ? { users: f.pushUsers, teams: f.pushTeams } : null,
    required_linear_history: f.linearHistory,
    allow_force_pushes: f.allowForcePushes,
    allow_deletions: f.allowDeletions,
    required_conversation_resolution: f.conversationResolution,
    lock_branch: f.lockBranch,
  };
}

export function protectionFormError(f: ProtectionForm): string | null {
  if (f.requirePr && (!Number.isInteger(f.approvals) || f.approvals < 0 || f.approvals > 6)) {
    return 'Required approvals must be between 0 and 6.';
  }
  return null;
}

/** One-line summary of a rule for the rules list. */
export function summarizeRule(p: BranchProtection): string[] {
  const out: string[] = [];
  const rpr = p.required_pull_request_reviews;
  if (rpr) {
    const n = rpr.required_approving_review_count;
    out.push(n ? `${n} approval${n === 1 ? '' : 's'} required` : 'Pull request required');
  }
  const sc = p.required_status_checks;
  if (sc) out.push(sc.contexts.length ? `${sc.contexts.length} status check${sc.contexts.length === 1 ? '' : 's'}` : 'Status checks');
  if (p.required_linear_history?.enabled) out.push('Linear history');
  if (p.required_signatures?.enabled) out.push('Signed commits');
  if (p.enforce_admins?.enabled) out.push('Includes administrators');
  if (p.restrictions) out.push('Push restricted');
  if (p.allow_force_pushes?.enabled) out.push('Force pushes allowed');
  if (p.lock_branch?.enabled) out.push('Locked');
  return out.length ? out : ['No restrictions'];
}

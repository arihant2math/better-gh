/**
 * Compact client model shapes. Normative spec: docs/SYNC_PROTOCOL.md §3.
 * Keep this file and the doc in sync.
 */

export type ID = number;
/** `YYYY-MM-DDTHH:MM:SSZ` — sorts correctly as a string. */
export type Timestamp = string;
export type Permission = 'read' | 'triage' | 'write' | 'maintain' | 'admin';
export type ReactionContent = '+1' | '-1' | 'laugh' | 'hooray' | 'confused' | 'heart' | 'rocket' | 'eyes';
export type ReactionCounts = Partial<Record<ReactionContent, number>>;

export interface User {
  id: ID;
  login: string;
  name: string | null;
  avatarUrl: string;
  type: 'User' | 'Bot';
}

export interface Org {
  id: ID;
  login: string;
  name: string | null;
  avatarUrl: string;
  description: string | null;
}

export interface Membership {
  id: ID;
  orgId: ID;
  userId: ID;
  role: 'admin' | 'member';
}

export interface Team {
  id: ID;
  orgId: ID;
  slug: string;
  name: string;
  description: string | null;
  privacy: 'closed' | 'secret';
  parentId: ID | null;
  memberIds: ID[];
  repoIds: ID[];
}

export interface Repo {
  id: ID;
  ownerId: ID;
  owner: string;
  name: string;
  description: string | null;
  private: boolean;
  fork: boolean;
  archived: boolean;
  defaultBranch: string;
  language: string | null;
  topics: string[];
  stars: number;
  forks: number;
  watchers: number;
  openIssues: number;
  openPulls: number;
  hasIssues: boolean;
  hasProjects: boolean;
  hasWiki: boolean;
  pushedAt: Timestamp | null;
  createdAt: Timestamp;
  updatedAt: Timestamp;
  /** Upstream URL of a pull mirror (read-only for git writes). */
  mirrorUrl?: string | null;
}

export interface ViewerRepo {
  id: ID;
  permission: Permission;
  starred: boolean;
  watching: 'subscribed' | 'ignored' | 'participating';
}

export interface Label {
  id: ID;
  repoId: ID;
  name: string;
  color: string;
  description: string | null;
}

export interface Milestone {
  id: ID;
  repoId: ID;
  number: number;
  title: string;
  description: string | null;
  state: 'open' | 'closed';
  dueOn: Timestamp | null;
  openIssues: number;
  closedIssues: number;
  createdAt: Timestamp;
  updatedAt: Timestamp;
  closedAt: Timestamp | null;
}

export type ReviewDecision = 'approved' | 'changes_requested' | 'review_required' | null;
export type ChecksState = 'success' | 'failure' | 'pending' | 'neutral' | null;

export interface Issue {
  id: ID;
  repoId: ID;
  number: number;
  title: string;
  /** Lazy: `undefined` = not loaded yet (load via partial sync). */
  body?: string | null;
  state: 'open' | 'closed';
  stateReason: 'completed' | 'not_planned' | 'reopened' | 'duplicate' | null;
  authorId: ID;
  assigneeIds: ID[];
  labelIds: ID[];
  milestoneId: ID | null;
  comments: number;
  locked: boolean;
  /** Present on rows written by bgh-issues. */
  activeLockReason?: 'off-topic' | 'too heated' | 'resolved' | 'spam' | null;
  reactions?: ReactionCounts;
  /** Parent issue id (sub-issues). */
  parentId?: ID | null;
  /** Sub-issue ids in priority order (may include issues of other repos). */
  subIssueIds?: ID[];
  /** Pinned to the repository's issue list. */
  pinned?: boolean;
  /** Pull requests that close this issue on merge (keyword or manual link; may be in other repos). */
  linkedPullIds?: ID[];
  createdAt: Timestamp;
  updatedAt: Timestamp;
  closedAt: Timestamp | null;
  isPr: boolean;
  draft?: boolean;
  merged?: boolean;
  mergedAt?: Timestamp | null;
  mergedById?: ID | null;
  headRef?: string;
  headRepoId?: ID | null;
  headSha?: string;
  baseRef?: string;
  baseSha?: string;
  mergeable?: boolean | null;
  mergeableState?: 'clean' | 'dirty' | 'blocked' | 'behind' | 'unstable' | 'unknown';
  reviewDecision?: ReviewDecision;
  requestedReviewerIds?: ID[];
  requestedTeamIds?: ID[];
  checks?: ChecksState;
  additions?: number;
  deletions?: number;
  changedFiles?: number;
  commits?: number;
  // Extensions sent by bgh-pulls (docs/packages/pulls.md):
  mergeCommitSha?: string | null;
  rebaseable?: boolean | null;
  maintainerCanModify?: boolean;
  autoMerge?: AutoMerge | null;
  reviewComments?: number;
  /** PRs only: issues this pull request closes on merge (may be in other repos). */
  closingIssueIds?: ID[];
}

export interface AutoMerge {
  enabledById?: ID | null;
  mergeMethod: 'merge' | 'squash' | 'rebase';
  commitTitle?: string | null;
  commitMessage?: string | null;
}

export interface Comment {
  id: ID;
  repoId: ID;
  issueId: ID;
  authorId: ID;
  body: string;
  authorAssociation: 'OWNER' | 'MEMBER' | 'COLLABORATOR' | 'CONTRIBUTOR' | 'FIRST_TIME_CONTRIBUTOR' | 'NONE';
  reactions?: ReactionCounts;
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

export interface Review {
  id: ID;
  repoId: ID;
  issueId: ID;
  authorId: ID;
  state: 'APPROVED' | 'CHANGES_REQUESTED' | 'COMMENTED' | 'DISMISSED' | 'PENDING';
  body: string;
  commitId: string;
  submittedAt: Timestamp | null;
}

export type IssueEventType =
  | 'labeled'
  | 'unlabeled'
  | 'assigned'
  | 'unassigned'
  | 'milestoned'
  | 'demilestoned'
  | 'renamed'
  | 'closed'
  | 'reopened'
  | 'merged'
  | 'referenced'
  | 'locked'
  | 'unlocked'
  | 'review_requested'
  | 'review_request_removed'
  | 'ready_for_review'
  | 'convert_to_draft'
  | 'head_ref_force_pushed'
  | 'mentioned'
  | 'subscribed'
  | 'cross-referenced'
  | 'pinned'
  | 'unpinned'
  | 'transferred'
  | 'sub_issue_added'
  | 'sub_issue_removed'
  | 'parent_issue_added'
  | 'parent_issue_removed'
  | 'connected'
  | 'disconnected';

export interface IssueEvent {
  id: ID;
  repoId: ID;
  issueId: ID;
  actorId: ID | null;
  event: IssueEventType;
  data: {
    labelId?: ID;
    labelName?: string;
    labelColor?: string;
    assigneeId?: ID;
    reviewerId?: ID;
    milestoneTitle?: string;
    from?: string;
    to?: string;
    stateReason?: string;
    commitId?: string;
    lockReason?: string;
    sourceIssueId?: ID;
    sourceCommentId?: ID;
    /** cross-referenced, connected/disconnected, closed by a PR: the other side ("owner/repo", number). */
    sourceNumber?: number;
    sourceRepository?: string;
    sourceIsPr?: boolean;
    subIssueId?: ID;
    subIssueNumber?: number;
    subIssueRepository?: string;
    parentIssueId?: ID;
    parentIssueNumber?: number;
    parentIssueRepository?: string;
    fromRepository?: string;
  };
  createdAt: Timestamp;
}

export interface Notification {
  id: ID;
  repoId: ID;
  subjectType: 'Issue' | 'PullRequest' | 'Commit' | 'Release' | 'Discussion' | 'CheckSuite';
  subjectId: ID | null;
  title: string;
  reason:
    | 'assign'
    | 'author'
    | 'comment'
    | 'mention'
    | 'review_requested'
    | 'state_change'
    | 'subscribed'
    | 'team_mention'
    | 'manual'
    | 'ci_activity'
    | 'security_alert';
  unread: boolean;
  updatedAt: Timestamp;
  lastReadAt: Timestamp | null;
}

// ------------------------------------------------------------------ projects (scope org:{ownerId} | user:{ownerId})

export interface Project {
  id: ID;
  ownerId: ID;
  number: number;
  title: string;
  shortDescription: string | null;
  readme: string | null;
  public: boolean;
  closed: boolean;
  closedAt: Timestamp | null;
  creatorId: ID | null;
  linkedRepoIds: ID[];
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

export type ProjectFieldType =
  | 'title'
  | 'assignees'
  | 'status'
  | 'labels'
  | 'repository'
  | 'milestone'
  | 'text'
  | 'number'
  | 'date'
  | 'single_select'
  | 'iteration';

/** GitHub Projects option colors. */
export type ProjectOptionColor = 'GRAY' | 'BLUE' | 'GREEN' | 'YELLOW' | 'ORANGE' | 'RED' | 'PINK' | 'PURPLE';

export interface ProjectFieldOption {
  /** Server-generated 8-hex id (temporary client ids are replaced by the echoed delta). */
  id: string;
  name: string;
  color: ProjectOptionColor | string;
  description: string;
}

export interface ProjectIteration {
  id: string;
  title: string;
  /** `YYYY-MM-DD` */
  startDate: string;
  /** days */
  duration: number;
}

export interface ProjectIterationConfig {
  startDate: string;
  duration: number;
  iterations: ProjectIteration[];
}

export interface ProjectField {
  id: ID;
  projectId: ID;
  name: string;
  dataType: ProjectFieldType;
  position: number;
  options: ProjectFieldOption[] | null;
  iterations: ProjectIterationConfig | null;
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

export type ProjectLayout = 'table' | 'board' | 'roadmap';

export interface ProjectView {
  id: ID;
  projectId: ID;
  number: number;
  name: string;
  layout: ProjectLayout;
  position: number;
  filter: string;
  groupByFieldId: ID | null;
  /** Board column field (single select / status / iteration). */
  columnFieldId: ID | null;
  sortBy: { fieldId: ID; direction: 'asc' | 'desc' }[];
  /** Ordered = column order. */
  visibleFieldIds: ID[];
  /** Board option ids hidden as columns. */
  hiddenColumnIds: string[];
  /** Roadmap date/iteration field. */
  dateFieldId: ID | null;
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

/** text → string, number → number, date → "YYYY-MM-DD", single_select/status → option id, iteration → iteration id. */
export type ProjectValue = string | number;

export interface ProjectItem {
  id: ID;
  projectId: ID;
  contentType: 'Issue' | 'PullRequest' | 'DraftIssue';
  issueId: ID | null;
  /** Draft only. */
  title: string | null;
  /** Draft only; may be absent (not loaded). */
  body?: string | null;
  /** Draft only. */
  assigneeIds: ID[];
  archived: boolean;
  /** Fractional key (base-62, see sync/fractional.ts). */
  position: string;
  viewPositions: Record<string, string>;
  values: Record<string, ProjectValue>;
  creatorId: ID | null;
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

export type ProjectWorkflowKind = 'item_added' | 'item_reopened' | 'item_closed' | 'pr_merged' | 'auto_add' | 'auto_archive';

export interface ProjectWorkflow {
  id: ID;
  projectId: ID;
  kind: ProjectWorkflowKind;
  enabled: boolean;
  config: { statusOptionId?: string; repoIds?: ID[]; filter?: string };
  updatedAt: Timestamp;
}

// ---------------------------------------------------------------- PR extensions
// Lazy models recorded by bgh-pulls; loaded per PR via
// `GET /_bgh/repos/{o}/{r}/pulls/{n}/sync`, streamed as deltas afterwards.

export type DiffSide = 'LEFT' | 'RIGHT';

/** Inline review comment. A thread is a root (`inReplyToId == null`) plus its replies. */
export interface ReviewComment {
  id: ID;
  repoId: ID;
  issueId: ID;
  reviewId: ID | null;
  inReplyToId: ID | null;
  authorId: ID;
  body: string;
  path: string;
  commitId: string;
  originalCommitId: string;
  subjectType: 'line' | 'file';
  side: DiffSide | null;
  startSide: DiffSide | null;
  /** `null` when outdated. */
  line: number | null;
  originalLine: number | null;
  startLine: number | null;
  originalStartLine: number | null;
  position: number | null;
  originalPosition: number | null;
  outdated: boolean;
  /** Thread resolution lives on the root comment. */
  resolvedAt: Timestamp | null;
  resolvedById: ID | null;
  diffHunk?: string;
  reactions?: ReactionCounts;
  createdAt: Timestamp;
  updatedAt: Timestamp;
}

export interface Reaction {
  id: ID;
  subjectType: string;
  subjectId: ID;
  userId: ID;
  content: ReactionContent;
  issueId?: ID;
  repoId?: ID;
}

export type CheckStatus = 'queued' | 'in_progress' | 'completed' | 'waiting' | 'requested' | 'pending';
export type CheckConclusion = 'success' | 'failure' | 'neutral' | 'cancelled' | 'skipped' | 'timed_out' | 'action_required' | 'stale' | null;

export interface CheckSuite {
  id: ID;
  repoId: ID;
  headSha: string;
  headBranch: string | null;
  appSlug: string;
  status: CheckStatus;
  conclusion: CheckConclusion;
  latestCheckRunsCount: number;
}

export interface CheckRun {
  id: ID;
  repoId: ID;
  checkSuiteId: ID | null;
  headSha: string;
  name: string;
  status: CheckStatus;
  conclusion: CheckConclusion;
  detailsUrl: string | null;
  title: string | null;
  startedAt: Timestamp | null;
  completedAt: Timestamp | null;
  /** Buttons the integration offers (`requested_action` on click). */
  actions?: CheckRunAction[];
}

export interface CheckRunAction {
  label: string;
  description: string;
  identifier: string;
}

export interface CommitStatus {
  id: ID;
  repoId: ID;
  sha: string;
  state: 'error' | 'failure' | 'pending' | 'success';
  context: string;
  description: string | null;
  targetUrl: string | null;
  creatorId: ID | null;
  createdAt: Timestamp;
}

/** Model name → row type. Adding a synced model starts here (see docs/FRONTEND.md). */
export interface ModelMap {
  user: User;
  org: Org;
  membership: Membership;
  team: Team;
  repo: Repo;
  viewerRepo: ViewerRepo;
  label: Label;
  milestone: Milestone;
  issue: Issue;
  comment: Comment;
  review: Review;
  issueEvent: IssueEvent;
  notification: Notification;
  project: Project;
  projectField: ProjectField;
  projectView: ProjectView;
  projectItem: ProjectItem;
  projectWorkflow: ProjectWorkflow;
  reviewComment: ReviewComment;
  reaction: Reaction;
  checkSuite: CheckSuite;
  checkRun: CheckRun;
  commitStatus: CommitStatus;
}

export type ModelName = keyof ModelMap;
export type AnyRow = ModelMap[ModelName];

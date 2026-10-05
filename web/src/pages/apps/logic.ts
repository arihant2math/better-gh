/** Pure helpers for the GitHub App pages (permission catalog, forms). */
import type { Access, AppDetail, AppInput, PermissionMap } from '../../api/apps';

export interface PermissionDef {
  key: string;
  label: string;
  description: string;
  /** Highest level the permission accepts. */
  max: Access;
}

/** Permissions shown in the registration form, grouped like GitHub's. */
export const PERMISSION_GROUPS: { title: string; items: PermissionDef[] }[] = [
  {
    title: 'Repository permissions',
    items: [
      { key: 'actions', label: 'Actions', description: 'Workflows, workflow runs and artifacts.', max: 'write' },
      { key: 'administration', label: 'Administration', description: 'Repository settings, teams and collaborators.', max: 'write' },
      { key: 'checks', label: 'Checks', description: 'Checks on code.', max: 'write' },
      { key: 'contents', label: 'Contents', description: 'Repository contents, commits, branches, downloads, releases and merges.', max: 'write' },
      { key: 'deployments', label: 'Deployments', description: 'Deployments and deployment statuses.', max: 'write' },
      { key: 'environments', label: 'Environments', description: 'Manage repository environments.', max: 'write' },
      { key: 'issues', label: 'Issues', description: 'Issues and related comments, assignees, labels and milestones.', max: 'write' },
      { key: 'packages', label: 'Packages', description: 'Packages published to the registry.', max: 'write' },
      { key: 'pages', label: 'Pages', description: 'Pages statuses, configuration and builds.', max: 'write' },
      { key: 'pull_requests', label: 'Pull requests', description: 'Pull requests and related comments, assignees, labels, milestones and merges.', max: 'write' },
      { key: 'repository_hooks', label: 'Webhooks', description: 'Manage the post-receive hooks of a repository.', max: 'write' },
      { key: 'repository_projects', label: 'Projects', description: 'Manage repository projects, columns and cards.', max: 'admin' },
      { key: 'secrets', label: 'Secrets', description: 'Manage Actions repository secrets.', max: 'write' },
      { key: 'security_events', label: 'Code scanning alerts', description: 'View and manage security events like code scanning alerts.', max: 'write' },
      { key: 'statuses', label: 'Commit statuses', description: 'Commit statuses.', max: 'write' },
      { key: 'workflows', label: 'Workflows', description: 'Update GitHub Actions workflow files.', max: 'write' },
    ],
  },
  {
    title: 'Organization permissions',
    items: [
      { key: 'members', label: 'Members', description: 'Organization members and teams.', max: 'write' },
      { key: 'organization_administration', label: 'Administration', description: 'Manage access to an organization.', max: 'write' },
      { key: 'organization_hooks', label: 'Webhooks', description: 'Manage the post-receive hooks for an organization.', max: 'write' },
      { key: 'organization_projects', label: 'Projects', description: 'Manage organization projects.', max: 'admin' },
      { key: 'organization_secrets', label: 'Secrets', description: 'Manage Actions organization secrets.', max: 'write' },
      { key: 'organization_self_hosted_runners', label: 'Self-hosted runners', description: 'View and manage Actions self-hosted runners.', max: 'write' },
    ],
  },
  {
    title: 'Account permissions',
    items: [
      { key: 'email_addresses', label: 'Email addresses', description: "Manage a user's email addresses.", max: 'write' },
      { key: 'followers', label: 'Followers', description: "A user's followers.", max: 'write' },
      { key: 'git_ssh_keys', label: 'Git SSH keys', description: 'Git SSH keys.', max: 'write' },
      { key: 'gpg_keys', label: 'GPG keys', description: "View and manage a user's GPG keys.", max: 'write' },
      { key: 'profile', label: 'Profile', description: "Manage a user's profile settings.", max: 'write' },
      { key: 'starring', label: 'Starring', description: 'List and manage repositories a user is starring.', max: 'write' },
    ],
  },
];

/** Webhook events offered in the form. */
export const EVENT_CHOICES = [
  'check_run',
  'check_suite',
  'commit_comment',
  'create',
  'delete',
  'deployment',
  'deployment_status',
  'fork',
  'issue_comment',
  'issues',
  'label',
  'member',
  'milestone',
  'pull_request',
  'pull_request_review',
  'pull_request_review_comment',
  'push',
  'release',
  'repository',
  'star',
  'status',
  'watch',
  'workflow_run',
];

export const ACCESS_LABEL: Record<Access, string> = { read: 'Read-only', write: 'Read and write', admin: 'Admin' };

/** Levels a permission accepts, lowest first. */
export function levels(max: Access): Access[] {
  return max === 'admin' ? ['read', 'write', 'admin'] : max === 'write' ? ['read', 'write'] : ['read'];
}

/** Every known permission label, for read-only summaries. */
export function permissionLabel(key: string): string {
  for (const g of PERMISSION_GROUPS) {
    const p = g.items.find((i) => i.key === key);
    if (p) return g.title.startsWith('Repository') ? p.label : `${p.label} (${g.title.split(' ')[0]!.toLowerCase()})`;
  }
  return key.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase());
}

/** `[key, access]` pairs of a permission map without the implied metadata entry, sorted. */
export function permissionEntries(p: PermissionMap): [string, Access][] {
  return Object.entries(p)
    .filter(([k]) => k !== 'metadata')
    .sort(([a], [b]) => a.localeCompare(b));
}

/** Changes between two permission maps (for the "accept new permissions" banner). */
export function permissionChanges(current: PermissionMap, requested: PermissionMap): { key: string; from?: Access; to?: Access }[] {
  const keys = new Set([...Object.keys(current), ...Object.keys(requested)]);
  keys.delete('metadata');
  return [...keys]
    .sort()
    .filter((k) => current[k] !== requested[k])
    .map((k) => ({ key: k, from: current[k], to: requested[k] }));
}

/** App slug, like the server's: lowercase ASCII alphanumerics joined by `-`. */
export function slugify(name: string): string {
  return name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '');
}

export interface AppFormValues {
  name: string;
  description: string;
  homepage_url: string;
  callback_urls: string;
  setup_url: string;
  setup_on_update: boolean;
  webhook_active: boolean;
  webhook_url: string;
  /** New secret; empty keeps the current one (when editing). */
  webhook_secret: string;
  permissions: PermissionMap;
  events: string[];
  public: boolean;
}

export const EMPTY_APP: AppFormValues = {
  name: '',
  description: '',
  homepage_url: '',
  callback_urls: '',
  setup_url: '',
  setup_on_update: false,
  webhook_active: false,
  webhook_url: '',
  webhook_secret: '',
  permissions: {},
  events: [],
  public: false,
};

export function fromApp(a: AppDetail): AppFormValues {
  const permissions = { ...a.permissions };
  delete permissions.metadata;
  return {
    name: a.name,
    description: a.description ?? '',
    homepage_url: a.homepage_url,
    callback_urls: a.callback_urls.join('\n'),
    setup_url: a.setup_url ?? '',
    setup_on_update: a.setup_on_update,
    webhook_active: a.webhook_active,
    webhook_url: a.webhook_url ?? '',
    webhook_secret: '',
    permissions,
    events: [...a.events],
    public: a.public,
  };
}

const isUrl = (s: string) => {
  try {
    const u = new URL(s);
    return u.protocol === 'http:' || u.protocol === 'https:';
  } catch {
    return false;
  }
};

/** Client-side checks mirroring the server's 422s. */
export function validateAppForm(v: AppFormValues): Partial<Record<keyof AppFormValues, string>> {
  const e: Partial<Record<keyof AppFormValues, string>> = {};
  if (!v.name.trim()) e.name = 'GitHub App name is required';
  else if (!slugify(v.name)) e.name = 'Name must contain letters or digits';
  else if (v.name.trim().length > 34) e.name = 'Name is too long (maximum is 34 characters)';
  if (!v.homepage_url.trim()) e.homepage_url = 'Homepage URL is required';
  else if (!isUrl(v.homepage_url.trim())) e.homepage_url = 'Homepage URL is not a valid URL';
  const cbs = splitLines(v.callback_urls);
  if (cbs.some((c) => !isUrl(c))) e.callback_urls = 'Every callback URL must be a valid URL';
  else if (cbs.length > 10) e.callback_urls = 'At most 10 callback URLs';
  if (v.setup_url.trim() && !isUrl(v.setup_url.trim())) e.setup_url = 'Setup URL is not a valid URL';
  if (v.webhook_active && !v.webhook_url.trim()) e.webhook_url = 'Webhook URL is required while the webhook is active';
  else if (v.webhook_url.trim() && !isUrl(v.webhook_url.trim())) e.webhook_url = 'Webhook URL is not a valid URL';
  return e;
}

export function splitLines(s: string): string[] {
  return s
    .split(/\s*\n\s*/)
    .map((x) => x.trim())
    .filter(Boolean);
}

/** Request body for create (`owner` set) or update (`initial` set: only changed fields). */
export function toInput(v: AppFormValues, initial?: AppFormValues): AppInput {
  const body: AppInput = {
    name: v.name.trim(),
    description: v.description.trim(),
    homepage_url: v.homepage_url.trim(),
    callback_urls: splitLines(v.callback_urls),
    setup_url: v.setup_url.trim() || null,
    setup_on_update: v.setup_on_update,
    webhook_active: v.webhook_active,
    webhook_url: v.webhook_url.trim() || null,
    permissions: v.permissions,
    events: v.events,
    public: v.public,
  };
  if (v.webhook_secret) body.webhook_secret = v.webhook_secret;
  if (!initial) return body;
  const out: AppInput = {};
  const prev = toInput({ ...initial, webhook_secret: '' });
  for (const k of Object.keys(body) as (keyof AppInput)[]) {
    if (JSON.stringify(body[k]) !== JSON.stringify(prev[k])) (out as Record<string, unknown>)[k] = body[k];
  }
  return out;
}

/** Path of the installation settings page for an account. */
export function installationPath(account: { login: string; type: string }, id: number): string {
  return account.type === 'Organization'
    ? `/organizations/${encodeURIComponent(account.login)}/settings/installations/${id}`
    : `/settings/installations/${id}`;
}

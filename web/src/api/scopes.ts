/**
 * GitHub classic OAuth / personal-access-token scopes, grouped like
 * github.com's "Select scopes" tree (a child's `parent` grants it).
 * `SCOPES` lists exactly what the server accepts (bgh-accounts
 * `KNOWN_SCOPES`); `describeScope` also knows a few github.com-only scopes
 * so foreign scope strings still read well.
 */

export interface ScopeInfo {
  id: string;
  description: string;
  parent?: string;
  /** Only site administrators may grant it. */
  siteAdminOnly?: boolean;
}

export const SCOPES: ScopeInfo[] = [
  { id: 'repo', description: 'Full control of private repositories' },
  { id: 'repo:status', description: 'Access commit status', parent: 'repo' },
  {
    id: 'repo_deployment',
    description: 'Access deployment status',
    parent: 'repo',
  },
  {
    id: 'public_repo',
    description: 'Access public repositories',
    parent: 'repo',
  },
  {
    id: 'repo:invite',
    description: 'Access repository invitations',
    parent: 'repo',
  },
  {
    id: 'security_events',
    description: 'Read and write security events',
    parent: 'repo',
  },
  { id: 'workflow', description: 'Update GitHub Action workflows' },
  {
    id: 'write:packages',
    description: 'Upload packages to GitHub Package Registry',
  },
  {
    id: 'read:packages',
    description: 'Download packages from GitHub Package Registry',
    parent: 'write:packages',
  },
  {
    id: 'delete:packages',
    description: 'Delete packages from GitHub Package Registry',
  },
  {
    id: 'admin:org',
    description: 'Full control of orgs and teams, read and write org projects',
  },
  {
    id: 'write:org',
    description: 'Read and write org and team membership, read and write org projects',
    parent: 'admin:org',
  },
  {
    id: 'read:org',
    description: 'Read org and team membership, read org projects',
    parent: 'admin:org',
  },
  { id: 'admin:public_key', description: 'Full control of user public keys' },
  {
    id: 'write:public_key',
    description: 'Write user public keys',
    parent: 'admin:public_key',
  },
  {
    id: 'read:public_key',
    description: 'Read user public keys',
    parent: 'admin:public_key',
  },
  { id: 'admin:repo_hook', description: 'Full control of repository hooks' },
  {
    id: 'write:repo_hook',
    description: 'Write repository hooks',
    parent: 'admin:repo_hook',
  },
  {
    id: 'read:repo_hook',
    description: 'Read repository hooks',
    parent: 'admin:repo_hook',
  },
  { id: 'admin:org_hook', description: 'Full control of organization hooks' },
  { id: 'gist', description: 'Create gists' },
  { id: 'notifications', description: 'Access notifications' },
  { id: 'user', description: 'Update ALL user data' },
  {
    id: 'read:user',
    description: 'Read ALL user profile data',
    parent: 'user',
  },
  {
    id: 'user:email',
    description: 'Access user email addresses (read-only)',
    parent: 'user',
  },
  {
    id: 'user:follow',
    description: 'Follow and unfollow users',
    parent: 'user',
  },
  { id: 'delete_repo', description: 'Delete repositories' },
  { id: 'admin:gpg_key', description: 'Full control of public user GPG keys' },
  {
    id: 'write:gpg_key',
    description: 'Write public user GPG keys',
    parent: 'admin:gpg_key',
  },
  {
    id: 'read:gpg_key',
    description: 'Read public user GPG keys',
    parent: 'admin:gpg_key',
  },
  { id: 'project', description: 'Full control of projects' },
  {
    id: 'read:project',
    description: 'Read access of projects',
    parent: 'project',
  },
  {
    id: 'site_admin',
    description: 'Site administration (this instance)',
    siteAdminOnly: true,
  },
];

/** github.com scopes this server doesn't issue (still described when seen). */
const EXTRA: Record<string, string> = {
  'write:discussion': 'Read and write team discussions',
  'read:discussion': 'Read team discussions',
  codespace: 'Full control of codespaces',
  'admin:enterprise': 'Full control of enterprises',
  copilot: 'Full control of GitHub Copilot settings and seat assignments',
  'admin:ssh_signing_key': 'Full control of public user SSH signing keys',
  audit_log: 'Full control of audit log',
};

const BY_ID = new Map(SCOPES.map((s) => [s.id, s]));

export function scopeInfo(id: string): ScopeInfo | undefined {
  return BY_ID.get(id);
}

/** Human description of a scope ("Full control of private repositories"); unknown scopes echo the id. */
export function describeScope(id: string): string {
  return BY_ID.get(id)?.description ?? EXTRA[id] ?? id;
}

/** Child scopes of a top-level scope. */
export function childScopes(id: string): ScopeInfo[] {
  return SCOPES.filter((s) => s.parent === id);
}

/** Top-level scopes in display order. */
export function topScopes(): ScopeInfo[] {
  return SCOPES.filter((s) => !s.parent);
}

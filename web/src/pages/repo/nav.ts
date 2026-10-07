/**
 * Pure helpers for the repository header and navigation (package P12):
 * rename/transfer redirects, tab visibility, the watch button label and
 * which unknown sub-paths still get a placeholder.
 */
import type { Repo, ViewerRepo } from '../../sync/models';

/**
 * Where to send the browser when `/:owner/:name/...` resolved (through the
 * server's rename/transfer redirects) to `fullName`. Keeps the sub-path,
 * query and hash. `null` when the URL already names the repository.
 */
export function canonicalRepoUrl(loc: { pathname: string; search: string; hash: string }, owner: string, name: string, fullName: string): string | null {
  const [newOwner, newName] = fullName.split('/');
  if (!newOwner || !newName) return null;
  if (`${owner}/${name}`.toLowerCase() === fullName.toLowerCase()) return null;
  // `['', owner, name, ...rest]`
  const rest = loc.pathname.split('/').slice(3).join('/');
  return `/${newOwner}/${newName}${rest ? `/${rest}` : ''}${loc.search}${loc.hash}`;
}

export type RepoTabId = 'code' | 'issues' | 'pulls' | 'actions' | 'projects' | 'wiki' | 'security' | 'pulse' | 'settings';

/** Header tabs in order; Issues/Projects/Wiki follow the repo's feature toggles, Settings needs admin. */
export function visibleRepoTabs(repo: Pick<Repo, 'hasIssues' | 'hasProjects' | 'hasWiki'>, canAdmin: boolean): RepoTabId[] {
  const out: RepoTabId[] = ['code'];
  if (repo.hasIssues) out.push('issues');
  out.push('pulls', 'actions');
  if (repo.hasProjects) out.push('projects');
  if (repo.hasWiki) out.push('wiki');
  out.push('security', 'pulse');
  if (canAdmin) out.push('settings');
  return out;
}

/** Which tab a repo sub-path belongs to (`''` = code). */
export function currentRepoTab(section: string): string {
  if (section === '' || section === 'tree' || section === 'blob' || section === 'blame' || section === 'commits' || section === 'commit' || section === 'branches' || section === 'tags' || section === 'releases' || section === 'compare') {
    return section === 'compare' ? 'pulls' : 'code';
  }
  if (section === 'pull') return 'pulls';
  if (section === 'labels' || section === 'milestones' || section === 'milestone') return 'issues';
  if (section === 'graphs' || section === 'community' || section === 'network') return 'pulse';
  return section;
}

/** Watch button label: Watch (participating), Unwatch (all activity / custom), Ignoring. */
export function watchLabel(watching: ViewerRepo['watching'] | undefined, custom = false): 'Watch' | 'Unwatch' | 'Ignoring' | 'Custom' {
  if (watching === 'ignored') return 'Ignoring';
  if (watching === 'subscribed') return custom ? 'Custom' : 'Unwatch';
  return 'Watch';
}

/** Tabs that are announced in the header but not built yet (P66 Security). */
export const PLACEHOLDER_TABS = new Set(['security']);

/** Ahead/behind summary of a fork branch against its upstream branch. */
export function syncSummary(aheadBy: number, behindBy: number, upstream: string): string {
  if (aheadBy === 0 && behindBy === 0) return `This branch is up to date with ${upstream}.`;
  const parts: string[] = [];
  if (aheadBy) parts.push(`${aheadBy} commit${aheadBy === 1 ? '' : 's'} ahead of`);
  if (behindBy) parts.push(`${behindBy} commit${behindBy === 1 ? '' : 's'} behind`);
  return `This branch is ${parts.join(' and ')} ${upstream}.`;
}

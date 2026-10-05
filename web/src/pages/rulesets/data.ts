/** Data for the ruleset editor's pickers and previews (all optional: failures yield empty lists). */
import { useResource } from '../../api/cache';
import { api, v3 } from '../../api/client';
import { listTags } from '../../api/code';
import { listBranchesAll } from '../../api/repoSettings';
import type { Repo } from '../../sync/models';
import { fetchAll } from '../orgsettings/api';
import { ACTIONS_APP } from './model';

export interface AppRef {
  id: number;
  slug: string;
  name: string;
}

export interface OrgRepo {
  id: number;
  name: string;
  full_name: string;
  private: boolean;
  default_branch: string;
}

const quiet = <T>(p: Promise<T>, fallback: T): Promise<T> => p.catch(() => fallback);

/** Branch or tag names of a repository (for the live target preview). */
export function useRefNames(repo: Repo | undefined, target: 'branch' | 'tag' | 'push') {
  const kind = target === 'tag' ? 'tag' : 'branch';
  return useResource<string[]>(repo && target !== 'push' ? `rulesets:refs:${kind}:${repo.id}/` : null, () =>
    kind === 'tag' ? quiet(listTags(repo!.owner, repo!.name).then((l) => l.map((t) => t.name)), []) : quiet(listBranchesAll(repo!.owner, repo!.name).then((l) => l.map((b) => b.name)), []),
  );
}

/** Every repository of an organization (repository targeting and its preview). */
export function useOrgRepos(org: string | null) {
  return useResource<OrgRepo[]>(org ? `rulesets:orgrepos:${org}/` : null, () => quiet(fetchAll<OrgRepo>(`${v3('orgs', org!, 'repos')}?per_page=100&type=all`), []));
}

interface CheckRunsResponse {
  check_runs: { name: string; app?: AppRef | null }[];
}

interface StatusResponse {
  statuses: { context: string }[];
}

/**
 * Status check names seen on the default branch's head (check runs and
 * commit statuses) and the apps that posted them, plus GitHub Actions.
 */
export function useCheckSuggestions(repo: Repo | undefined) {
  return useResource<{ contexts: string[]; apps: AppRef[] }>(repo ? `rulesets:checks:${repo.id}/` : null, async () => {
    const ref = encodeURIComponent(repo!.defaultBranch);
    const base = v3('repos', repo!.owner, repo!.name, 'commits');
    const [runs, status] = await Promise.all([
      quiet(api.get<CheckRunsResponse>(`${base}/${ref}/check-runs?per_page=100`), { check_runs: [] }),
      quiet(api.get<StatusResponse>(`${base}/${ref}/status`), { statuses: [] }),
    ]);
    const contexts = new Set<string>();
    const apps = new Map<number, AppRef>([[ACTIONS_APP.id, ACTIONS_APP]]);
    for (const r of runs.check_runs ?? []) {
      contexts.add(r.name);
      if (r.app?.id) apps.set(r.app.id, { id: r.app.id, slug: r.app.slug, name: r.app.name });
    }
    for (const s of status.statuses ?? []) contexts.add(s.context);
    return { contexts: [...contexts].sort(), apps: [...apps.values()] };
  });
}

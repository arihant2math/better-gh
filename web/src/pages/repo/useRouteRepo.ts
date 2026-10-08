import { createContext, use } from 'react';
import type { Repo } from '../../sync/models';

/** The repository `RepoLayout` resolved for the current `/:owner/:repo/...` route. */
export const RouteRepoContext = createContext<Repo | null>(null);

/**
 * The repository of the current `/:owner/:repo/...` route. Only for pages
 * rendered inside `RepoLayout`, which shows loading / not found until the row
 * exists and then provides it, so pages never re-resolve the route.
 */
export function useRouteRepo(): Repo {
  const repo = use(RouteRepoContext);
  if (!repo) throw new Error('useRouteRepo() used outside RepoLayout');
  return repo;
}

/** Sidebar values of the pull request creation form, applied once the PR exists. */
import { api } from '../../api/client';
import { store } from '../../sync';
import type { ID, Repo } from '../../sync/models';
import { addIssueItem } from '../../sync/projects';

export interface NewPullMeta {
  reviewerIds: ID[];
  teamIds: ID[];
  assigneeIds: ID[];
  labelIds: ID[];
  milestoneId: ID | null;
  projectIds: ID[];
}

export function hasMeta(m: NewPullMeta): boolean {
  return m.reviewerIds.length + m.teamIds.length + m.assigneeIds.length + m.labelIds.length + m.projectIds.length > 0 || m.milestoneId != null;
}

/**
 * Apply `meta` to the new PR `number` as one batch: labels, assignees and
 * milestone in a single issue PATCH, reviewers in one request, one item per
 * project, all concurrently. Resolves with the names of the parts that failed.
 */
export async function applyNewPullMeta(repo: Repo, number: number, meta: NewPullMeta): Promise<string[]> {
  const s = store();
  const base = `/api/v3/repos/${encodeURIComponent(repo.owner)}/${encodeURIComponent(repo.name)}`;
  const login = (id: ID) => s.get('user', id)?.login;
  const jobs: [string, () => Promise<unknown>][] = [];
  if (meta.labelIds.length || meta.assigneeIds.length || meta.milestoneId != null) {
    jobs.push([
      'labels, assignees and milestone',
      () =>
        api.patch(`${base}/issues/${number}`, {
          ...(meta.labelIds.length ? { labels: meta.labelIds.map((id) => s.get('label', id)?.name).filter(Boolean) } : {}),
          ...(meta.assigneeIds.length ? { assignees: meta.assigneeIds.map(login).filter(Boolean) } : {}),
          ...(meta.milestoneId != null ? { milestone: s.get('milestone', meta.milestoneId)?.number } : {}),
        }),
    ]);
  }
  if (meta.reviewerIds.length || meta.teamIds.length) {
    jobs.push([
      'reviewers',
      () =>
        api.post(`${base}/pulls/${number}/requested_reviewers`, {
          reviewers: meta.reviewerIds.map(login).filter(Boolean),
          team_reviewers: meta.teamIds.map((id) => s.get('team', id)?.slug).filter(Boolean),
        }),
    ]);
  }
  for (const id of meta.projectIds) {
    const p = s.get('project', id);
    if (p) jobs.push([`project “${p.title}”`, () => addIssueItem(p, { owner: repo.owner, repo: repo.name, number, isPr: true }).done]);
  }
  const results = await Promise.allSettled(jobs.map(([, run]) => run()));
  return jobs.filter((_, i) => results[i]!.status === 'rejected').map(([name]) => name);
}

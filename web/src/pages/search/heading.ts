import type { SearchType } from '../../search/qualifiers';

const NOUNS: Record<SearchType, [one: string, many: string]> = {
  code: ['code result', 'code results'],
  repositories: ['repository', 'repositories'],
  issues: ['issue', 'issues'],
  pulls: ['pull request', 'pull requests'],
  users: ['user', 'users'],
  commits: ['commit', 'commits'],
};

/** Results heading: "1 issue", "37 issues", "No issues", "4 code results". */
export function resultsHeading(type: SearchType, total: number): string {
  const [one, many] = NOUNS[type];
  if (total === 0) return `No ${many}`;
  return `${total.toLocaleString()} ${total === 1 ? one : many}`;
}

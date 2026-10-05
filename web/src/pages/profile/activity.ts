/** One-line summaries of GitHub activity events for "Contribution activity". */
import type { RestEvent } from '../../api/profile';

export interface EventSummary {
  kind: 'push' | 'create' | 'delete' | 'star' | 'fork' | 'issue' | 'pull' | 'comment' | 'release' | 'other';
  /** Text before the repository name, e.g. "Pushed 3 commits to". */
  text: string;
  repo: string;
  /** Optional link target inside the repository (e.g. `/issues/4`). */
  path?: string;
  detail?: string;
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`;

export function summarizeEvent(e: RestEvent): EventSummary {
  const p = e.payload ?? {};
  const repo = e.repo?.name ?? '';
  const action = typeof p.action === 'string' ? p.action : '';
  const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
  switch (e.type) {
    case 'PushEvent': {
      const n = typeof p.size === 'number' ? p.size : Array.isArray(p.commits) ? p.commits.length : 1;
      const ref = typeof p.ref === 'string' ? p.ref.replace(/^refs\/heads\//, '') : undefined;
      return { kind: 'push', text: `Pushed ${plural(n, 'commit')} to`, repo, detail: ref };
    }
    case 'CreateEvent': {
      const t = String(p.ref_type ?? 'repository');
      if (t === 'repository') return { kind: 'create', text: 'Created repository', repo };
      return { kind: 'create', text: `Created ${t} ${String(p.ref ?? '')} in`, repo };
    }
    case 'DeleteEvent':
      return { kind: 'delete', text: `Deleted ${String(p.ref_type ?? 'branch')} ${String(p.ref ?? '')} in`, repo };
    case 'WatchEvent':
      return { kind: 'star', text: 'Starred', repo };
    case 'ForkEvent':
      return { kind: 'fork', text: 'Forked', repo };
    case 'PublicEvent':
      return { kind: 'create', text: 'Made public', repo };
    case 'IssuesEvent': {
      const i = (p.issue ?? {}) as { number?: number; title?: string };
      return { kind: 'issue', text: `${cap(action || 'opened')} issue #${i.number ?? '?'} in`, repo, path: `/issues/${i.number}`, detail: i.title };
    }
    case 'PullRequestEvent': {
      const pr = (p.pull_request ?? {}) as { number?: number; title?: string; merged?: boolean };
      const verb = action === 'closed' && pr.merged ? 'Merged' : cap(action || 'opened');
      return { kind: 'pull', text: `${verb} pull request #${pr.number ?? '?'} in`, repo, path: `/pull/${pr.number}`, detail: pr.title };
    }
    case 'PullRequestReviewEvent': {
      const pr = (p.pull_request ?? {}) as { number?: number; title?: string };
      return { kind: 'pull', text: `Reviewed pull request #${pr.number ?? '?'} in`, repo, path: `/pull/${pr.number}`, detail: pr.title };
    }
    case 'IssueCommentEvent':
    case 'PullRequestReviewCommentEvent':
    case 'CommitCommentEvent': {
      const i = (p.issue ?? p.pull_request ?? {}) as { number?: number; title?: string };
      return { kind: 'comment', text: i.number ? `Commented on #${i.number} in` : 'Commented in', repo, path: i.number ? `/issues/${i.number}` : undefined, detail: i.title };
    }
    case 'ReleaseEvent': {
      const r = (p.release ?? {}) as { tag_name?: string; name?: string };
      return { kind: 'release', text: `Published release ${r.name || r.tag_name || ''} in`.replace(/\s+in$/, ' in'), repo };
    }
    case 'MemberEvent':
      return { kind: 'other', text: 'Added a collaborator to', repo };
    default:
      return { kind: 'other', text: `${e.type.replace(/Event$/, '').replace(/([a-z])([A-Z])/g, '$1 $2')} in`, repo };
  }
}

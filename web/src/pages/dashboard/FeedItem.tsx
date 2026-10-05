import type { ReactNode } from 'react';
import { Link } from '../../router';
import { Avatar, StateIcon } from '../../ui/Badge';
import {
  CommentIcon,
  GitBranchIcon,
  GitCommitIcon,
  GitMergeIcon,
  GitPullRequestIcon,
  IssueClosedIcon,
  IssueOpenedIcon,
  PersonIcon,
  RepoForkedIcon,
  RepoIcon,
  RocketIcon,
  StarFillIcon,
  TagIcon,
  TrashIcon,
  EyeIcon,
  type Icon,
} from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './DashboardPage.module.css';
import type { FeedEvent, FeedGroup } from './feed';

interface IssueRef {
  number: number;
  title: string;
  state?: string;
  html_url?: string;
  merged?: boolean;
  pull_request?: unknown;
  draft?: boolean;
}

function path(url: string | undefined, fallback: string): string {
  if (!url) return fallback;
  try {
    return new URL(url, window.location.origin).pathname;
  } catch {
    return fallback;
  }
}

function issueOf(e: FeedEvent): IssueRef | undefined {
  return (e.payload.issue ?? e.payload.pull_request) as IssueRef | undefined;
}

function isPrEvent(e: FeedEvent): boolean {
  return e.type.startsWith('PullRequest') || !!(e.payload.issue as IssueRef | undefined)?.pull_request;
}

function issueLink(e: FeedEvent): string {
  const i = issueOf(e);
  if (!i) return `/${e.repo.name}`;
  return path(i.html_url, `/${e.repo.name}/${isPrEvent(e) ? 'pull' : 'issues'}/${i.number}`);
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function describe(g: FeedGroup): { icon: Icon; verb: ReactNode; tone?: string } {
  const e = g.events[0]!;
  const n = g.events.length;
  const repo = (
    <Link to={`/${g.repo}`} className={styles.feedRepo}>
      {g.repo}
    </Link>
  );
  switch (g.type) {
    case 'PushEvent': {
      const commits = g.events.reduce((sum, x) => sum + Number(x.payload.size ?? (x.payload.commits as unknown[] | undefined)?.length ?? 0), 0);
      const branch = String(e.payload.ref ?? '').replace(/^refs\/heads\//, '');
      return {
        icon: GitCommitIcon,
        verb: (
          <>
            pushed {plural(commits, 'commit')} to <code className={styles.branch}>{branch}</code>
            {n > 1 ? ` (${n} pushes)` : ''} in {repo}
          </>
        ),
      };
    }
    case 'IssuesEvent':
      return {
        icon: g.action === 'closed' ? IssueClosedIcon : IssueOpenedIcon,
        tone: g.action === 'closed' ? 'closed' : 'open',
        verb: (
          <>
            {g.action} {n > 1 ? plural(n, 'issue') : 'an issue'} in {repo}
          </>
        ),
      };
    case 'PullRequestEvent':
      return {
        icon: g.action === 'merged' ? GitMergeIcon : GitPullRequestIcon,
        tone: g.action === 'merged' ? 'merged' : g.action === 'closed' ? 'closed' : 'open',
        verb: (
          <>
            {g.action} {n > 1 ? plural(n, 'pull request') : 'a pull request'} in {repo}
          </>
        ),
      };
    case 'IssueCommentEvent':
    case 'PullRequestReviewCommentEvent':
      return {
        icon: CommentIcon,
        verb: (
          <>
            commented {n > 1 ? `${n} times ` : ''}in {repo}
          </>
        ),
      };
    case 'PullRequestReviewEvent':
      return { icon: EyeIcon, verb: <>reviewed {n > 1 ? plural(n, 'pull request') : 'a pull request'} in {repo}</> };
    case 'WatchEvent':
      return { icon: StarFillIcon, tone: 'star', verb: <>starred {repo}</> };
    case 'ForkEvent': {
      const forkee = (e.payload.forkee as { full_name?: string } | undefined)?.full_name;
      return {
        icon: RepoForkedIcon,
        verb: (
          <>
            forked {repo}
            {forkee && (
              <>
                {' '}
                to <Link to={`/${forkee}`}>{forkee}</Link>
              </>
            )}
          </>
        ),
      };
    }
    case 'CreateEvent':
      return g.action === 'repository'
        ? { icon: RepoIcon, verb: <>created repository {repo}</> }
        : {
            icon: g.action === 'tag' ? TagIcon : GitBranchIcon,
            verb: (
              <>
                created {g.action} <code className={styles.branch}>{String(e.payload.ref ?? '')}</code>
                {n > 1 ? ` and ${n - 1} more` : ''} in {repo}
              </>
            ),
          };
    case 'DeleteEvent':
      return { icon: TrashIcon, verb: <>deleted {g.action} <code className={styles.branch}>{String(e.payload.ref ?? '')}</code> in {repo}</> };
    case 'ReleaseEvent': {
      const rel = e.payload.release as { name?: string; tag_name?: string } | undefined;
      return { icon: RocketIcon, tone: 'open', verb: <>released {rel?.name || rel?.tag_name} in {repo}</> };
    }
    case 'MemberEvent': {
      const member = (e.payload.member as { login?: string } | undefined)?.login;
      return { icon: PersonIcon, verb: <>added {member} to {repo}</> };
    }
    case 'PublicEvent':
      return { icon: RepoIcon, verb: <>made {repo} public</> };
    default:
      return { icon: RepoIcon, verb: <>{g.type.replace(/Event$/, '').toLowerCase()} in {repo}</> };
  }
}

/** Issue/PR line inside a grouped feed item. */
function SubjectLine({ e }: { e: FeedEvent }) {
  const i = issueOf(e);
  if (!i) return null;
  const pr = isPrEvent(e);
  const comment = e.payload.comment as { body?: string } | undefined;
  const merged = (e.payload.pull_request as IssueRef | undefined)?.merged;
  return (
    <div className={styles.subject}>
      <Link to={issueLink(e)} className={styles.subjectLink}>
        <StateIcon issue={{ isPr: pr, state: (i.state as 'open' | 'closed') ?? 'open', stateReason: null, merged: !!merged, draft: i.draft }} size={14} />
        <span className={styles.subjectTitle}>{i.title}</span>
        <span className={styles.subjectNumber}>#{i.number}</span>
      </Link>
      {comment?.body && <p className={styles.commentSnippet}>{comment.body}</p>}
    </div>
  );
}

const MAX_LINES = 4;

export function FeedItem({ group }: { group: FeedGroup }) {
  const { icon: I, verb, tone } = describe(group);
  const e = group.events[0]!;
  const commits = group.type === 'PushEvent' ? group.events.flatMap((x) => (x.payload.commits as { sha: string; message: string }[] | undefined) ?? []) : [];
  const subjects = group.events.filter((x) => issueOf(x));
  return (
    <article className={styles.feedItem}>
      <Link to={`/${group.actor.login}`} className={styles.feedAvatar} aria-label={group.actor.login}>
        <Avatar user={{ login: group.actor.login, avatarUrl: group.actor.avatar_url }} size={28} />
        <span className={styles.feedIcon} data-tone={tone}>
          <I size={12} />
        </span>
      </Link>
      <div className={styles.feedBody}>
        <div className={styles.feedLine}>
          <Link to={`/${group.actor.login}`} className={styles.feedActor}>
            {group.actor.display_login ?? group.actor.login}
          </Link>{' '}
          {verb}
          <span className={styles.feedTime}>
            <RelativeTime date={e.created_at} short />
          </span>
        </div>
        {commits.length > 0 && (
          <ul className={styles.commits}>
            {commits.slice(0, MAX_LINES).map((c) => (
              <li key={c.sha}>
                <Link to={`/${group.repo}/commit/${c.sha}`} className={styles.sha}>
                  {c.sha.slice(0, 7)}
                </Link>{' '}
                <span className={styles.commitMsg}>{c.message.split('\n')[0]}</span>
              </li>
            ))}
            {commits.length > MAX_LINES && <li className={styles.moreLine}>{commits.length - MAX_LINES} more commits</li>}
          </ul>
        )}
        {subjects.slice(0, MAX_LINES).map((x) => (
          <SubjectLine key={x.id} e={x} />
        ))}
        {subjects.length > MAX_LINES && <div className={styles.moreLine}>and {subjects.length - MAX_LINES} more</div>}
      </div>
    </article>
  );
}

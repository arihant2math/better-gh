import type { CSSProperties, ReactNode } from 'react';
import type { Issue, Label, User } from '../sync/models';
import styles from './Badge.module.css';
import { cx } from './Button';
import {
  GitMergeIcon,
  GitPullRequestClosedIcon,
  GitPullRequestDraftIcon,
  GitPullRequestIcon,
  IssueClosedIcon,
  IssueOpenedIcon,
  SkipIcon,
} from './icons';

export function Counter({ children, accent }: { children: ReactNode; accent?: boolean }) {
  return <span className={cx(styles.counter, accent && styles.counterAccent)}>{children}</span>;
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className={styles.kbd}>{children}</kbd>;
}

/** Tag-like neutral badge ("Private", "Draft", "Bot"...). */
export function Tag({ children }: { children: ReactNode }) {
  return <span className={styles.tag}>{children}</span>;
}

const labelStyle = (color: string) => ({ '--c': `#${color}` }) as CSSProperties;

export function LabelPill({ label, size = 'md' }: { label: Pick<Label, 'name' | 'color' | 'description'>; size?: 'sm' | 'md' }) {
  return (
    <span className={cx(styles.label, size === 'sm' && styles.labelSm)} style={labelStyle(label.color)} title={label.description ?? undefined}>
      {label.name}
    </span>
  );
}

export function ColorDot({ color }: { color: string }) {
  return <span className={styles.dot} style={labelStyle(color)} />;
}

export type IssueVisualState = 'open' | 'closed' | 'not_planned' | 'merged' | 'draft' | 'pr_closed';

export function issueVisualState(i: Pick<Issue, 'isPr' | 'state' | 'stateReason' | 'merged' | 'draft'>): IssueVisualState {
  if (i.isPr) {
    if (i.merged) return 'merged';
    if (i.state === 'closed') return 'pr_closed';
    return i.draft ? 'draft' : 'open';
  }
  if (i.state === 'open') return 'open';
  return i.stateReason === 'not_planned' ? 'not_planned' : 'closed';
}

const STATE_ICON = {
  issue: { open: IssueOpenedIcon, closed: IssueClosedIcon, not_planned: SkipIcon },
  pr: { open: GitPullRequestIcon, draft: GitPullRequestDraftIcon, merged: GitMergeIcon, pr_closed: GitPullRequestClosedIcon },
} as const;

export function StateIcon({ issue, size = 16 }: { issue: Pick<Issue, 'isPr' | 'state' | 'stateReason' | 'merged' | 'draft'>; size?: number }) {
  const s = issueVisualState(issue);
  const I = issue.isPr ? STATE_ICON.pr[s as keyof typeof STATE_ICON.pr] : STATE_ICON.issue[s as keyof typeof STATE_ICON.issue];
  return <I size={size} className={styles[`icon-${s}`]} aria-label={s.replace('_', ' ')} />;
}

const STATE_TEXT: Record<IssueVisualState, string> = {
  open: 'Open',
  closed: 'Closed',
  not_planned: 'Closed',
  merged: 'Merged',
  draft: 'Draft',
  pr_closed: 'Closed',
};

const STATE_CLASS: Record<IssueVisualState, string> = {
  open: styles.open!,
  closed: styles.closed!,
  not_planned: styles.notPlanned!,
  merged: styles.merged!,
  draft: styles.draft!,
  pr_closed: styles.prClosed!,
};

export function StateBadge({ issue }: { issue: Pick<Issue, 'isPr' | 'state' | 'stateReason' | 'merged' | 'draft'> }) {
  const s = issueVisualState(issue);
  return (
    <span className={cx(styles.state, STATE_CLASS[s])}>
      <StateIcon issue={issue} size={16} />
      {STATE_TEXT[s]}
    </span>
  );
}

// ------------------------------------------------------------------ avatars

const AVATAR_COLORS = ['#e5484d', '#f76b15', '#d6a10e', '#30a46c', '#12a594', '#0090ff', '#3e63dd', '#6e56cf', '#ab4aba', '#d6409f', '#7d8590'];

function hash(s: string): number {
  let h = 0;
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0;
  return Math.abs(h);
}

export function Avatar({
  user,
  size = 20,
  square,
  title,
}: {
  user: Pick<User, 'login' | 'avatarUrl'> & { name?: string | null } | undefined | null;
  size?: number;
  square?: boolean;
  title?: string;
}) {
  const login = user?.login ?? '?';
  const initials = (user?.name || login)
    .replace(/\[bot\]$/, '')
    .split(/[\s-]+/)
    .map((p) => p[0])
    .join('')
    .slice(0, size >= 28 ? 2 : 1)
    .toUpperCase();
  const style: CSSProperties = {
    width: size,
    height: size,
    fontSize: Math.max(9, Math.round(size * 0.42)),
    background: user?.avatarUrl ? 'var(--bg-muted)' : AVATAR_COLORS[hash(login) % AVATAR_COLORS.length],
  };
  return (
    <span className={cx(styles.avatar, square && styles.avatarSquare)} style={style} title={title ?? login} aria-hidden={!title}>
      {user?.avatarUrl ? <img src={user.avatarUrl} alt="" loading="lazy" decoding="async" /> : initials}
    </span>
  );
}

export function AvatarStack({ users, size = 20, max = 3 }: { users: (User | undefined)[]; size?: number; max?: number }) {
  const list = users.filter((u): u is User => !!u);
  return (
    <span className={styles.avatarStack}>
      {list.slice(0, max).map((u) => (
        <Avatar key={u.login} user={u} size={size} title={u.login} />
      ))}
    </span>
  );
}

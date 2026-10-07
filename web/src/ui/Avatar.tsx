import type { CSSProperties } from 'react';
import type { User } from '../sync/models';
import styles from './Badge.module.css';
import { cx } from './Button';

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


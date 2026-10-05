/** Small pieces shared by the commits list and the single-commit page. */
import type { CommitStatusRollup } from '../../api/code';
import type { BrowsePerson } from '../../api/types';
import { Link } from '../../router';
import { Avatar } from '../../ui/Badge';
import { cx } from '../../ui/Button';
import { CheckIcon, DotFillIcon, XIcon } from '../../ui/icons';
import { toast } from '../../ui/Toast';
import { Tooltip } from '../../ui/Tooltip';
import { ciSummary } from './group';
import styles from './Commits.module.css';

export function copyText(text: string, what = 'Copied to clipboard'): void {
  const done = () => toast({ title: what });
  if (navigator.clipboard?.writeText) {
    navigator.clipboard.writeText(text).then(done, () => toast({ title: 'Copy failed', kind: 'error' }));
  } else toast({ title: 'Clipboard not available', kind: 'error' });
}

/** Green check / red x / yellow dot for a CI rollup; nothing when there is no CI. */
export function CiIcon({ status, size = 16 }: { status: CommitStatusRollup | undefined; size?: number }) {
  if (!status) return null;
  const failed = status.state === 'failure' || status.state === 'error';
  const pending = status.state === 'pending';
  const I = failed ? XIcon : pending ? DotFillIcon : CheckIcon;
  const cls = failed ? styles.ciFail : pending ? styles.ciPending : status.state === 'neutral' ? styles.ciNeutral : styles.ciOk;
  return (
    <Tooltip label={ciSummary(status)}>
      <span className={cx(styles.ci, cls)} tabIndex={0} aria-label={`CI ${status.state}: ${ciSummary(status)}`}>
        <I size={size} />
      </span>
    </Tooltip>
  );
}

/** Avatar + login (linked to the profile) or the raw git name. */
export function Person({ person, size = 20, avatar = true }: { person: Pick<BrowsePerson, 'name' | 'login' | 'avatar_url'>; size?: number; avatar?: boolean }) {
  const user = { login: person.login ?? person.name, avatarUrl: person.avatar_url ?? '', name: person.name };
  return (
    <span className={styles.person}>
      {avatar && <Avatar user={user} size={size} />}
      {person.login ? (
        <Link to={`/${person.login}`} className={styles.personName}>
          {person.login}
        </Link>
      ) : (
        <span className={styles.personName}>{person.name}</span>
      )}
    </span>
  );
}

/** Same author and committer (by login, else name/email)? */
export function samePerson(a: Pick<BrowsePerson, 'login' | 'name' | 'email'>, b: Pick<BrowsePerson, 'login' | 'name' | 'email'>): boolean {
  if (a.login || b.login) return a.login === b.login;
  return a.name === b.name && a.email === b.email;
}

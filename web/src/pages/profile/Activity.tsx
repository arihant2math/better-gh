import type { RestEvent } from '../../api/profile';
import { Link } from '../../router';
import { CommentIcon, GitBranchIcon, GitCommitIcon, GitPullRequestIcon, IssueOpenedIcon, PulseIcon, RepoForkedIcon, RepoIcon, StarIcon, TagIcon, TrashIcon, type Icon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import { summarizeEvent, type EventSummary } from './activity';
import styles from './ProfilePage.module.css';

const ICONS: Record<EventSummary['kind'], Icon> = {
  push: GitCommitIcon,
  create: RepoIcon,
  delete: TrashIcon,
  star: StarIcon,
  fork: RepoForkedIcon,
  issue: IssueOpenedIcon,
  pull: GitPullRequestIcon,
  comment: CommentIcon,
  release: TagIcon,
  other: PulseIcon,
};

/** Timeline of recent public events. */
export function Activity({ events }: { events: RestEvent[] }) {
  if (events.length === 0) return <p className={styles.muted}>No recent public activity.</p>;
  return (
    <ol className={styles.activity}>
      {events.map((e) => {
        const s = summarizeEvent(e);
        const I = s.kind === 'create' && s.text.startsWith('Created branch') ? GitBranchIcon : ICONS[s.kind];
        return (
          <li key={e.id} className={styles.activityItem}>
            <span className={styles.activityIcon}>
              <I size={14} />
            </span>
            <div className={styles.activityText}>
              {s.text} {s.repo && <Link to={`/${s.repo}${s.path ?? ''}`}>{s.repo}</Link>}
              {s.detail && <div className={styles.activityDetail}>{s.detail}</div>}
            </div>
            <span className={styles.activityTime}>
              <RelativeTime date={e.created_at} />
            </span>
          </li>
        );
      })}
    </ol>
  );
}

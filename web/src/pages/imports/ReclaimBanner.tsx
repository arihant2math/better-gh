import { useResource } from '../../api/cache';
import { MANNEQUIN_KEYS, listMyReclaims } from '../../api/mannequins';
import { Link } from '../../router';
import { Avatar } from '../../ui/Badge';
import { DownloadIcon } from '../../ui/icons';
import styles from '../invitations/Invitation.module.css';

/** Dashboard banner: pending invitations to claim a mannequin's imported contributions. */
export function ReclaimBanner() {
  const res = useResource(MANNEQUIN_KEYS.mine, listMyReclaims, { ttlMs: 60_000 });
  const pending = (res.data ?? []).filter((r) => r.status === 'pending');
  if (!pending.length) return null;
  return (
    <section className={styles.banner} aria-label="Imported contributions">
      <div className={styles.bannerHead}>
        <DownloadIcon size={16} />
        {pending.length === 1 ? 'Someone thinks these imported contributions are yours' : `${pending.length} invitations to claim imported contributions`}
      </div>
      <ul className={styles.bannerList}>
        {pending.map((r) => (
          <li key={r.id} className={styles.bannerRow}>
            <Avatar user={{ login: r.mannequin?.login ?? '', avatarUrl: r.mannequin?.avatar_url ?? '' }} size={20} />
            <span className={styles.bannerName}>
              <strong>{r.mannequin?.source_login ?? r.mannequin?.login}</strong>
              {r.mannequin?.source && <span className={styles.bannerRole}> · {r.mannequin.source}</span>}
            </span>
            <Link to="/settings/reclaims">Review</Link>
          </li>
        ))}
      </ul>
    </section>
  );
}

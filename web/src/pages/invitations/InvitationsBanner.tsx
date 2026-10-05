import { useResource } from '../../api/cache';
import { INVITE_KEYS, loadPendingInvitations } from '../../api/invitations';
import { Link } from '../../router';
import { Avatar } from '../../ui/Badge';
import { MailIcon } from '../../ui/icons';
import styles from './Invitation.module.css';
import { pendingItems } from './model';

/** Dashboard banner listing the viewer's pending org and repository invitations. */
export function InvitationsBanner() {
  const res = useResource(INVITE_KEYS.pending, loadPendingInvitations, { ttlMs: 30_000 });
  const items = res.data ? pendingItems(res.data.orgs, res.data.repos) : [];
  if (!items.length) return null;
  return (
    <section className={styles.banner} aria-label="Pending invitations">
      <div className={styles.bannerHead}>
        <MailIcon size={16} />
        {items.length === 1 ? 'You have a pending invitation' : `You have ${items.length} pending invitations`}
      </div>
      <ul className={styles.bannerList}>
        {items.map((i) => (
          <li key={i.key} className={styles.bannerRow}>
            <Avatar user={{ login: i.name, avatarUrl: i.avatarUrl }} size={20} square={i.kind === 'org'} />
            <span className={styles.bannerName}>
              {i.kind === 'org' ? 'Join the ' : 'Collaborate on '}
              <strong>{i.name}</strong>
              {i.kind === 'org' ? ' organization' : ''} <span className={styles.bannerRole}>· {i.role}</span>
            </span>
            <Link to={i.href}>View invitation</Link>
          </li>
        ))}
      </ul>
    </section>
  );
}

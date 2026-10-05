import type { ReactNode } from 'react';
import { ApiError } from '../../api/client';
import { Banner, errorMessage } from '../../components/settings/kit';
import { Avatar } from '../../ui/Badge';
import { Skeleton } from '../../ui/EmptyState';
import styles from './Invitation.module.css';

export { styles as invitationStyles };

export function InvitationCard({ children, label }: { children: ReactNode; label: string }) {
  return (
    <div className={styles.page}>
      <section className={styles.card} aria-label={label}>
        {children}
      </section>
    </div>
  );
}

export function CardSkeleton() {
  return (
    <InvitationCard label="Loading invitation">
      <Skeleton width={48} height={48} />
      <Skeleton width="70%" height={22} />
      <Skeleton width="50%" />
      <Skeleton height={72} />
    </InvitationCard>
  );
}

/** Inviter avatar → target avatar. */
export function AvatarPair({ from, to, square }: { from: { login: string; avatar_url: string } | null; to: { login: string; avatar_url: string }; square?: boolean }) {
  return (
    <div className={styles.avatars} aria-hidden>
      {from && (
        <>
          <Avatar user={{ login: from.login, avatarUrl: from.avatar_url }} size={48} />
          <span>+</span>
        </>
      )}
      <Avatar user={{ login: to.login, avatarUrl: to.avatar_url }} size={48} square={square} />
    </div>
  );
}

export const isNotFound = (e: unknown) => e instanceof ApiError && e.status === 404;

export function LoadError({ error }: { error: unknown }) {
  return (
    <div className={styles.error}>
      <Banner tone="danger">{errorMessage(error)}</Banner>
    </div>
  );
}

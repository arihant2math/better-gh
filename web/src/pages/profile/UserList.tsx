import { observer } from 'mobx-react-lite';
import { useMemo, useState, type ReactNode } from 'react';
import { Pill } from '../../components/settings/kit';
import { Link } from '../../router';
import { userByLogin } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { Box, EmptyState, Skeleton } from '../../ui/EmptyState';
import { PeopleIcon, SearchIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { VirtualList } from '../../ui/VirtualList';
import { FollowButton, useFollowState, useMyFollowing } from './follow';
import styles from './ProfilePage.module.css';
import type { SimpleUser } from '../../api/types';

export interface PersonItem {
  login: string;
  id: number;
  avatarUrl: string;
  name?: string | null;
  /** e.g. "Owner" for organization admins. */
  badge?: string;
}

export const personFromRest = (u: SimpleUser): PersonItem => ({ login: u.login, id: u.id, avatarUrl: u.avatar_url, name: null });

const PersonRow = observer(function PersonRow({ p, following }: { p: PersonItem; following: Set<string> | undefined }) {
  const synced = userByLogin(p.login);
  const name = p.name ?? synced?.name ?? null;
  const state = useFollowState(p.login, following ? following.has(p.login.toLowerCase()) : undefined);
  return (
    <div className={styles.userRow} role="listitem">
      <Link to={`/${p.login}`} tabIndex={-1} aria-hidden>
        <Avatar user={{ login: p.login, avatarUrl: p.avatarUrl || synced?.avatarUrl || '', name }} size={48} />
      </Link>
      <div className={styles.userRowText}>
        <div className={styles.userRowName}>
          <Link to={`/${p.login}`}>{name || p.login}</Link>
          {name && <span className={styles.muted}>{p.login}</span>}
          {p.badge && <Pill>{p.badge}</Pill>}
        </div>
      </div>
      <FollowButton state={state} />
    </div>
  );
});

/** Followers / following / organization people (with follow buttons and a filter). */
export function UserList({
  people,
  loading,
  error,
  emptyTitle,
  emptyBody,
  label,
}: {
  people: PersonItem[] | undefined;
  loading: boolean;
  error?: ReactNode;
  emptyTitle: string;
  emptyBody?: ReactNode;
  label: string;
}) {
  const following = useMyFollowing();
  const [q, setQ] = useState('');
  const shown = useMemo(() => {
    const s = q.trim().toLowerCase();
    return (people ?? []).filter((p) => !s || p.login.toLowerCase().includes(s) || (p.name ?? '').toLowerCase().includes(s));
  }, [people, q]);

  if (!people && loading)
    return (
      <div className={styles.skelStack} aria-busy="true" aria-label="Loading">
        {Array.from({ length: 4 }, (_, i) => (
          <div key={i} className={styles.userRow}>
            <Skeleton width={48} height={48} style={{ borderRadius: '50%' }} />
            <Skeleton width="30%" />
          </div>
        ))}
      </div>
    );
  if (!people && error) return <Box padded>{error}</Box>;
  if (!people || people.length === 0)
    return (
      <EmptyState icon={PeopleIcon} title={emptyTitle}>
        {emptyBody}
      </EmptyState>
    );

  return (
    <div>
      {people.length > 8 && (
        <div className={styles.listTools}>
          <Input type="search" leadingIcon={SearchIcon} placeholder="Find a person…" aria-label="Find a person" value={q} onChange={(e) => setQ(e.target.value)} />
        </div>
      )}
      {shown.length === 0 ? (
        <EmptyState icon={SearchIcon} title="No one matches">
          Try a different name.
        </EmptyState>
      ) : shown.length > 100 ? (
        <VirtualList
          className={`${styles.userRows} ${styles.virtual}`}
          items={shown}
          getKey={(p) => p.id}
          estimateSize={73}
          aria-label={label}
          renderItem={(p) => <PersonRow p={p} following={following} />}
        />
      ) : (
        <div className={styles.userRows} role="list" aria-label={label}>
          {shown.map((p) => (
            <PersonRow key={p.id} p={p} following={following} />
          ))}
        </div>
      )}
    </div>
  );
}

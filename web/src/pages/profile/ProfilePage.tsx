import { observer } from 'mobx-react-lite';
import { invalidate, useResource } from '../../api/cache';
import { getAccount, profileKeys } from '../../api/profile';
import { NotFound } from '../../app/NotFound';
import { useEffect, useReducer } from 'react';
import { navigate, useParams } from '../../router';
import { orgByLogin, userByLogin } from '../../sync/selectors';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { canonicalAccountUrl } from './canonical';
import { OrgProfile } from './OrgProfile';
import styles from './ProfilePage.module.css';
import { UserProfile } from './UserProfile';

/**
 * `/:owner` — user or organization profile. Decides the kind from the store
 * when the account is synced (instant), else from `GET /users/{owner}`.
 */
export default observer(function ProfilePage() {
  const { owner } = useParams<{ owner: string }>();
  const org = orgByLogin(owner);
  const user = org ? undefined : userByLogin(owner);
  const known = !!org || !!user;
  const [, retry] = useReducer((x: number) => x + 1, 0);
  // Unknown to the store: ask the server what it is (shared with UserProfile's header).
  const account = useResource(known ? null : profileKeys.account(owner), () => getAccount(owner));
  const a = account.data;
  // A renamed account: `GET /users/{old}` follows the 301 to the current login; show the canonical URL.
  useEffect(() => {
    if (!a) return;
    const to = canonicalAccountUrl(window.location, owner, a.login);
    if (to) navigate(to, { replace: true });
  }, [a, owner]);

  if (org) return <OrgProfile key={org.login} login={org.login} synced={org} />;
  if (user) return <UserProfile key={user.login} login={user.login} synced={user} />;
  if (a?.type === 'Organization') return <OrgProfile key={a.login} login={a.login} synced={undefined} />;
  if (a) return <UserProfile key={a.login} login={a.login} synced={undefined} />;
  if (account.error) {
    if ((account.error as { status?: number }).status === 404) return <NotFound what="account" />;
    return (
      <div className={styles.page}>
        <EmptyState
          title="Couldn’t load this profile"
          action={
            <Button
              onClick={() => {
                invalidate(profileKeys.account(owner));
                retry();
              }}
            >
              Retry
            </Button>
          }
        >
          {errorText(account.error)}
        </EmptyState>
      </div>
    );
  }
  return (
    <div className={styles.page} aria-busy="true" aria-label="Loading profile">
      <div className={styles.userLayout}>
        <div className={styles.sidebar}>
          <Skeleton width={240} height={240} style={{ borderRadius: '50%' }} />
          <Skeleton width="60%" height={24} />
          <Skeleton width="40%" />
        </div>
        <div className={styles.skelStack}>
          <Skeleton height={36} />
          <Skeleton height={120} />
          <Skeleton height={120} />
        </div>
      </div>
    </div>
  );
});

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : 'Something went wrong.';
}

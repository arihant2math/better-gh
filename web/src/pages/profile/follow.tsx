/**
 * Follow state shared by every follow button on profile pages. Follows aren't
 * synced, so the base state comes from REST (`/user/following/{u}` for the
 * profile header, `/user/following` for list rows) and optimistic overrides
 * live here for the session. Counters add `useFollowDelta` /
 * `useMyFollowingDelta` instead of refetching, so nothing flashes.
 */
import { useSyncExternalStore } from 'react';
import { useResource } from '../../api/cache';
import { checkFollowing, follow, listMyFollowing, unfollow } from '../../api/profile';
import { session } from '../../app/session';
import { errorMessage } from '../../components/settings/kit';
import { Button } from '../../ui/Button';
import { toast } from '../../ui/Toast';
import { onReset } from '../../api/reset';

interface Override {
  on: boolean;
  /** Server state when the user first clicked. */
  base: boolean;
}

const overrides = new Map<string, Override>();
const pending = new Set<string>();
const listeners = new Set<() => void>();
let version = 0;
const emit = () => {
  version++;
  listeners.forEach((l) => l());
};
onReset(() => {
  overrides.clear();
  pending.clear();
  emit();
});
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => listeners.delete(l);
};

function useOverrides(): number {
  return useSyncExternalStore(subscribe, () => version);
}

const key = (login: string) => login.toLowerCase();

export interface FollowState {
  login: string;
  /** Current state including the optimistic override (undefined: unknown yet). */
  following: boolean | undefined;
  busy: boolean;
}

/** Viewer's followed logins (lowercase), for list rows. */
export function useMyFollowing(): Set<string> | undefined {
  const viewer = session.user?.login;
  const { data } = useResource(viewer ? `profile:following-mine:${viewer}` : null, listMyFollowing);
  return data ? new Set(data.map((u) => key(u.login))) : undefined;
}

/** Follow state of `login` given the server's `base` answer. */
export function useFollowState(login: string, base: boolean | undefined): FollowState {
  useOverrides();
  const o = overrides.get(key(login));
  return { login, following: o ? o.on : base, busy: pending.has(key(login)) };
}

/** Profile-header variant: asks `/user/following/{login}` itself. */
export function useFollowCheck(login: string, enabled: boolean): FollowState {
  const { data } = useResource(enabled ? `profile:follows:${key(login)}` : null, () => checkFollowing(login));
  return useFollowState(login, data);
}

export async function setFollowing(login: string, on: boolean, base: boolean): Promise<void> {
  const k = key(login);
  const prev = overrides.get(k);
  overrides.set(k, { on, base: prev ? prev.base : base });
  pending.add(k);
  emit();
  try {
    await (on ? follow(login) : unfollow(login));
  } catch (e) {
    if (prev) overrides.set(k, prev);
    else overrides.delete(k);
    toast({ kind: 'error', title: `Could not ${on ? 'follow' : 'unfollow'} ${login}`, description: errorMessage(e) });
  } finally {
    pending.delete(k);
    emit();
  }
}

/** Change of `login`'s follower count caused by the viewer this session. */
export function useFollowDelta(login: string): number {
  useOverrides();
  const o = overrides.get(key(login));
  return o ? Number(o.on) - Number(o.base) : 0;
}

/** Change of the viewer's following count this session. */
export function useMyFollowingDelta(): number {
  useOverrides();
  let d = 0;
  for (const o of overrides.values()) d += Number(o.on) - Number(o.base);
  return d;
}

/** Follow / Unfollow button (optimistic). Renders nothing for the viewer themself or when signed out. */
export function FollowButton({ state, size = 'sm' }: { state: FollowState; size?: 'sm' | 'md' }) {
  const me = session.user;
  if (!me || key(me.login) === key(state.login)) return null;
  const on = !!state.following;
  return (
    <Button
      size={size}
      variant={on ? 'secondary' : 'primary'}
      disabled={state.following === undefined}
      aria-pressed={on}
      aria-label={`${on ? 'Unfollow' : 'Follow'} ${state.login}`}
      onClick={() => void setFollowing(state.login, !on, on)}
      block={size === 'md'}
    >
      {on ? 'Unfollow' : 'Follow'}
    </Button>
  );
}

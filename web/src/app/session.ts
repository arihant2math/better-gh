import { makeAutoObservable, runInAction } from 'mobx';
import { ApiError, api, requestSudo } from '../api/client';
import { resetClientState } from '../api/reset';
import { transport } from '../api/transport';
import { getBoot, isMockMode, setBoot, type BootData, type BootUser } from '../boot';
import { loginHref, navigate } from '../router';
import { hasSync, setSyncClient, sync } from '../sync';
import { SyncClient } from '../sync/client';
import { IdbPersistence, openPersistence } from '../sync/persistence';
import { toast } from '../ui/Toast';

function dbName(userId: number): string {
  return `${isMockMode() ? 'bgh-mock' : 'bgh'}-${userId}`;
}

/**
 * Who is signed in, and the lifecycle of their sync client.
 * `ready` flips once the local store has data (instant when IndexedDB is warm).
 */
class Session {
  user: BootUser | null = null;
  /** A sync client exists (the store can be read). */
  started = false;
  ready = false;

  constructor() {
    makeAutoObservable<Session, 'starting'>(this, { starting: false });
  }

  /** Read the (final) boot data. Called once by main.tsx. */
  init(): void {
    this.user = getBoot().user;
  }

  private starting: Promise<void> | null = null;

  /** Start the sync client for the signed-in user (idempotent). */
  start(): Promise<void> {
    if (!this.user || hasSync()) return Promise.resolve();
    this.starting ??= this.doStart(this.user).finally(() => (this.starting = null));
    return this.starting;
  }

  private async doStart(user: BootUser): Promise<void> {
    const persistence = await openPersistence(dbName(user.id));
    if (this.user?.id !== user.id) {
      persistence.close();
      return;
    }
    const client = new SyncClient({
      userId: user.id,
      transport: transport(),
      persistence,
      csrf: () => getBoot().csrf,
      hooks: {
        onRollback: (tx, message) => toast({ kind: 'error', title: `Couldn't save: ${tx.label}`, description: message }),
        onUnauthenticated: () => this.expired(),
        // Sudo 401s from queued writes (repo delete/transfer) prompt instead of logging out.
        onSudoRequired: requestSudo,
      },
    });
    setSyncClient(client);
    runInAction(() => (this.started = true));
    void client.ready.then(() => runInAction(() => (this.ready = true)));
    client.start().catch((err: unknown) => {
      console.error('[sync] start failed', err);
      toast({ kind: 'error', title: 'Could not load your workspace', description: String(err), duration: 0 });
    });
  }

  /**
   * Sign in. Resolves to `{ twoFactorToken }` when the account needs a
   * second factor (then call `verifyTwoFactor`), else `null`.
   */
  async login(login: string, password: string): Promise<{ twoFactorToken: string; methods: string[] } | null> {
    try {
      const boot = await api.post<BootData>('/_bgh/auth/login', { login, password });
      this.adopt(boot);
      return null;
    } catch (e) {
      const body =
        e instanceof ApiError && e.status === 401 ? (e.body as { twoFactorRequired?: boolean; twoFactorToken?: string; twoFactorMethods?: string[] } | null) : null;
      if (body?.twoFactorRequired && body.twoFactorToken) return { twoFactorToken: body.twoFactorToken, methods: body.twoFactorMethods ?? ['totp', 'recovery_code'] };
      throw e;
    }
  }

  /** Second step of a 2FA sign-in: TOTP or recovery code. */
  async verifyTwoFactor(twoFactorToken: string, code: string): Promise<void> {
    const boot = await api.post<BootData>('/_bgh/auth/2fa', { twoFactorToken, code });
    this.adopt(boot);
  }

  async signup(input: { login: string; email: string; password: string }): Promise<void> {
    const boot = await api.post<BootData>('/_bgh/auth/signup', input);
    this.adopt(boot);
  }

  /**
   * Second factor of a pending login that did not start on the login page
   * (SSO redirect to /login/two-factor): the legacy session endpoint sets
   * the cookie but answers with the user, so boot data is fetched after.
   */
  async completeTwoFactor(twoFactorToken: string, code: string): Promise<void> {
    await api.post('/_bgh/session/two_factor', { two_factor_token: twoFactorToken, code });
    await this.refreshBoot();
  }

  /** Re-read boot data (after a cookie changed out of band) and adopt it. */
  async refreshBoot(): Promise<BootData> {
    const boot = await api.get<BootData>('/_bgh/boot');
    if (boot.user?.id !== this.user?.id || boot.csrf !== getBoot().csrf) {
      if (this.user && boot.user?.id !== this.user.id) this.teardown();
      this.adopt(boot);
    }
    return boot;
  }

  /** Use fresh boot data (sign-in responses): updates CSRF + user and starts sync. */
  /** Patch the signed-in user's display fields (after profile / avatar edits). */
  updateUser(patch: Partial<Pick<BootUser, 'name' | 'avatarUrl' | 'login' | 'twoFactorSetupRequired'>>): void {
    if (!this.user) return;
    this.user = { ...this.user, ...patch };
    setBoot({ ...getBoot(), user: this.user });
  }

  adopt(boot: BootData) {
    // The SW's cached shell embeds the previous boot data (e.g. `user: null`
    // from the login page); drop it so the next reload renders this session.
    dropShellCache();
    setBoot(boot);
    runInAction(() => {
      this.user = boot.user;
      this.ready = false;
    });
    void this.start();
  }

  async logout(): Promise<void> {
    const user = this.user;
    try {
      await api.post('/_bgh/auth/logout');
    } catch {
      /* best effort */
    }
    this.teardown();
    if (user) await IdbPersistence.destroy(dbName(user.id)).catch(() => undefined);
    dropShellCache();
    navigate('/login');
  }

  /** Session expired server-side (401 / WS 4001). */
  expired(): void {
    if (!this.user) return;
    this.teardown();
    dropShellCache();
    toast({ title: 'Your session expired', description: 'Sign in again to continue.' });
    navigate(loginHref());
  }

  /**
   * End the viewer's session client-side (logout, expiry, account switch):
   * stop sync and drop every per-viewer cache (`api/reset`) before the next
   * sign-in can render anything.
   */
  private teardown() {
    resetClientState();
    if (hasSync()) {
      sync().stop();
      setSyncClient(null);
    }
    setBoot({ ...getBoot(), user: null });
    runInAction(() => {
      this.user = null;
      this.started = false;
      this.ready = false;
    });
  }
}

export const session = new Session();

/**
 * Ask the service worker to forget its cached app shell: the shell embeds
 * boot data (user, CSRF token), which is wrong once the session changes
 * without a navigation (sign-in, sign-out, expiry).
 */
export function dropShellCache(): void {
  try {
    navigator.serviceWorker?.controller?.postMessage({ type: 'logout' });
  } catch {
    /* no service worker */
  }
}

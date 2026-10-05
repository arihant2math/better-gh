import { makeAutoObservable, runInAction } from 'mobx';
import { api } from '../api/client';
import { transport } from '../api/transport';
import { getBoot, isMockMode, setBoot, type BootData, type BootUser } from '../boot';
import { navigate } from '../router';
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
    makeAutoObservable(this);
  }

  /** Read the (final) boot data. Called once by main.tsx. */
  init(): void {
    this.user = getBoot().user;
  }

  async start(): Promise<void> {
    const user = this.user;
    if (!user || hasSync()) return;
    const persistence = await openPersistence(dbName(user.id));
    const client = new SyncClient({
      userId: user.id,
      transport: transport(),
      persistence,
      csrf: () => getBoot().csrf,
      hooks: {
        onRollback: (tx, message) => toast({ kind: 'error', title: `Couldn't save: ${tx.label}`, description: message }),
        onUnauthenticated: () => this.expired(),
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

  async login(login: string, password: string): Promise<void> {
    const boot = await api.post<BootData>('/_bgh/auth/login', { login, password });
    this.adopt(boot);
  }

  async signup(input: { login: string; email: string; password: string }): Promise<void> {
    const boot = await api.post<BootData>('/_bgh/auth/signup', input);
    this.adopt(boot);
  }

  private adopt(boot: BootData) {
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
    navigator.serviceWorker?.controller?.postMessage({ type: 'logout' });
    navigate('/login');
  }

  /** Session expired server-side (401 / WS 4001). */
  expired(): void {
    if (!this.user) return;
    this.teardown();
    toast({ title: 'Your session expired', description: 'Sign in again to continue.' });
    navigate(`/login?return_to=${encodeURIComponent(location.pathname)}`);
  }

  private teardown() {
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

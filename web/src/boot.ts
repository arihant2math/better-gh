/**
 * Boot data inlined by the server into index.html (docs/SYNC_PROTOCOL.md §9)
 * so the first render needs no round trip to know who the viewer is.
 */
export interface BootUser {
  id: number;
  login: string;
  name: string | null;
  avatarUrl: string;
  /** Site administrator (optional; fetched from `/api/v3/user` when absent). */
  siteAdmin?: boolean;
  /** The site requires 2FA and this account has none (P36): only setup is allowed. */
  twoFactorSetupRequired?: boolean;
}

export interface BootData {
  user: BootUser | null;
  csrf: string;
  config: {
    siteName: string;
    signupEnabled: boolean;
    version: string;
  };
  /** Server time the boot data was rendered. */
  ts?: string;
}

declare global {
  interface Window {
    __BGH_BOOT__?: BootData;
  }
}

const DEFAULT_BOOT: BootData = {
  user: null,
  csrf: '',
  config: { siteName: 'Better GitHub', signupEnabled: true, version: 'dev' },
};

let boot: BootData = (typeof window !== 'undefined' && window.__BGH_BOOT__) || DEFAULT_BOOT;

export function getBoot(): BootData {
  return boot;
}

export function setBoot(next: BootData): void {
  boot = next;
}

/** Boot data older than this (e.g. a shell served by the service worker) is refreshed in the background. */
export const BOOT_MAX_AGE_MS = 5 * 60_000;

export function bootIsStale(b: BootData = boot): boolean {
  if (!b.ts) return !b.user && !b.csrf; // no inline boot at all (dev server)
  return Date.now() - Date.parse(b.ts) > BOOT_MAX_AGE_MS;
}

/** Mock mode: `?mock` (persisted for the tab session), `?mock=0` to leave, or `VITE_MOCK=1`. */
export function isMockMode(): boolean {
  if (typeof window === 'undefined') return false;
  const params = new URLSearchParams(window.location.search);
  try {
    if (params.has('mock')) {
      const on = params.get('mock') !== '0';
      sessionStorage.setItem('bgh.mock', on ? '1' : '0');
      return on;
    }
    const stored = sessionStorage.getItem('bgh.mock');
    if (stored) return stored === '1';
  } catch {
    /* storage disabled */
  }
  return import.meta.env.VITE_MOCK === '1';
}

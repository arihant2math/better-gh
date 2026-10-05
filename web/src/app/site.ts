import { makeAutoObservable, runInAction } from 'mobx';
import { api } from '../api/client';
import { getBoot } from '../boot';

/** `GET /_bgh/site` (public): banner, maintenance and sign-in options. */
export interface PublicSiteInfo {
  site_name: string;
  announcement: { message: string; expires_at: string | null; user_dismissible: boolean } | null;
  maintenance: { enabled: boolean; message: string | null; scheduled_at: string | null };
  signup_policy: 'open' | 'invite' | 'closed';
  password_login: boolean;
  oidc_providers: { name: string; display_name: string }[];
}

const DISMISS_KEY = 'bgh.announcement.dismissed';
const REFRESH_MS = 5 * 60_000;

/** Stable id of an announcement (dismissals are per message). */
export function announcementId(message: string): string {
  let h = 5381;
  for (let i = 0; i < message.length; i++) h = ((h << 5) + h + message.charCodeAt(i)) | 0;
  return (h >>> 0).toString(36);
}

/**
 * Instance-wide info for every signed-in page: the announcement banner, the
 * maintenance banner and whether the viewer is a site administrator (for the
 * "Site admin" menu entry). Loaded in the background after sign-in; nothing
 * waits on it.
 */
class SiteState {
  info: PublicSiteInfo | null = null;
  /** `null` until known. */
  viewerSiteAdmin: boolean | null = null;
  dismissed: string | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;

  constructor() {
    makeAutoObservable<SiteState, 'timer'>(this, { timer: false });
    try {
      this.dismissed = localStorage.getItem(DISMISS_KEY);
    } catch {
      /* storage disabled */
    }
  }

  /** Start loading (idempotent); called once the session has a user. */
  start(): void {
    const boot = getBoot().user as ({ siteAdmin?: boolean } & object) | null;
    if (boot && typeof boot.siteAdmin === 'boolean') this.viewerSiteAdmin = boot.siteAdmin;
    if (this.timer) return;
    void this.refresh();
    if (this.viewerSiteAdmin === null) void this.loadViewer();
    this.timer = setInterval(() => void this.refresh(), REFRESH_MS);
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
    this.viewerSiteAdmin = null;
  }

  async refresh(): Promise<void> {
    try {
      const info = await api.get<PublicSiteInfo>('/_bgh/site');
      if (info && typeof info === 'object' && 'maintenance' in info) runInAction(() => (this.info = info));
    } catch {
      /* older server or mock backend: no banners */
    }
  }

  private async loadViewer(): Promise<void> {
    try {
      const me = await api.get<{ site_admin?: boolean }>('/api/v3/user');
      runInAction(() => (this.viewerSiteAdmin = !!me?.site_admin));
    } catch {
      runInAction(() => (this.viewerSiteAdmin = false));
    }
  }

  /** The announcement to show now (not expired, not dismissed). */
  get announcement(): PublicSiteInfo['announcement'] {
    const a = this.info?.announcement;
    if (!a?.message) return null;
    if (a.expires_at && Date.parse(a.expires_at) <= Date.now()) return null;
    if (a.user_dismissible && this.dismissed === announcementId(a.message)) return null;
    return a;
  }

  dismissAnnouncement(): void {
    const a = this.info?.announcement;
    if (!a) return;
    this.dismissed = announcementId(a.message);
    try {
      localStorage.setItem(DISMISS_KEY, this.dismissed);
    } catch {
      /* ignore */
    }
  }
}

export const site = new SiteState();

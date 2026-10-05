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
  /** Site admins may still use their password when `password_login` is off. */
  password_login_admin_exempt?: boolean;
  /** LDAP sign-in (directory passwords) is enabled. */
  ldap?: boolean;
  oidc_providers: { name: string; display_name: string }[];
  /** SAML single sign-on; `login_url` takes `?return_to=`. */
  saml?: { display_name: string; login_url: string } | null;
  /** Sign-in required for everything (absent on older servers). */
  private_mode?: boolean;
  /** Site policy for repository visibility (absent on older servers). */
  repository_visibilities?: { allowed: RepoVisibility[]; default_user: RepoVisibility; default_org: RepoVisibility };
}

export type RepoVisibility = 'public' | 'internal' | 'private';
const ALL_VISIBILITIES: RepoVisibility[] = ['public', 'internal', 'private'];

/**
 * Visibilities a new repository of a user (or organization) owner may get
 * under the site policy, and the default to preselect.
 */
export function visibilityPolicy(info: PublicSiteInfo | null, isOrg: boolean): { allowed: RepoVisibility[]; preferred: RepoVisibility } {
  const policy = info?.repository_visibilities;
  const allowed = (policy?.allowed ?? ALL_VISIBILITIES).filter((v) => isOrg || v !== 'internal');
  const wanted = isOrg ? policy?.default_org : policy?.default_user;
  const preferred = wanted && allowed.includes(wanted) ? wanted : allowed.includes('public') ? 'public' : (allowed[allowed.length - 1] ?? 'private');
  return { allowed, preferred };
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
export class SiteState {
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

  /** Some banner should be visible (the banner chunk is loaded only then). */
  get hasBanner(): boolean {
    const m = this.info?.maintenance;
    return !!this.announcement || !!m?.enabled || (!!m?.scheduled_at && Date.parse(m.scheduled_at) > Date.now());
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

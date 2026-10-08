import { useState } from 'react';
import { KEYS, listSessions, revokeOtherSessions, revokeSession, useEditableResource, type SessionInfo } from '@/api/userSettings';
import { session as appSession } from '@/app/session';
import { Banner, ConfirmDialog, ItemList, ItemRow, PageHeader, Pill, Section, errorMessage } from '@/components/settings/kit';
import { Button } from '@/ui/Button';
import { Skeleton } from '@/ui/EmptyState';
import { DeviceDesktopIcon, DeviceMobileIcon, TerminalIcon, type Icon } from '@/ui/icons';
import { RelativeTime } from '@/ui/RelativeTime';
import { toast } from '@/ui/Toast';
import styles from './userSettings.module.css';

export interface ParsedAgent {
  browser: string;
  os: string;
  kind: 'desktop' | 'mobile' | 'cli';
}

/** Small user-agent parser: enough to tell sessions apart ("Chrome on macOS"). */
export function parseUserAgent(ua: string | null | undefined): ParsedAgent {
  if (!ua) return { browser: 'Unknown browser', os: 'unknown device', kind: 'desktop' };
  const cli = /^(GitHub CLI|git\/|curl\/|Wget\/|python-requests|Go-http-client|node-fetch|undici)/i.exec(ua);
  if (cli) {
    const name = /^GitHub CLI/i.test(ua) ? 'GitHub CLI' : ua.split(/[/ ]/)[0]!;
    return { browser: name, os: 'command line', kind: 'cli' };
  }
  const browser = /Edg(e|A|iOS)?\//.test(ua)
    ? 'Edge'
    : /OPR\/|Opera/.test(ua)
      ? 'Opera'
      : /Firefox\/|FxiOS\//.test(ua)
        ? 'Firefox'
        : /Chrome\/|CriOS\//.test(ua) && !/Chromium\//.test(ua)
          ? 'Chrome'
          : /Chromium\//.test(ua)
            ? 'Chromium'
            : /Safari\//.test(ua)
              ? 'Safari'
              : 'Unknown browser';
  const os = /iPhone|iPad|iPod/.test(ua)
    ? /iPad/.test(ua)
      ? 'iPad'
      : 'iPhone'
    : /Android/.test(ua)
      ? 'Android'
      : /Mac OS X|Macintosh/.test(ua)
        ? 'macOS'
        : /Windows/.test(ua)
          ? 'Windows'
          : /CrOS/.test(ua)
            ? 'ChromeOS'
            : /Linux|X11/.test(ua)
              ? 'Linux'
              : 'unknown device';
  const kind = /Mobi|iPhone|iPad|Android/.test(ua) ? 'mobile' : 'desktop';
  return { browser, os, kind };
}

const ICONS: Record<ParsedAgent['kind'], Icon> = { desktop: DeviceDesktopIcon, mobile: DeviceMobileIcon, cli: TerminalIcon };

export default function SessionSettings() {
  const res = useEditableResource(KEYS.sessions, listSessions);
  const [confirmAll, setConfirmAll] = useState(false);
  const [revoking, setRevoking] = useState<SessionInfo | null>(null);
  const list = res.data;
  const others = (list ?? []).filter((s) => !s.current);

  const revoke = async (s: SessionInfo) => {
    // Optimistic removal; restored on failure.
    res.update((l) => l.filter((x) => x.id !== s.id));
    try {
      await revokeSession(s.id);
      toast({ kind: 'success', title: 'Session revoked' });
      if (s.current) appSession.expired();
    } catch (e) {
      res.update((l) => [...l, s].sort((a, b) => b.last_seen_at.localeCompare(a.last_seen_at)));
      toast({ kind: 'error', title: errorMessage(e) });
    }
  };

  return (
    <>
      <PageHeader
        title="Sessions"
        description="Devices that are signed in to your account. Revoke any session you don't recognize."
        actions={
          <Button variant="danger" disabled={!others.length} onClick={() => setConfirmAll(true)}>
            Sign out all other sessions
          </Button>
        }
      />
      <Section title="Web sessions">
        {res.error && !list ? (
          <Banner tone="danger">{errorMessage(res.error)}</Banner>
        ) : !list ? (
          <Skeleton height={160} />
        ) : (
          <ItemList aria-label="Sessions" empty="No active sessions.">
            {list.map((s) => {
              const a = parseUserAgent(s.user_agent);
              return (
                <ItemRow
                  key={s.id}
                  icon={ICONS[a.kind]}
                  className={styles.sessionRow}
                  title={
                    <>
                      <span data-testid="session-title">
                        {a.browser} on {a.os}
                      </span>
                      {s.current && <Pill tone="success">Your current session</Pill>}
                    </>
                  }
                  meta={
                    <>
                      {s.ip ?? 'Unknown IP'} · {s.current ? 'Active now' : <>Last active <RelativeTime date={s.last_seen_at} /></>} · Signed in{' '}
                      <RelativeTime date={s.created_at} />
                    </>
                  }
                  actions={
                    s.current ? null : (
                      <Button size="sm" onClick={() => setRevoking(s)}>
                        Revoke
                      </Button>
                    )
                  }
                >
                  {s.user_agent && (
                    <div className={styles.ua} title={s.user_agent}>
                      {s.user_agent}
                    </div>
                  )}
                </ItemRow>
              );
            })}
          </ItemList>
        )}
      </Section>
      <ConfirmDialog
        open={!!revoking}
        onClose={() => setRevoking(null)}
        title="Revoke session?"
        confirmLabel="Revoke session"
        onConfirm={() => {
          const s = revoking;
          if (s) void revoke(s);
        }}
      >
        <p>The device will be signed out immediately.</p>
      </ConfirmDialog>
      <ConfirmDialog
        open={confirmAll}
        onClose={() => setConfirmAll(false)}
        title="Sign out all other sessions?"
        confirmLabel="Sign out other sessions"
        onConfirm={async () => {
          const prev = list ?? [];
          res.update((l) => l.filter((x) => x.current));
          try {
            await revokeOtherSessions();
            toast({ kind: 'success', title: `Signed out ${others.length} other session${others.length === 1 ? '' : 's'}` });
          } catch (e) {
            res.update(() => prev);
            throw e;
          }
        }}
      >
        <p>Every other device and browser signed in to your account will be signed out. This session stays signed in.</p>
      </ConfirmDialog>
    </>
  );
}

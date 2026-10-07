import { describe, expect, it } from 'vitest';
import type { Command } from './commands';
import { commandScore, rankCommands, strictScore } from './paletteRank';

const cmd = (id: string, title: string, group: string, keywords?: string): Command => ({ id, title, group, keywords, run: () => {} });

// Registration order as in Shell.tsx: navigation first, site admin last.
const CMDS: Command[] = [
  cmd('nav.home', 'Go to Home', 'Navigation'),
  cmd('nav.inbox', 'Go to Inbox', 'Navigation', 'notifications'),
  cmd('nav.pulls', 'Go to pull requests', 'Navigation', 'reviews prs'),
  cmd('issue.new', 'New issue', 'Issues'),
  cmd('ui.theme', 'Toggle dark mode', 'Preferences', 'theme light dark'),
  cmd('ui.sidebar', 'Toggle sidebar', 'Preferences'),
  cmd('repo.new', 'Create new repository', 'Create', 'new repo'),
  cmd('nav.profile', 'Go to your profile', 'Navigation'),
  cmd('settings.blocked', 'Settings: Blocked users', 'Settings'),
  cmd('settings.oauth', 'Settings: OAuth apps', 'Settings'),
  cmd('auth.logout', 'Sign out', 'Account'),
  cmd('admin.hooks', 'Site admin: Global webhooks', 'Site admin'),
  cmd('admin.runners', 'Site admin: Runners', 'Site admin', 'actions self-hosted queue runner groups'),
];
const ids = (r: { command: Command }[]) => r.map((x) => x.command.id);

describe('strictScore', () => {
  it('accepts contiguous and word-start segment matches', () => {
    expect(strictScore('glo', 'site admin: global webhooks')).toBeGreaterThan(1000);
    expect(strictScore('newiss', 'new issue')).toBeGreaterThan(0);
    expect(strictScore('gtp', 'go to pull requests')).toBeGreaterThan(0);
  });
  it('rejects scattered subsequences', () => {
    expect(strictScore('glo', 'toggle dark mode')).toBe(0);
    expect(strictScore('glo', 'go to your profile')).toBe(0);
    expect(strictScore('glo', 'settings: blocked users')).toBe(0);
  });
  it('ranks prefix > word start > mid-word > segments', () => {
    const t = 'go to pull requests';
    expect(strictScore('go', t)).toBeGreaterThan(strictScore('pull', t));
    expect(strictScore('pull', t)).toBeGreaterThan(strictScore('ull', t));
    expect(strictScore('ull', t)).toBeGreaterThan(strictScore('gtp', t));
  });
});

describe('commandScore', () => {
  it('requires every token to match', () => {
    expect(commandScore('toggle dark', CMDS[4]!)).toBeGreaterThan(0);
    expect(commandScore('toggle xyz', CMDS[4]!)).toBe(0);
  });
  it('matches keywords and group but prefers the title', () => {
    expect(commandScore('notif', CMDS[1]!)).toBeGreaterThan(0);
    expect(commandScore('inbox', CMDS[1]!)).toBeGreaterThan(commandScore('notif', CMDS[1]!));
  });
});

describe('rankCommands', () => {
  it('empty query outside > mode shows the default sections only, navigation first', () => {
    const r = ids(rankCommands(CMDS, '', false));
    expect(r.slice(0, 4)).toEqual(['nav.home', 'nav.inbox', 'nav.pulls', 'nav.profile']);
    expect(r).toContain('issue.new');
    expect(r.some((id) => id.startsWith('admin.') || id.startsWith('settings.') || id === 'auth.logout')).toBe(false);
  });
  it('> mode lists all sections with Navigation first and Site admin last', () => {
    const r = rankCommands(CMDS, '', true);
    expect(r).toHaveLength(CMDS.length);
    const groups = [...new Set(r.map((x) => x.command.group))];
    expect(groups[0]).toBe('Navigation');
    expect(groups.at(-1)).toBe('Site admin');
    expect(groups.indexOf('Settings')).toBeGreaterThan(groups.indexOf('Preferences'));
  });
  it('glo only matches global webhooks', () => {
    expect(ids(rankCommands(CMDS, 'glo', false))).toEqual(['admin.hooks']);
  });
  it('orders by score, then section', () => {
    const r = ids(rankCommands(CMDS, 'toggle', false));
    expect(r).toEqual(['ui.sidebar', 'ui.theme']); // equal prefix match: shorter title first
    expect(ids(rankCommands(CMDS, 'new', false))[0]).toBe('issue.new');
  });
});

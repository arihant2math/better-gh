/**
 * Command ranking for the command palette: strict matching (contiguous or
 * word-start segments, no scattered subsequences) and a section order so
 * everyday commands come before settings and site admin.
 */
import type { Command } from './commands';

/** Section order; sections not listed (page-contextual ones) sit at CONTEXTUAL. */
const SECTION_TIER: Record<string, number> = {
  Navigation: 0,
  Create: 1,
  Issues: 1,
  Repository: 2,
  Search: 2,
  Preferences: 3,
  Help: 3,
  Settings: 4,
  Account: 5,
  'Site admin': 6,
};
const CONTEXTUAL = 2;
/** Highest tier shown in the default (empty query, not `>`) list. */
const DEFAULT_MAX_TIER = CONTEXTUAL;

function sectionTier(group: string): number {
  return SECTION_TIER[group] ?? CONTEXTUAL;
}

const BOUNDARY = /[\s/_\-.#:]/;
const isWordStart = (t: string, i: number) => i === 0 || BOUNDARY.test(t[i - 1]!);

/**
 * Strict match of one query token against `text` (both lowercase). Returns
 * 0 for no match. Contiguous substrings score highest (prefix, then word
 * start, then mid-word); otherwise the token must split into runs that each
 * begin at a word start, in order ("newiss" → "New ISSue", "gtp" → "Go To
 * Pull requests").
 */
export function strictScore(token: string, text: string): number {
  if (!token) return 1;
  const direct = text.indexOf(token);
  if (direct >= 0) {
    return 1000 + (direct === 0 ? 400 : 0) + (isWordStart(text, direct) ? 200 : 0) - Math.min(text.length, 200) / 4;
  }
  const runs = segments(token, 0, text, 0, new Map());
  return runs ? 500 - runs * 20 - Math.min(text.length, 200) / 4 : 0;
}

/**
 * Fewest word-start runs covering token[qi..] within text[ti..], or 0 if
 * impossible. Memoized on (qi, ti), so at most |q|·|t| states.
 */
function segments(q: string, qi: number, t: string, ti: number, memo: Map<number, number>): number {
  if (qi === q.length) return 0;
  const key = qi * (t.length + 1) + ti;
  const cached = memo.get(key);
  if (cached !== undefined) return cached;
  const best = searchSegments(q, qi, t, ti, memo);
  memo.set(key, best);
  return best;
}

function searchSegments(q: string, qi: number, t: string, ti: number, memo: Map<number, number>): number {
  let best = 0;
  for (let j = ti; j < t.length; j++) {
    if (t[j] !== q[qi] || !isWordStart(t, j)) continue;
    // Try the longest run first: it usually needs the fewest segments.
    let len = 0;
    while (qi + len < q.length && t[j + len] === q[qi + len]) len++;
    for (let l = len; l >= 1; l--) {
      if (qi + l === q.length) return 1;
      const rest = segments(q, qi + l, t, j + l, memo);
      if (rest && (!best || rest + 1 < best)) best = rest + 1;
    }
    if (best === 2) return best;
  }
  return best;
}

/** Score a command for `query`: every space-separated token must match strictly. */
export function commandScore(query: string, c: Pick<Command, 'title' | 'keywords' | 'group'>): number {
  const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!tokens.length) return 1;
  const title = c.title.toLowerCase();
  const keywords = (c.keywords ?? '').toLowerCase();
  const group = c.group.toLowerCase();
  let total = 0;
  for (const tok of tokens) {
    const s = Math.max(strictScore(tok, title), keywords ? strictScore(tok, keywords) * 0.6 : 0, strictScore(tok, group) * 0.5);
    if (!s) return 0;
    total += s;
  }
  return total;
}

export interface RankedCommand {
  command: Command;
  score: number;
}

/**
 * Rank commands for the palette. `cmds` is in registration order.
 * With an empty query: section order, and outside `>` mode only the
 * default sections (navigation, create, page-contextual). In `>` mode
 * results are grouped by section (Navigation first), best match first
 * within a section; otherwise best match first, section as tiebreak.
 */
export function rankCommands(cmds: readonly Command[], query: string, commandMode: boolean): RankedCommand[] {
  const scored = cmds
    .map((command, order) => ({ command, order, tier: sectionTier(command.group), score: commandScore(query, command) }))
    .filter((x) => x.score > 0 && (query || commandMode || x.tier <= DEFAULT_MAX_TIER));
  const groupRank = new Map<string, number>();
  if (commandMode) {
    // Sections ordered by tier, then by their best score, then first registration.
    const best = new Map<string, { tier: number; score: number; order: number }>();
    for (const x of scored) {
      const b = best.get(x.command.group);
      if (!b) best.set(x.command.group, { tier: x.tier, score: x.score, order: x.order });
      else if (x.score > b.score) b.score = x.score;
    }
    const ordered = [...best.entries()].sort(([, a], [, b]) => a.tier - b.tier || b.score - a.score || a.order - b.order);
    ordered.forEach(([g], i) => groupRank.set(g, i));
  }
  const rank = (x: (typeof scored)[number]) => groupRank.get(x.command.group) ?? 0;
  scored.sort((a, b) => rank(a) - rank(b) || b.score - a.score || a.tier - b.tier || a.order - b.order);
  return scored.map(({ command, score }) => ({ command, score }));
}

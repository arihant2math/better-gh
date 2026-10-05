/**
 * Reference scanning for rendered text: a port of the server's
 * `bgh_core::markdown::scan` (crates/bgh-core/src/markdown.rs) so issue
 * bodies render the same in the client and in API `body_html` / emails.
 * Parity is enforced by the shared corpus in `testdata/markdown/`.
 */

/** A repository autolink reference (`GET /_bgh/repos/{o}/{r}/autolinks`). */
export interface AutolinkRule {
  key_prefix: string;
  /** Contains `<num>`. */
  url_template: string;
  /** Ids are `[A-Za-z0-9]+` (else digits only). */
  is_alphanumeric: boolean;
}

export interface ScanContext {
  /** Link prefix: `''` for app-relative links, or an absolute base URL. */
  base: string;
  /** `[owner, repo]` for `#12`, `GH-12` and SHAs. */
  repo?: readonly [string, string];
  /** Longest prefix first. */
  autolinks?: readonly AutolinkRule[];
  /** gemoji table (shortcode → emoji), lazy-loaded. */
  emoji?: Readonly<Record<string, string>> | null;
  /** Link references and mentions (emoji always apply). */
  references?: boolean;
}

export type Segment =
  | { kind: 'text'; text: string }
  | { kind: 'link'; url: string; text: string; cls: string }
  | { kind: 'emoji'; name: string; emoji: string };

const isAlnum = (c: string | undefined) => c !== undefined && /^[A-Za-z0-9]$/.test(c);
const isWord = (c: string | undefined) => c !== undefined && (isAlnum(c) || c === '_');
const isLogin = (c: string | undefined) => c !== undefined && (isAlnum(c) || c === '-');
const isRepoChar = (c: string | undefined) => c !== undefined && (isAlnum(c) || c === '-' || c === '_' || c === '.');
const isDigit = (c: string | undefined) => c !== undefined && c >= '0' && c <= '9';
const isHex = (c: string | undefined) => c !== undefined && /^[0-9a-fA-F]$/.test(c);

/** Split text into plain segments, reference links and emoji. */
export function scan(text: string, ctx: ScanContext): Segment[] {
  const out: Segment[] = [];
  const base = ctx.base;
  const refs = ctx.references !== false;
  const n = text.length;
  let plainStart = 0;
  let i = 0;
  const push = (start: number, end: number, seg: Segment) => {
    if (plainStart < start) out.push({ kind: 'text', text: text.slice(plainStart, start) });
    out.push(seg);
    plainStart = end;
  };
  while (i < n) {
    const prev = text[i - 1];
    const prevOk = i === 0 || (!isWord(prev) && prev !== '/' && prev !== '@');
    const c = text[i];
    // :emoji:
    if (c === ':' && ctx.emoji) {
      let j = i + 1;
      while (j < n && (isWord(text[j]) || text[j] === '+' || text[j] === '-')) j++;
      if (j > i + 1 && text[j] === ':') {
        const name = text.slice(i + 1, j);
        const e = Object.hasOwn(ctx.emoji, name) ? ctx.emoji[name] : undefined;
        if (e) {
          push(i, j + 1, { kind: 'emoji', name, emoji: e });
          i = j + 1;
          continue;
        }
      }
    }
    // custom autolinks
    if (refs && prevOk && ctx.autolinks?.length) {
      let hit: [number, string] | null = null;
      for (const rule of ctx.autolinks) {
        const p = rule.key_prefix;
        const end = i + p.length;
        if (!p || end >= n || text.slice(i, end).toLowerCase() !== p.toLowerCase()) continue;
        let j = end;
        while (j < n && (isDigit(text[j]) || (rule.is_alphanumeric && isAlnum(text[j])))) j++;
        if (j > end && (j === n || !isWord(text[j]))) {
          hit = [j, rule.url_template.split('<num>').join(text.slice(end, j))];
          break;
        }
      }
      if (hit) {
        push(i, hit[0], { kind: 'link', url: hit[1], text: text.slice(i, hit[0]), cls: 'autolink' });
        i = hit[0];
        continue;
      }
    }
    // @mention or @org/team
    if (refs && c === '@' && prevOk && isAlnum(text[i + 1])) {
      let j = i + 1;
      while (j < n && isLogin(text[j]) && j - i <= 39) j++;
      const login = text.slice(i + 1, j).replace(/-+$/, '');
      j = i + 1 + login.length;
      if (text[j] === '/' && isAlnum(text[j + 1])) {
        let k = j + 1;
        while (k < n && (isLogin(text[k]) || text[k] === '_')) k++;
        if (k === n || !isWord(text[k])) {
          const team = text.slice(j + 1, k);
          push(i, k, { kind: 'link', url: `${base}/orgs/${login}/teams/${team}`, text: text.slice(i, k), cls: 'team-mention' });
          i = k;
          continue;
        }
      }
      if (j === n || !isWord(text[j])) {
        push(i, j, { kind: 'link', url: `${base}/${login}`, text: text.slice(i, j), cls: 'user-mention' });
        i = j;
        continue;
      }
    }
    // owner/repo#123
    if (refs && prevOk && isAlnum(c)) {
      let j = i;
      while (j < n && isLogin(text[j])) j++;
      if (text[j] === '/') {
        let k = j + 1;
        while (k < n && isRepoChar(text[k])) k++;
        if (k > j + 1 && k + 1 < n && text[k] === '#' && isDigit(text[k + 1])) {
          let m = k + 1;
          while (m < n && isDigit(text[m])) m++;
          if (m === n || !isWord(text[m])) {
            const [owner, repo, num] = [text.slice(i, j), text.slice(j + 1, k), text.slice(k + 1, m)];
            push(i, m, { kind: 'link', url: `${base}/${owner}/${repo}/issues/${num}`, text: text.slice(i, m), cls: 'issue-link' });
            i = m;
            continue;
          }
        }
      }
    }
    if (refs && ctx.repo) {
      const [owner, repo] = ctx.repo;
      // GH-123
      if (prevOk && text.startsWith('GH-', i) && isDigit(text[i + 3])) {
        let j = i + 3;
        while (j < n && isDigit(text[j])) j++;
        if (j === n || !isWord(text[j])) {
          push(i, j, { kind: 'link', url: `${base}/${owner}/${repo}/issues/${text.slice(i + 3, j)}`, text: text.slice(i, j), cls: 'issue-link' });
          i = j;
          continue;
        }
      }
      // #123
      if (c === '#' && prevOk && isDigit(text[i + 1])) {
        let j = i + 1;
        while (j < n && isDigit(text[j])) j++;
        if (j === n || !isWord(text[j])) {
          push(i, j, { kind: 'link', url: `${base}/${owner}/${repo}/issues/${text.slice(i + 1, j)}`, text: text.slice(i, j), cls: 'issue-link' });
          i = j;
          continue;
        }
      }
      // commit SHA
      if (prevOk && isHex(c)) {
        let j = i;
        while (j < n && isHex(text[j])) j++;
        const sha = text.slice(i, j);
        if (sha.length >= 7 && sha.length <= 40 && (j === n || !isWord(text[j])) && /\d/.test(sha) && /[a-f]/i.test(sha)) {
          push(i, j, { kind: 'link', url: `${base}/${owner}/${repo}/commit/${sha.toLowerCase()}`, text: sha.slice(0, 7), cls: 'commit-link' });
          i = j;
          continue;
        }
      }
    }
    i++;
  }
  if (plainStart < n) out.push({ kind: 'text', text: text.slice(plainStart) });
  return out;
}

/**
 * Short text for an autolinked URL on this instance (`origin`): `#12`,
 * `owner/repo#12`, `#12 (comment)`, `abc1234`, `owner/repo@abc1234`.
 * Mirrors the server's `short_url`.
 */
export function shortUrl(origin: string, repo: readonly [string, string] | undefined, url: string): { text: string; cls: string } | null {
  const base = origin.replace(/\/+$/, '');
  if (!base || !url.startsWith(`${base}/`)) return null;
  const rest = url.slice(base.length + 1);
  const hash = rest.indexOf('#');
  const path = hash < 0 ? rest : rest.slice(0, hash);
  const frag = hash < 0 ? null : rest.slice(hash + 1);
  if (path.includes('?')) return null;
  const parts = path.replace(/\/+$/, '').split('/');
  if (parts.length !== 4) return null;
  const [owner, name, kind, id] = parts as [string, string, string, string];
  const same = !!repo && repo[0].toLowerCase() === owner.toLowerCase() && repo[1].toLowerCase() === name.toLowerCase();
  const prefix = same ? '' : `${owner}/${name}`;
  if ((kind === 'issues' || kind === 'pull') && /^\d+$/.test(id)) {
    const comment = frag !== null && (frag.startsWith('issuecomment-') || frag.startsWith('discussion_r'));
    if (frag !== null && !comment) return null;
    return { text: `${prefix}#${id}${comment ? ' (comment)' : ''}`, cls: 'issue-link' };
  }
  if (kind === 'commit' && frag === null && id.length >= 7 && id.length <= 40 && /^[0-9a-f]+$/i.test(id)) {
    const sha = id.slice(0, 7);
    return { text: same ? sha : `${prefix}@${sha}`, cls: 'commit-link' };
  }
  return null;
}

/** Heading anchor slug with de-duplication (comrak's `Anchorizer`). */
export function anchorizer(): (text: string) => string {
  const seen = new Set<string>();
  return (text) => {
    const id = text
      .toLowerCase()
      .replace(/[^ \-\p{L}\p{M}\p{N}\p{Pc}]/gu, '')
      .replace(/ /g, '-');
    let anchor = id;
    for (let k = 1; seen.has(anchor); k++) anchor = `${id}-${k}`;
    seen.add(anchor);
    return anchor;
  };
}

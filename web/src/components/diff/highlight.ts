/**
 * Syntax highlighting for diffs (P37): the server highlights whole blobs
 * (`/_bgh/repos/{o}/{r}/blob-lines/{sha}?hl=1`, `hl-*` spans, one HTML
 * string per line); these helpers map those lines onto diff lines by line
 * number and decide when to skip highlighting.
 */
import type { DiffHunk, DiffLine } from './parseDiff';

/** Highlighted lines of both sides of one file (`null` = side has no highlighting). */
export interface FileHighlight {
  old: readonly string[] | null;
  new: readonly string[] | null;
}

/** Files with more changed lines than this are rendered as plain text. */
export const MAX_HIGHLIGHT_CHANGES = 5000;

/** HTML of one diff line, if highlighted. */
export function lineHtml(l: DiffLine | null | undefined, hl: FileHighlight | undefined): string | undefined {
  if (!l || !hl || l.type === 'meta') return undefined;
  if (l.type === 'del') return l.oldNo != null ? hl.old?.[l.oldNo - 1] : undefined;
  const fromNew = l.newNo != null ? hl.new?.[l.newNo - 1] : undefined;
  if (fromNew !== undefined) return fromNew;
  return l.type === 'ctx' && l.oldNo != null ? hl.old?.[l.oldNo - 1] : undefined;
}

const ENTITIES: Record<string, string> = { '&lt;': '<', '&gt;': '>', '&amp;': '&', '&quot;': '"', '&#39;': "'", '&#x27;': "'" };

/** Plain text of a highlighted line (tags stripped, entities decoded). */
export function htmlText(html: string): string {
  return html.replace(/<[^>]*>/g, '').replace(/&(?:lt|gt|amp|quot|#39|#x27);/g, (e) => ENTITIES[e] ?? e);
}

/**
 * Whether `hl` really is this diff's content: compares a sample of lines
 * on each side (a mismatch, e.g. a stale blob, falls back to plain text).
 */
export function highlightMatches(hunks: readonly DiffHunk[], hl: FileHighlight, sample = 24): boolean {
  let checked = 0;
  for (const h of hunks) {
    for (const l of h.lines) {
      if (l.type === 'meta') continue;
      const html = lineHtml(l, hl);
      if (html === undefined) continue;
      if (htmlText(html).replace(/\r$/, '') !== l.text.replace(/\r$/, '')) return false;
      if (++checked >= sample) return true;
    }
  }
  return true;
}

// ------------------------------------------------------------ generated files

const GENERATED_NAMES = new Set([
  'package-lock.json',
  'npm-shrinkwrap.json',
  'yarn.lock',
  'pnpm-lock.yaml',
  'bun.lockb',
  'Cargo.lock',
  'Gemfile.lock',
  'poetry.lock',
  'Pipfile.lock',
  'composer.lock',
  'go.sum',
  'flake.lock',
  'mix.lock',
  'pubspec.lock',
  'Podfile.lock',
  'packages.lock.json',
  'uv.lock',
]);

const GENERATED_PATTERNS: RegExp[] = [
  /\.min\.(js|css)$/,
  /\.(js|css)\.map$/,
  /\.pb\.go$/,
  /_pb2(_grpc)?\.py$/,
  /\.pb\.(cc|h)$/,
  /(^|\/)[^/]*\.generated\.[^/]+$/,
  /(^|\/)[^/]*_generated\.[^/]+$/,
  /\.g\.dart$/,
  /\.designer\.cs$/i,
  /(^|\/)__generated__\//,
  /(^|\/)__snapshots__\/[^/]+\.snap$/,
];

/**
 * Generated or vendored lockfiles that GitHub collapses by default
 * (filename heuristics; `.gitattributes` `linguist-generated` arrives with P78).
 */
export function isGenerated(path: string): boolean {
  const name = path.slice(path.lastIndexOf('/') + 1);
  return GENERATED_NAMES.has(name) || GENERATED_PATTERNS.some((re) => re.test(path));
}

const MARKDOWN = /\.(md|markdown|mdown|mkd|mkdn)$/i;

/** Files that offer the rendered ("rich") diff. */
export function isMarkdown(path: string): boolean {
  return MARKDOWN.test(path);
}

const IMAGE = /\.(png|jpe?g|gif|webp|bmp|ico|svg|avif)$/i;

export function isImagePath(path: string): boolean {
  return IMAGE.test(path);
}

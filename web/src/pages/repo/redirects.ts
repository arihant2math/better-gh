/**
 * Targets of the `html_url`s the backend emits that are aliases of existing
 * pages (package P12). Pure so the mapping is unit-tested.
 */
const quote = (s: string) => (/[\s"]/.test(s) ? `"${s.replace(/"/g, '')}"` : s);

export type AliasKind = 'label' | 'repo-search' | 'org-people' | 'org-repositories' | 'org-teams';

export function aliasTarget(kind: AliasKind, p: Record<string, string>, search: string): string {
  const qs = new URLSearchParams(search);
  switch (kind) {
    case 'label':
      return `/${p.owner}/${p.repo}/issues?q=${encodeURIComponent(`is:open label:${quote(p.name ?? '')}`)}`;
    case 'repo-search': {
      const q = qs.get('q')?.trim() ?? '';
      return `/search?q=${encodeURIComponent(`repo:${p.owner}/${p.repo}${q ? ` ${q}` : ''}`)}&type=${encodeURIComponent(qs.get('type') ?? 'code')}`;
    }
    case 'org-people':
      return `/${p.org}?tab=people`;
    case 'org-repositories':
      return `/${p.org}?tab=repositories`;
    case 'org-teams':
      return `/${p.org}?tab=teams`;
  }
}

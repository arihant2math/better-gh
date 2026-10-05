/** `ValueSource` over the local store: qualifier values (users, labels, repos...) for autocomplete. */
import { store } from '../sync';
import type { Repo } from '../sync/models';
import type { ValueKind, ValueSource, ValueSuggestion } from './qualifiers';

function starts(text: string, prefix: string): boolean {
  return text.toLowerCase().startsWith(prefix.toLowerCase());
}

function rank(items: ValueSuggestion[], prefix: string, limit: number): ValueSuggestion[] {
  const p = prefix.toLowerCase();
  return items
    .filter((s) => !p || s.value.toLowerCase().includes(p) || s.detail?.toLowerCase().includes(p))
    .sort((a, b) => Number(starts(b.value, p)) - Number(starts(a.value, p)) || a.value.localeCompare(b.value))
    .slice(0, limit);
}

/** Values from the store; `repo` narrows labels and milestones to one repository. */
export function storeValueSource(repo?: Repo): ValueSource {
  return {
    values(kind: ValueKind, prefix: string, limit: number): ValueSuggestion[] {
      const s = store();
      switch (kind) {
        case 'user':
          return rank(
            s.all('user').map((u) => ({ value: u.login, detail: u.name ?? undefined })),
            prefix,
            limit,
          );
        case 'label': {
          const labels = repo ? s.byIndex('label', 'repoId', repo.id) : s.all('label');
          const seen = new Map<string, ValueSuggestion>();
          for (const l of labels) if (!seen.has(l.name.toLowerCase())) seen.set(l.name.toLowerCase(), { value: l.name, detail: l.description ?? undefined, color: l.color });
          return rank([...seen.values()], prefix, limit);
        }
        case 'milestone': {
          const ms = repo ? s.byIndex('milestone', 'repoId', repo.id) : s.all('milestone');
          return rank(
            [...new Set(ms.filter((m) => m.state === 'open').map((m) => m.title))].map((value) => ({ value })),
            prefix,
            limit,
          );
        }
        case 'repo':
          return rank(
            s.all('repo').map((r) => ({ value: `${r.owner}/${r.name}`, detail: r.description ?? undefined })),
            prefix,
            limit,
          );
        case 'owner': {
          const owners = new Set(s.all('repo').map((r) => r.owner));
          for (const o of s.all('org')) owners.add(o.login);
          return rank([...owners].map((value) => ({ value })), prefix, limit);
        }
        case 'language':
          return rank(
            [...new Set(s.all('repo').map((r) => r.language).filter((l): l is string => !!l))].map((value) => ({ value: value.toLowerCase(), detail: value })),
            prefix,
            limit,
          );
        case 'topic':
          return rank([...new Set(s.all('repo').flatMap((r) => r.topics))].map((value) => ({ value })), prefix, limit);
        default:
          return [];
      }
    },
  };
}

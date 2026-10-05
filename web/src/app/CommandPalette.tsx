import { observer } from 'mobx-react-lite';
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { matchPath, navigate, prefetch } from '../router';
import type { SearchScope } from '../search/api';
import { recordPerf } from '../search/perf';
import { usePaletteSearch } from '../search/usePaletteSearch';
import { store } from '../sync';
import type { Issue, Repo } from '../sync/models';
import { orgByLogin, userByLogin } from '../sync/selectors';
import { Avatar, StateIcon } from '../ui/Badge';
import { Dialog } from '../ui/Dialog';
import { fuzzyScore } from '../ui/fuzzy';
import { LockIcon, RepoIcon, SearchIcon, type Icon } from '../ui/icons';
import { Spinner } from '../ui/Spinner';
import { formatKeys } from '../shortcuts/manager';
import { commands } from './commands';
import styles from './CommandPalette.module.css';
import { currentRepo, ui } from './uiState';

interface Result {
  id: string;
  group: string;
  title: string;
  subtitle?: string;
  icon?: Icon;
  leading?: ReactNode;
  shortcut?: string;
  href?: string;
  run: () => void;
  score: number;
  /** Came from the server (not in the local store). */
  remote?: boolean;
}

const ISSUES = 'Issues & pull requests';
const MAX_PER_GROUP = { Commands: 6, Repositories: 5, [ISSUES]: 10, People: 4 } as Record<string, number>;

interface ScopeOption {
  id: string;
  label: string;
  scope: SearchScope;
  /** Local filter: does a repo belong to the scope? */
  has: (r: Repo) => boolean;
}

function scopeOptions(here: Repo | undefined, owner: string | undefined): ScopeOption[] {
  const out: ScopeOption[] = [{ id: 'global', label: 'Everywhere', scope: { kind: 'global' }, has: () => true }];
  const o = here?.owner ?? owner;
  if (o) out.push({ id: `org:${o}`, label: o, scope: { kind: 'org', login: o }, has: (r) => r.owner.toLowerCase() === o.toLowerCase() });
  if (here) {
    const full = `${here.owner}/${here.name}`;
    out.push({ id: `repo:${full}`, label: full, scope: { kind: 'repo', fullName: full }, has: (r) => r.id === here.id });
  }
  return out;
}

function issueHref(r: Pick<Repo, 'owner' | 'name'>, i: Pick<Issue, 'isPr' | 'number'>): string {
  return `/${r.owner}/${r.name}/${i.isPr ? 'pull' : 'issues'}/${i.number}`;
}

function issueResults(query: string, repos: Map<number, Repo>, here: Repo | undefined, inScope: (r: Repo) => boolean): Result[] {
  const s = store();
  const all = s.all('issue');
  const numberQuery = /^#?(\d+)$/.exec(query.trim());
  const scored: { i: Issue; score: number }[] = [];
  for (const i of all) {
    const r = repos.get(i.repoId);
    if (!r || !inScope(r)) continue;
    let score = 0;
    if (numberQuery) {
      if (String(i.number).startsWith(numberQuery[1]!)) score = (i.number === Number(numberQuery[1]) ? 2000 : 800) + (i.repoId === here?.id ? 500 : 0);
    } else if (query) {
      score = fuzzyScore(query, i.title);
      if (score > 0 && i.repoId === here?.id) score += 50;
      if (score > 0 && i.state === 'open') score += 20;
    } else if (here && i.repoId === here.id && i.state === 'open') {
      score = Date.parse(i.updatedAt) / 1e12; // recent issues of the current repo
    }
    if (score > 0) scored.push({ i, score });
  }
  scored.sort((a, b) => b.score - a.score);
  return scored.slice(0, MAX_PER_GROUP[ISSUES]).map(({ i, score }) => {
    const r = repos.get(i.repoId)!;
    const href = issueHref(r, i);
    return {
      id: `issue:${i.id}`,
      group: ISSUES,
      title: i.title,
      subtitle: `${r.owner}/${r.name}#${i.number}`,
      leading: <StateIcon issue={i} />,
      href,
      run: () => navigate(href),
      score,
    };
  });
}

export const CommandPalette = observer(function CommandPalette() {
  return (
    <Dialog open={ui.paletteOpen} onClose={() => ui.closePalette()} position="top" hideHeader className={styles.dialog} aria-label="Command palette">
      {ui.paletteOpen && <PaletteBody />}
    </Dialog>
  );
});

const PaletteBody = observer(function PaletteBody() {
  const [query, setQuery] = useState(ui.paletteMode === 'commands' ? '>' : '');
  const [active, setActive] = useState(0);
  const listRef = useRef<HTMLDivElement>(null);
  const typedAt = useRef(0);
  const here = currentRepo();
  const pageOwner = matchPath(window.location.pathname)?.params.owner;
  const scopes = useMemo(() => scopeOptions(here, pageOwner && (orgByLogin(pageOwner) || userByLogin(pageOwner)) ? pageOwner : undefined), [here, pageOwner]);
  const [scopeId, setScopeId] = useState('global');
  const scope = scopes.find((s) => s.id === scopeId) ?? scopes[0]!;
  const repos = store().all('repo');
  const cmdList = commands.list();

  const commandMode = query.startsWith('>');
  const q = (commandMode ? query.slice(1) : query).trim();
  const server = usePaletteSearch(q, scope.scope, !commandMode && q.length > 0);

  const local = useMemo(() => {
    const t0 = performance.now();
    const cmds: Result[] = cmdList
      .map((c) => ({ c, score: fuzzyScore(q, `${c.title} ${c.keywords ?? ''} ${c.group}`) }))
      .filter((x) => x.score > 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, commandMode ? 50 : MAX_PER_GROUP.Commands)
      .map(({ c, score }) => ({ id: `cmd:${c.id}`, group: 'Commands', title: c.title, subtitle: c.group, icon: c.icon, shortcut: c.shortcut, run: c.run, score }));
    if (commandMode) return { groups: [cmds], ms: performance.now() - t0 };
    const repoMap = new Map(repos.map((r) => [r.id, r]));
    const repoResults: Result[] = repos
      .filter(scope.has)
      .map((r) => ({ r, score: q ? fuzzyScore(q, `${r.owner}/${r.name}`) : 1 }))
      .filter((x) => x.score > 0)
      .sort((a, b) => b.score - a.score || (b.r.pushedAt ?? '').localeCompare(a.r.pushedAt ?? ''))
      .slice(0, MAX_PER_GROUP.Repositories)
      .map(({ r, score }) => ({
        id: `repo:${r.id}`,
        group: 'Repositories',
        title: `${r.owner}/${r.name}`,
        subtitle: r.description ?? undefined,
        icon: r.private ? LockIcon : RepoIcon,
        href: `/${r.owner}/${r.name}`,
        run: () => navigate(`/${r.owner}/${r.name}`),
        score,
      }));
    const issues = issueResults(q, repoMap, here, scope.has);
    const people: Result[] =
      q.length >= 2 && scope.scope.kind === 'global'
        ? store()
            .all('user')
            .map((u) => ({ u, score: Math.max(fuzzyScore(q, u.login), u.name ? fuzzyScore(q, u.name) * 0.9 : 0) }))
            .filter((x) => x.score >= 1000)
            .sort((a, b) => b.score - a.score)
            .slice(0, MAX_PER_GROUP.People)
            .map(({ u, score }) => ({
              id: `user:${u.id}`,
              group: 'People',
              title: u.login,
              subtitle: u.name ?? undefined,
              leading: <Avatar user={u} size={16} />,
              href: `/${u.login}`,
              run: () => navigate(`/${u.login}`),
              score,
            }))
        : [];
    return { groups: [cmds, repoResults, issues, people], ms: performance.now() - t0 };
  }, [repos, cmdList, here, scope, q, commandMode]);

  // Server results stream in below the local ones of the same group (deduped by id).
  const results = useMemo(() => {
    const [cmds = [], repoResults = [], issues = [], people = []] = local.groups;
    if (commandMode) return cmds;
    const res = server.result;
    const remoteIssues: Result[] = [];
    const remoteRepos: Result[] = [];
    const remotePeople: Result[] = [];
    if (res) {
      const seen = new Set([...issues, ...repoResults, ...people].map((r) => r.id));
      for (const h of res.issues) {
        if (seen.has(`issue:${h.id}`)) continue;
        const [owner, name] = h.repo.split('/') as [string, string];
        const href = issueHref({ owner, name }, { isPr: h.pull_request, number: h.number });
        remoteIssues.push({
          id: `issue:${h.id}`,
          group: ISSUES,
          title: h.title,
          subtitle: `${h.repo}#${h.number}`,
          leading: <StateIcon issue={{ isPr: h.pull_request, state: h.state, stateReason: null, merged: false, draft: false }} />,
          href,
          run: () => navigate(href),
          score: 0,
          remote: true,
        });
      }
      for (const h of res.repos) {
        if (seen.has(`repo:${h.id}`)) continue;
        remoteRepos.push({
          id: `repo:${h.id}`,
          group: 'Repositories',
          title: h.full_name,
          subtitle: h.description ?? undefined,
          icon: h.private ? LockIcon : RepoIcon,
          href: `/${h.full_name}`,
          run: () => navigate(`/${h.full_name}`),
          score: 0,
          remote: true,
        });
      }
      for (const h of res.users) {
        if (seen.has(`user:${h.id}`)) continue;
        remotePeople.push({
          id: `user:${h.id}`,
          group: 'People',
          title: h.login,
          subtitle: h.name ?? undefined,
          leading: <Avatar user={{ login: h.login, avatarUrl: h.avatar_url ?? '' }} size={16} />,
          href: `/${h.login}`,
          run: () => navigate(`/${h.login}`),
          score: 0,
          remote: true,
        });
      }
    }
    const groups = [
      cmds,
      [...repoResults, ...remoteRepos].slice(0, MAX_PER_GROUP.Repositories! + 3),
      [...issues, ...remoteIssues].slice(0, MAX_PER_GROUP[ISSUES]! + 5),
      [...people, ...remotePeople].slice(0, MAX_PER_GROUP.People! + 2),
    ].filter((g) => g.length);
    // Put the strongest local group first when searching (remote-only groups keep their place).
    if (q) groups.sort((a, b) => weight(b) - weight(a));
    const out = groups.flat();
    if (q) {
      const prefix = scope.scope.kind === 'repo' ? `repo:${scope.scope.fullName} ` : scope.scope.kind === 'org' ? `org:${scope.scope.login} ` : '';
      const href = `/search?q=${encodeURIComponent(prefix + q)}&type=issues`;
      out.push({ id: 'search:all', group: 'Search', title: `Search for “${q}”`, subtitle: scope.id === 'global' ? 'All results' : `in ${scope.label}`, icon: SearchIcon, href, run: () => navigate(href), score: 0 });
    }
    return out;
  }, [local, server.result, commandMode, q, scope]);

  // Perf: keystroke → local results committed; request start → server results committed.
  useLayoutEffect(() => {
    if (typedAt.current) {
      recordPerf('palette.local', performance.now() - typedAt.current);
      recordPerf('palette.local.compute', local.ms);
      typedAt.current = 0;
    }
  }, [local]);
  useLayoutEffect(() => {
    if (server.result && server.startedAt) recordPerf('palette.server.rendered', performance.now() - server.startedAt);
  }, [server.result, server.startedAt]);

  const [prevQuery, setPrevQuery] = useState(query);
  if (prevQuery !== query) {
    setPrevQuery(query);
    setActive(0);
  }
  const current = results[Math.min(active, results.length - 1)];

  useEffect(() => {
    listRef.current?.querySelector('[data-active="true"]')?.scrollIntoView({ block: 'nearest' });
    if (current?.href) prefetch(current.href);
  }, [current]);

  const run = (r: Result | undefined) => {
    if (!r) return;
    ui.closePalette();
    r.run();
  };

  const cycleScope = (dir: 1 | -1) => {
    const i = scopes.findIndex((s) => s.id === scope.id);
    setScopeId(scopes[(i + dir + scopes.length) % scopes.length]!.id);
  };

  let lastGroup = '';
  return (
    <div className={styles.palette}>
      <div className={styles.inputRow}>
        <SearchIcon size={16} />
        {scopes.length > 1 && !commandMode && (
          <button type="button" className={styles.scope} onClick={() => cycleScope(1)} title="Change scope (Tab)" data-scope={scope.scope.kind}>
            {scope.label}
          </button>
        )}
        <input
          autoFocus
          data-autofocus
          className={styles.input}
          value={query}
          placeholder={scope.id === 'global' ? 'Search issues, repositories, people, commands…  (type > for commands)' : `Search in ${scope.label}…`}
          onChange={(e) => {
            typedAt.current = performance.now();
            setQuery(e.target.value);
          }}
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-results"
          aria-activedescendant={current ? `palette-${current.id}` : undefined}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) setActive((a) => Math.min(results.length - 1, a + 1));
            else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) setActive((a) => Math.max(0, a - 1));
            else if (e.key === 'Enter') run(current);
            else if (e.key === 'Tab' && scopes.length > 1 && !commandMode) cycleScope(e.shiftKey ? -1 : 1);
            else if (e.key === 'Backspace' && query === '' && scope.id !== 'global') setScopeId('global');
            else return;
            e.preventDefault();
          }}
        />
        {server.loading && q && <Spinner size={14} />}
      </div>
      <div ref={listRef} id="palette-results" className={styles.results} role="listbox" aria-busy={server.loading}>
        {results.length === 0 && <div className={styles.empty}>{server.loading ? 'Searching…' : `No results for “${query}”`}</div>}
        {results.map((r, i) => {
          const header = r.group !== lastGroup ? r.group : null;
          lastGroup = r.group;
          return (
            <div key={r.id}>
              {header && <div className={styles.group}>{header}</div>}
              <div
                id={`palette-${r.id}`}
                role="option"
                aria-selected={r === current}
                data-active={r === current}
                data-remote={r.remote || undefined}
                className={styles.item}
                onPointerMove={() => setActive(i)}
                onClick={() => run(r)}
              >
                <span className={styles.itemIcon}>{r.icon ? <r.icon size={16} /> : r.leading}</span>
                <span className={styles.itemTitle}>{r.title}</span>
                {r.subtitle && <span className={styles.itemSub}>{r.subtitle}</span>}
                {r.shortcut && (
                  <span className={styles.itemKeys}>
                    {formatKeys(r.shortcut).map((k) => (
                      <kbd key={k}>{k}</kbd>
                    ))}
                  </span>
                )}
              </div>
            </div>
          );
        })}
      </div>
      <div className={styles.footer}>
        <span>
          <kbd>↑</kbd>
          <kbd>↓</kbd> navigate
        </span>
        <span>
          <kbd>↵</kbd> open
        </span>
        {scopes.length > 1 && (
          <span>
            <kbd>Tab</kbd> scope
          </span>
        )}
        <span>
          <kbd>&gt;</kbd> commands
        </span>
        <span>
          <kbd>esc</kbd> close
        </span>
      </div>
    </div>
  );
});

/** Group order weight: best local score (commands count half); remote-only groups sink. */
function weight(g: Result[]): number {
  const best = g.find((r) => !r.remote);
  if (!best) return -1;
  return best.score / (best.group === 'Commands' ? 2 : 1);
}

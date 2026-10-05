import { observer } from 'mobx-react-lite';
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { navigate, prefetch } from '../router';
import { store } from '../sync';
import type { Issue, Repo } from '../sync/models';
import { StateIcon } from '../ui/Badge';
import { Dialog } from '../ui/Dialog';
import { fuzzyScore } from '../ui/fuzzy';
import { LockIcon, RepoIcon, SearchIcon, type Icon } from '../ui/icons';
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
}

const MAX_PER_GROUP = { Commands: 6, Repositories: 5, 'Issues & pull requests': 10 } as Record<string, number>;

function issueResults(query: string, repos: Map<number, Repo>, here: Repo | undefined): Result[] {
  const s = store();
  const all = s.all('issue');
  const numberQuery = /^#?(\d+)$/.exec(query.trim());
  const scored: { i: Issue; score: number }[] = [];
  for (const i of all) {
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
  return scored.slice(0, MAX_PER_GROUP['Issues & pull requests']).map(({ i, score }) => {
    const r = repos.get(i.repoId);
    const href = r ? `/${r.owner}/${r.name}/${i.isPr ? 'pull' : 'issues'}/${i.number}` : '/';
    return {
      id: `issue:${i.id}`,
      group: 'Issues & pull requests',
      title: i.title,
      subtitle: r ? `${r.owner}/${r.name}#${i.number}` : `#${i.number}`,
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
  const here = currentRepo();
  const repos = store().all('repo');
  const cmdList = commands.list();

  const results = useMemo(() => {
    const commandMode = query.startsWith('>');
    const q = (commandMode ? query.slice(1) : query).trim();
    const out: Result[] = [];
    const cmds: Result[] = cmdList
      .map((c) => ({ c, score: fuzzyScore(q, `${c.title} ${c.keywords ?? ''} ${c.group}`) }))
      .filter((x) => x.score > 0)
      .sort((a, b) => b.score - a.score)
      .slice(0, commandMode ? 50 : MAX_PER_GROUP.Commands)
      .map(({ c, score }) => ({ id: `cmd:${c.id}`, group: 'Commands', title: c.title, subtitle: c.group, icon: c.icon, shortcut: c.shortcut, run: c.run, score }));
    if (commandMode) return cmds;
    const repoMap = new Map(repos.map((r) => [r.id, r]));
    const repoResults = repos
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
    const issues = issueResults(q, repoMap, here);
    // Put the strongest group first when searching.
    const groups = [cmds, repoResults, issues].filter((g) => g.length);
    if (q) groups.sort((a, b) => (b[0]?.score ?? 0) / (b[0]?.group === 'Commands' ? 2 : 1) - (a[0]?.score ?? 0) / (a[0]?.group === 'Commands' ? 2 : 1));
    for (const g of groups) out.push(...g);
    return out;
  }, [query, repos, cmdList, here]);

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

  let lastGroup = '';
  return (
    <div className={styles.palette}>
      <div className={styles.inputRow}>
        <SearchIcon size={16} />
        <input
          autoFocus
          className={styles.input}
          value={query}
          placeholder="Search issues, repositories, commands…  (type > for commands)"
          onChange={(e) => setQuery(e.target.value)}
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-results"
          aria-activedescendant={current ? `palette-${current.id}` : undefined}
          onKeyDown={(e) => {
            if (e.key === 'ArrowDown' || (e.ctrlKey && e.key === 'n')) setActive((a) => Math.min(results.length - 1, a + 1));
            else if (e.key === 'ArrowUp' || (e.ctrlKey && e.key === 'p')) setActive((a) => Math.max(0, a - 1));
            else if (e.key === 'Enter') run(current);
            else return;
            e.preventDefault();
          }}
        />
      </div>
      <div ref={listRef} id="palette-results" className={styles.results} role="listbox">
        {results.length === 0 && <div className={styles.empty}>No results for “{query}”</div>}
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

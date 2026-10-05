import { observer } from 'mobx-react-lite';
import { useMemo, useRef, useState, type MouseEvent, type ReactNode } from 'react';
import { session } from '../../app/session';
import { navigate, setQuery, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import { useComputed } from '../../sync/hooks';
import type { Issue, Repo } from '../../sync/models';
import { addLabels, closeIssue, removeLabel, reopenIssue, toggleAssignee } from '../../sync/mutations';
import { assignableUsers, labelsForRepo, milestonesForRepo } from '../../sync/selectors';
import { Avatar, ColorDot } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import {
  CheckIcon,
  ChevronDownIcon,
  IssueClosedIcon,
  IssueOpenedIcon,
  GitPullRequestIcon,
  SearchIcon,
  SortDescIcon,
  XIcon,
} from '../../ui/icons';
import { Menu, SelectPanel } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import { VirtualList } from '../../ui/VirtualList';
import { QueryInput } from '../../search/QueryInput';
import { storeValueSource } from '../../search/storeSource';
import { applyFilter, parseQuery, serializeQuery, SORTS, type FilterContext, type IssueFilter } from './filters';
import styles from './IssueList.module.css';
import { IssueRow, issueHref } from './IssueRow';

export interface IssueListProps {
  kind: 'issue' | 'pr';
  /** All candidate rows (read from the store; called inside a computed). */
  source: () => readonly Issue[];
  /** Repo context enables label/assignee/milestone filters and pickers. */
  repo?: Repo;
  showRepo?: boolean;
  defaultQuery?: string;
  header?: ReactNode;
  emptyTitle?: string;
}

/**
 * The canonical list pattern: instant filtering over the local store,
 * URL-synced query, virtualized rows, j/k cursor, multi-select + bulk actions,
 * optimistic mutations.
 */
export const IssueList = observer(function IssueList({ kind, source, repo, showRepo, defaultQuery = 'is:open', header, emptyTitle }: IssueListProps) {
  const query = useQuery().get('q') ?? defaultQuery;
  const filter = useMemo(() => parseQuery(query), [query]);
  const [draft, setDraft] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  const [selected, setSelected] = useState<Set<number>>(() => new Set());
  const anchorIndex = useRef<number | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const valueSource = useMemo(() => storeValueSource(repo), [repo]);

  const viewer = session.user!;
  const ctx: FilterContext = useMemo(
    () => ({
      viewerLogin: viewer.login,
      labelName: (id) => store().get('label', id)?.name,
      userLogin: (id) => store().get('user', id)?.login,
      milestoneTitle: (id) => store().get('milestone', id)?.title,
    }),
    [viewer.login],
  );

  const result = useComputed(() => applyFilter(source(), filter, ctx), [source, filter, ctx]);
  const items = result.items;
  const cursor = Math.min(active, Math.max(0, items.length - 1));

  const setFilter = (patch: Partial<IssueFilter>) => {
    const next = serializeQuery({ ...filter, ...patch });
    setQuery({ q: next === defaultQuery ? null : next });
    setActive(0);
  };

  const selectedIssues = items.filter((i) => selected.has(i.id));
  const targets = selectedIssues.length ? selectedIssues : items[cursor] ? [items[cursor]] : [];

  const toggleSelect = (index: number, e?: MouseEvent | KeyboardEvent) => {
    const issue = items[index];
    if (!issue) return;
    setSelected((prev) => {
      const next = new Set(prev);
      if (e?.shiftKey && anchorIndex.current != null) {
        const [a, b] = [anchorIndex.current, index].sort((x, y) => x - y);
        const on = !prev.has(issue.id);
        for (let i = a!; i <= b!; i++) {
          const id = items[i]!.id;
          if (on) next.add(id);
          else next.delete(id);
        }
      } else if (next.has(issue.id)) next.delete(issue.id);
      else next.add(issue.id);
      return next;
    });
    anchorIndex.current = index;
  };

  // Pickers (bulk-capable)
  const labelBtn = useRef<HTMLButtonElement>(null);
  const assigneeBtn = useRef<HTMLButtonElement>(null);
  const authorBtn = useRef<HTMLButtonElement>(null);
  const sortBtn = useRef<HTMLButtonElement>(null);
  const bulkLabelBtn = useRef<HTMLButtonElement>(null);
  const bulkAssignBtn = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState<null | 'label' | 'assignee' | 'author' | 'sort' | 'bulkLabel' | 'bulkAssign'>(null);

  useShortcuts(kind === 'pr' ? 'Pull request list' : 'Issue list', {
    j: { handler: () => setActive(Math.min(items.length - 1, cursor + 1)), description: 'Next item', group: 'Lists' },
    k: { handler: () => setActive(Math.max(0, cursor - 1)), description: 'Previous item', group: 'Lists' },
    arrowdown: { handler: () => setActive(Math.min(items.length - 1, cursor + 1)), hidden: true },
    arrowup: { handler: () => setActive(Math.max(0, cursor - 1)), hidden: true },
    enter: { handler: () => (items[cursor] ? navigate(issueHref(items[cursor])) : false), description: 'Open item', group: 'Lists' },
    o: { handler: () => (items[cursor] ? navigate(issueHref(items[cursor])) : false), hidden: true },
    x: { handler: (e) => toggleSelect(cursor, e), description: 'Select item', group: 'Lists' },
    'shift+x': { handler: (e) => toggleSelect(cursor, e), hidden: true },
    escape: { handler: () => (selected.size ? setSelected(new Set()) : false), description: 'Clear selection', group: 'Lists' },
    l: { handler: () => (repo && targets.length ? setOpen('bulkLabel') : false), description: 'Labels for selected', group: 'Lists' },
    a: { handler: () => (repo && targets.length ? setOpen('bulkAssign') : false), description: 'Assign selected', group: 'Lists' },
    f: {
      handler: () => {
        searchRef.current?.focus();
        searchRef.current?.select();
      },
      description: 'Filter list',
      group: 'Lists',
    },
  });

  const bulk = (fn: (i: Issue) => unknown, label: string) => {
    const list = targets;
    list.forEach(fn);
    toast({ kind: 'success', title: `${label} ${list.length} ${kind === 'pr' ? 'pull request' : 'issue'}${list.length > 1 ? 's' : ''}` });
    setSelected(new Set());
  };

  const labels = repo ? labelsForRepo(repo.id) : [];
  const people = repo ? assignableUsers(repo) : [];
  const StateIconOpen = kind === 'pr' ? GitPullRequestIcon : IssueOpenedIcon;

  // Bulk label state: selected = on every target.
  const bulkLabelItems = labels.map((l) => ({
    id: l.id,
    text: l.name,
    description: l.description ?? undefined,
    leading: <ColorDot color={l.color} />,
    selected: targets.length > 0 && targets.every((t) => t.labelIds.includes(l.id)),
  }));
  const bulkAssignItems = people.map((u) => ({
    id: u.id,
    text: u.login,
    description: u.name ?? undefined,
    leading: <Avatar user={u} size={18} />,
    selected: targets.length > 0 && targets.every((t) => t.assigneeIds.includes(u.id)),
  }));

  return (
    <div className={styles.page}>
      {header}
      <div className={styles.toolbar}>
        <QueryInput
          inputRef={searchRef}
          className={styles.search}
          leadingIcon={SearchIcon}
          set={kind === 'pr' ? 'pull-list' : 'issue-list'}
          source={valueSource}
          value={draft ?? query}
          onChange={setDraft}
          onBlur={() => setDraft(null)}
          onSubmit={(value) => {
            setQuery({ q: value === defaultQuery ? null : value });
            setDraft(null);
            setActive(0);
            searchRef.current?.blur();
          }}
          onCancel={() => {
            setDraft(null);
            searchRef.current?.blur();
          }}
          aria-label="Filter"
          trailing={query !== defaultQuery && <IconButton icon={XIcon} label="Clear filter" size="sm" onClick={() => setQuery({ q: null })} />}
        />
        {repo && (
          <>
            <Button ref={labelBtn} variant="ghost" trailingIcon={ChevronDownIcon} aria-expanded={open === 'label'} onClick={() => setOpen('label')}>
              Label
            </Button>
            <SelectPanel
              open={open === 'label'}
              onClose={() => setOpen(null)}
              anchor={labelBtn}
              title="Filter by label"
              items={labels.map((l) => ({
                id: l.name,
                text: l.name,
                leading: <ColorDot color={l.color} />,
                selected: filter.labels.some((x) => x.toLowerCase() === l.name.toLowerCase()),
              }))}
              onToggle={(name) => {
                const n = String(name);
                const has = filter.labels.some((x) => x.toLowerCase() === n.toLowerCase());
                setFilter({ labels: has ? filter.labels.filter((x) => x.toLowerCase() !== n.toLowerCase()) : [...filter.labels, n] });
              }}
            />
            <Button ref={assigneeBtn} variant="ghost" trailingIcon={ChevronDownIcon} aria-expanded={open === 'assignee'} onClick={() => setOpen('assignee')}>
              Assignee
            </Button>
            <SelectPanel
              open={open === 'assignee'}
              onClose={() => setOpen(null)}
              anchor={assigneeBtn}
              title="Filter by assignee"
              multiple={false}
              items={[
                { id: 'none', text: 'Assigned to nobody', selected: filter.assignee === 'none' },
                ...people.map((u) => ({ id: u.login, text: u.login, description: u.name ?? undefined, leading: <Avatar user={u} size={18} />, selected: filter.assignee === u.login })),
              ]}
              onToggle={(login) => setFilter({ assignee: filter.assignee === login ? undefined : String(login) })}
            />
            <Button ref={authorBtn} variant="ghost" trailingIcon={ChevronDownIcon} aria-expanded={open === 'author'} onClick={() => setOpen('author')}>
              Author
            </Button>
            <SelectPanel
              open={open === 'author'}
              onClose={() => setOpen(null)}
              anchor={authorBtn}
              title="Filter by author"
              multiple={false}
              items={people.map((u) => ({ id: u.login, text: u.login, description: u.name ?? undefined, leading: <Avatar user={u} size={18} />, selected: filter.author === u.login }))}
              onToggle={(login) => setFilter({ author: filter.author === login ? undefined : String(login) })}
            />
            {milestonesForRepo(repo.id).length > 0 && <MilestoneFilter repo={repo} filter={filter} setFilter={setFilter} />}
          </>
        )}
        <Button ref={sortBtn} variant="ghost" leadingIcon={SortDescIcon} trailingIcon={ChevronDownIcon} aria-expanded={open === 'sort'} onClick={() => setOpen('sort')}>
          {SORTS.find((s) => s.key === filter.sort)?.label}
        </Button>
        <Menu
          open={open === 'sort'}
          onClose={() => setOpen(null)}
          anchor={sortBtn}
          placement="bottom-end"
          items={SORTS.map((s) => ({
            id: s.key,
            label: s.label,
            leading: <span style={{ width: 16, display: 'inline-flex', color: 'var(--accent-fg)' }}>{s.key === filter.sort && <CheckIcon size={16} />}</span>,
            onSelect: () => setFilter({ sort: s.key }),
          }))}
        />
      </div>

      <div className={styles.subbar}>
        {selectedIssues.length > 0 ? (
          <div className={styles.bulkBar}>
            <span className={styles.bulkCount}>{selectedIssues.length} selected</span>
            <Button size="sm" leadingIcon={IssueClosedIcon} onClick={() => bulk((i) => i.state === 'open' && closeIssue(i), 'Closed')}>
              Close
            </Button>
            <Button size="sm" leadingIcon={StateIconOpen} onClick={() => bulk((i) => i.state === 'closed' && !i.merged && reopenIssue(i), 'Reopened')}>
              Reopen
            </Button>
            {repo && (
              <>
                <Button ref={bulkLabelBtn} size="sm" kbd="L" onClick={() => setOpen('bulkLabel')}>
                  Label
                </Button>
                <Button ref={bulkAssignBtn} size="sm" kbd="A" onClick={() => setOpen('bulkAssign')}>
                  Assign
                </Button>
              </>
            )}
            <IconButton icon={XIcon} label="Clear selection" shortcut="Esc" size="sm" onClick={() => setSelected(new Set())} />
          </div>
        ) : (
          <div className={styles.stateTabs} role="tablist">
            <button type="button" role="tab" aria-selected={filter.state === 'open'} className={styles.stateTab} onClick={() => setFilter({ state: 'open', merged: undefined })}>
              <StateIconOpen size={16} />
              {result.openCount.toLocaleString()} Open
            </button>
            <button type="button" role="tab" aria-selected={filter.state === 'closed'} className={styles.stateTab} onClick={() => setFilter({ state: 'closed' })}>
              <CheckIcon size={16} />
              {result.closedCount.toLocaleString()} Closed
            </button>
          </div>
        )}
      </div>

      {/* Bulk pickers anchor to the bulk buttons, or the active row when opened by keyboard. */}
      {repo && (
        <>
          <SelectPanel
            open={open === 'bulkLabel'}
            onClose={() => setOpen(null)}
            anchor={selectedIssues.length ? bulkLabelBtn : searchRef}
            title={`Labels for ${targets.length} item${targets.length === 1 ? '' : 's'}`}
            items={bulkLabelItems}
            onToggle={(id) => {
              const lid = Number(id);
              const allHave = targets.every((t) => t.labelIds.includes(lid));
              for (const t of targets) {
                if (allHave) removeLabel(t, lid);
                else if (!t.labelIds.includes(lid)) addLabels(t, [lid]);
              }
            }}
          />
          <SelectPanel
            open={open === 'bulkAssign'}
            onClose={() => setOpen(null)}
            anchor={selectedIssues.length ? bulkAssignBtn : searchRef}
            title={`Assign ${targets.length} item${targets.length === 1 ? '' : 's'}`}
            items={bulkAssignItems}
            onToggle={(id) => {
              const uid = Number(id);
              const allHave = targets.every((t) => t.assigneeIds.includes(uid));
              for (const t of targets) if (allHave || !t.assigneeIds.includes(uid)) toggleAssignee(t, uid);
            }}
          />
        </>
      )}

      {items.length === 0 ? (
        <EmptyState icon={kind === 'pr' ? GitPullRequestIcon : IssueOpenedIcon} title={emptyTitle ?? `No ${kind === 'pr' ? 'pull requests' : 'issues'} match`}>
          {query !== defaultQuery ? (
            <Button variant="ghost" onClick={() => setQuery({ q: null })}>
              Clear filters
            </Button>
          ) : null}
        </EmptyState>
      ) : (
        <VirtualList
          className={styles.list}
          items={items}
          estimateSize={57}
          activeIndex={cursor}
          getKey={(i) => i.id}
          aria-label={kind === 'pr' ? 'Pull requests' : 'Issues'}
          renderItem={(issue, index) => (
            <IssueRow
              issue={issue}
              showRepo={showRepo}
              active={index === cursor}
              selected={selected.has(issue.id)}
              onActivate={() => setActive(index)}
              onToggleSelect={(e) => toggleSelect(index, e)}
            />
          )}
        />
      )}
    </div>
  );
});

const MilestoneFilter = observer(function MilestoneFilter({ repo, filter, setFilter }: { repo: Repo; filter: IssueFilter; setFilter: (p: Partial<IssueFilter>) => void }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button ref={ref} variant="ghost" trailingIcon={ChevronDownIcon} aria-expanded={open} onClick={() => setOpen(true)} className={cx()}>
        Milestone
      </Button>
      <SelectPanel
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        title="Filter by milestone"
        multiple={false}
        items={[
          { id: 'none', text: 'Issues with no milestone', selected: filter.milestone === 'none' },
          ...milestonesForRepo(repo.id).map((m) => ({ id: m.title, text: m.title, description: m.state === 'closed' ? 'Closed' : undefined, selected: filter.milestone === m.title })),
        ]}
        onToggle={(t) => setFilter({ milestone: filter.milestone === t ? undefined : String(t) })}
      />
    </>
  );
});

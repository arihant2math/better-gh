import { observer } from 'mobx-react-lite';
import { useEffect, useRef, useState, type DragEvent, type KeyboardEvent } from 'react';
import { useCommands } from '../../app/commands';
import { listSubIssues, type RestIssueRef } from '../../api/endpoints';
import { Link } from '../../router';
import { store } from '../../sync';
import type { ID, Issue, Repo } from '../../sync/models';
import { addSubIssue, createIssue, moveSubIssue, removeSubIssue } from '../../sync/mutations';
import { canTriage, issuesForRepo } from '../../sync/selectors';
import { AvatarStack, StateIcon } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { ArrowDownIcon, ArrowUpIcon, GrabberIcon, IssueTracksIcon, PlusIcon, XIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { SelectPanel } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import styles from './SubIssues.module.css';

/** Ids of `issue` and all its ancestors (an ancestor can't become a sub-issue). */
function ancestors(issue: Issue): Set<ID> {
  const out = new Set<ID>([issue.id]);
  let cur: Issue | undefined = issue;
  while (cur?.parentId != null && !out.has(cur.parentId)) {
    out.add(cur.parentId);
    cur = store().get('issue', cur.parentId);
  }
  return out;
}

/** Sub-issues of other repos aren't in the store: fetch them once for display. */
function useRemoteChildren(repo: Repo, issue: Issue, missing: boolean): Map<ID, RestIssueRef> {
  const [rows, setRows] = useState<{ key: string; map: Map<ID, RestIssueRef> }>({ key: '', map: new Map() });
  const key = `${issue.id}:${(issue.subIssueIds ?? []).join(',')}`;
  useEffect(() => {
    if (!missing || issue.id < 0) return;
    let cancelled = false;
    listSubIssues(repo.owner, repo.name, issue.number).then(
      (list) => !cancelled && setRows({ key, map: new Map(list.map((r) => [r.id, r])) }),
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
  }, [missing, key, repo.owner, repo.name, issue.number, issue.id]);
  return rows.map;
}

/**
 * Sub-issues of an issue: ordered list with progress, add (existing or new),
 * remove and reorder (drag, ↑/↓ buttons or Alt+↑/↓). All optimistic.
 */
export const SubIssuesPanel = observer(function SubIssuesPanel({ issue, repo }: { issue: Issue; repo: Repo }) {
  const s = store();
  const ids = issue.subIssueIds ?? [];
  const writable = canTriage(repo.id) && issue.id > 0;
  const missing = ids.some((id) => !s.get('issue', id));
  const remote = useRemoteChildren(repo, issue, missing);
  const [picking, setPicking] = useState(false);
  const [creating, setCreating] = useState<string | null>(null);
  const [dragId, setDragId] = useState<ID | null>(null);
  const addRef = useRef<HTMLButtonElement>(null);
  const listRef = useRef<HTMLOListElement>(null);

  useCommands(
    writable
      ? [
          { id: 'issue.subissue.add', title: 'Add existing sub-issue…', group: 'Issue', icon: IssueTracksIcon, run: () => setPicking(true) },
          { id: 'issue.subissue.create', title: 'Create sub-issue…', group: 'Issue', icon: PlusIcon, run: () => setCreating('') },
        ]
      : [],
    [writable],
  );

  if (!ids.length && !writable) return null;

  const rows = ids.map((id) => {
    const local = s.get('issue', id);
    if (local) {
      const r = s.get('repo', local.repoId);
      return { id, local, number: local.number, title: local.title, closed: local.state === 'closed', href: `/${r?.owner}/${r?.name}/issues/${local.number}`, repoName: r && r.id !== repo.id ? `${r.owner}/${r.name}` : null };
    }
    const rr = remote.get(id);
    const path = rr ? new URL(rr.html_url, location.origin).pathname : null;
    return { id, local: undefined, number: rr?.number ?? 0, title: rr?.title ?? 'Loading…', closed: rr?.state === 'closed', href: path, repoName: path ? path.split('/').slice(1, 3).join('/') : null };
  });
  const done = rows.filter((r) => r.closed).length;

  const exclude = ancestors(issue);
  const candidates = issuesForRepo(repo.id)
    .filter((i) => !i.isPr && i.id > 0 && !exclude.has(i.id) && !ids.includes(i.id))
    .sort((a, b) => b.number - a.number);

  const move = (id: ID, to: number) => {
    if (moveSubIssue(issue, id, to)) {
      requestAnimationFrame(() => listRef.current?.querySelector<HTMLElement>(`[data-id="${id}"]`)?.focus());
    }
  };
  const onRowKey = (e: KeyboardEvent, id: ID, index: number) => {
    if (!writable || !e.altKey) return;
    if (e.key === 'ArrowUp') move(id, index - 1);
    else if (e.key === 'ArrowDown') move(id, index + 1);
    else return;
    e.preventDefault();
  };
  const onDrop = (e: DragEvent, index: number) => {
    e.preventDefault();
    if (dragId != null) move(dragId, index);
    setDragId(null);
  };

  const submitNew = () => {
    const title = (creating ?? '').trim();
    if (!title) return;
    setCreating('');
    const { done: created } = createIssue(repo, { title });
    created.then(
      (res) => {
        const child = res.data as { id?: number; number?: number } | undefined;
        if (child?.id) addSubIssue(issue, { id: child.id, parentId: null });
        toast({ kind: 'success', title: `Created sub-issue${child?.number ? ` #${child.number}` : ''}` });
      },
      () => undefined,
    );
  };

  return (
    <section className={styles.panel} aria-label="Sub-issues">
      <header className={styles.header}>
        <IssueTracksIcon size={16} />
        <h2 className={styles.title}>Sub-issues</h2>
        {rows.length > 0 && (
          <>
            <span className={styles.count}>
              {done} of {rows.length}
            </span>
            <progress className={styles.progress} max={rows.length} value={done} aria-label={`${done} of ${rows.length} sub-issues closed`} />
          </>
        )}
        <span className={styles.spacer} />
        {writable && (
          <>
            <Button size="sm" variant="ghost" leadingIcon={PlusIcon} onClick={() => setCreating('')}>
              Create
            </Button>
            <Button ref={addRef} size="sm" onClick={() => setPicking(true)}>
              Add existing
            </Button>
            <SelectPanel
              open={picking}
              onClose={() => setPicking(false)}
              anchor={addRef}
              placement="bottom-end"
              title="Add sub-issue"
              placeholder="Search issues"
              multiple={false}
              emptyText="No issues to add"
              items={candidates.slice(0, 300).map((i) => ({
                id: i.id,
                text: `#${i.number} ${i.title}`,
                leading: <StateIcon issue={i} size={14} />,
                description: i.parentId != null ? 'Has a parent (will be moved)' : undefined,
                selected: false,
              }))}
              onToggle={(id) => {
                const child = store().get('issue', Number(id));
                if (child) addSubIssue(issue, child);
              }}
            />
          </>
        )}
      </header>
      {rows.length > 0 && (
        <ol ref={listRef} className={styles.list}>
          {rows.map((r, index) => (
            <li
              key={r.id}
              data-id={r.id}
              tabIndex={-1}
              className={cx(styles.row, dragId === r.id && styles.dragging)}
              draggable={writable}
              onDragStart={(e) => {
                setDragId(r.id);
                e.dataTransfer.effectAllowed = 'move';
              }}
              onDragEnd={() => setDragId(null)}
              onDragOver={(e) => dragId != null && e.preventDefault()}
              onDrop={(e) => onDrop(e, index)}
              onKeyDown={(e) => onRowKey(e, r.id, index)}
            >
              {writable && <GrabberIcon size={16} className={styles.grabber} aria-hidden />}
              {r.local ? <StateIcon issue={r.local} size={16} /> : <span className={cx(styles.dot, r.closed && styles.dotClosed)} />}
              {r.href ? (
                <Link to={r.href} className={styles.rowTitle} onKeyDown={(e) => onRowKey(e, r.id, index)}>
                  {r.title}
                </Link>
              ) : (
                <span className={styles.rowTitle}>{r.title}</span>
              )}
              <span className={styles.rowNumber}>
                {r.repoName ? `${r.repoName}#${r.number}` : `#${r.number}`}
              </span>
              <span className={styles.spacer} />
              {r.local && r.local.assigneeIds.length > 0 && <AvatarStack users={r.local.assigneeIds.map((a) => s.get('user', a))} size={18} />}
              {writable && (
                <span className={styles.rowActions}>
                  <IconButton icon={ArrowUpIcon} size="sm" label="Move up" shortcut="Alt ↑" disabled={index === 0} onClick={() => move(r.id, index - 1)} />
                  <IconButton icon={ArrowDownIcon} size="sm" label="Move down" shortcut="Alt ↓" disabled={index === rows.length - 1} onClick={() => move(r.id, index + 1)} />
                  <IconButton icon={XIcon} size="sm" label="Remove sub-issue" onClick={() => removeSubIssue(issue, r.id)} />
                </span>
              )}
            </li>
          ))}
        </ol>
      )}
      {creating !== null && (
        <form
          className={styles.create}
          onSubmit={(e) => {
            e.preventDefault();
            submitNew();
          }}
        >
          <Input
            autoFocus
            size="sm"
            value={creating}
            placeholder="Sub-issue title"
            aria-label="New sub-issue title"
            onChange={(e) => setCreating(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                e.stopPropagation();
                setCreating(null);
              }
            }}
          />
          <Button size="sm" type="submit" variant="primary" disabled={!creating.trim()}>
            Create
          </Button>
          <Button size="sm" variant="ghost" onClick={() => setCreating(null)}>
            Cancel
          </Button>
        </form>
      )}
      {rows.length === 0 && creating === null && <div className={styles.empty}>No sub-issues yet. Break this issue down into smaller pieces of work.</div>}
    </section>
  );
});

/**
 * Issue types, dependencies and duplicates on the issue page (P41): the
 * sidebar "Type" and "Relationships" sections, the header tags and the
 * duplicate-of picker used by the close menu. All writes are optimistic
 * (`sync/issueRelations`).
 */
import { observer } from 'mobx-react-lite';
import { useEffect, useRef, useState, type RefObject } from 'react';
import { api, v3 } from '../../api/client';
import { useResource } from '../../api/cache';
import type { RestIssueRef } from '../../api/endpoints';
import { Link } from '../../router';
import { store } from '../../sync';
import {
  addBlockedBy,
  addBlocking,
  blockedClosure,
  closeAsDuplicate,
  issueTypesKey,
  listIssueTypes,
  removeBlockedBy,
  setIssueType,
} from '../../sync/issueRelations';
import type { ID, Issue, IssueTypeColor, Repo } from '../../sync/models';
import { canTriage, issuesForRepo } from '../../sync/selectors';
import { ColorDot, StateIcon, Tag } from '../../ui/Badge';
import { IconButton } from '../../ui/Button';
import { BlockedIcon, DuplicateIcon, GearIcon, XIcon } from '../../ui/icons';
import { Menu, SelectPanel } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import rel from './IssueRelations.module.css';
import styles from './IssueView.module.css';

/** GitHub's issue type palette (Primer "fg" colors, readable in both themes). */
const TYPE_HEX: Record<IssueTypeColor, string> = {
  gray: '59636e',
  blue: '0969da',
  green: '1a7f37',
  yellow: '9a6700',
  orange: 'bc4c00',
  red: 'd1242f',
  pink: 'bf3989',
  purple: '8250df',
};

export const typeHex = (c: IssueTypeColor | null | undefined) => TYPE_HEX[c ?? 'gray'] ?? TYPE_HEX.gray;

/** Small "● Bug" chip. */
export function IssueTypeChip({ name, color }: { name: string; color: IssueTypeColor | null | undefined }) {
  return (
    <span className={rel.typeChip} data-testid="issue-type">
      <ColorDot color={typeHex(color)} />
      {name}
    </span>
  );
}

/** Whether the repository may have issue types (owned by an organization). */
function useOrgTypes(repo: Repo, enabled: boolean) {
  // Users are in the store as `user` rows; anything else may be an org.
  const isUser = store().get('user', repo.ownerId) !== undefined && store().get('org', repo.ownerId) === undefined;
  const res = useResource(enabled && !isUser ? issueTypesKey(repo.owner) : null, () => listIssueTypes(repo.owner), { ttlMs: 5 * 60_000 });
  return { isUser, res };
}

/** Header tags: type, Blocked, duplicate of. */
export const IssueRelationTags = observer(function IssueRelationTags({ issue }: { issue: Issue }) {
  const s = store();
  const dup = issue.duplicateOfId != null && issue.stateReason === 'duplicate' ? s.get('issue', issue.duplicateOfId) : undefined;
  const dupRepo = dup ? s.get('repo', dup.repoId) : undefined;
  return (
    <>
      {issue.issueType && <IssueTypeChip name={issue.issueType.name} color={issue.issueType.color} />}
      {issue.state === 'open' && (issue.openBlockedBy ?? 0) > 0 && (
        <Tag>
          <BlockedIcon size={12} /> Blocked
        </Tag>
      )}
      {issue.state === 'closed' && issue.stateReason === 'duplicate' && issue.duplicateOfId != null && (
        <span className={styles.metaText} data-testid="duplicate-of">
          <DuplicateIcon size={12} /> Closed as duplicate of{' '}
          {dup && dupRepo ? (
            <Link to={`/${dupRepo.owner}/${dupRepo.name}/issues/${dup.number}`}>
              {dup.repoId === issue.repoId ? '' : `${dupRepo.owner}/${dupRepo.name}`}#{dup.number}
            </Link>
          ) : (
            'another issue'
          )}
        </span>
      )}
    </>
  );
});

function SectionHeader({ title, onEdit, anchor }: { title: string; onEdit?: () => void; anchor?: RefObject<HTMLButtonElement | null> }) {
  return onEdit ? (
    <button ref={anchor} type="button" className={styles.sideHeader} onClick={onEdit} title={title}>
      {title}
      <GearIcon size={14} />
    </button>
  ) : (
    <div className={styles.sideHeaderStatic}>{title}</div>
  );
}

/** Sidebar "Type" (organization repositories, or an issue that has one). */
export const IssueTypeSection = observer(function IssueTypeSection({ issue, repo }: { issue: Issue; repo: Repo }) {
  const writable = canTriage(repo.id) && issue.id > 0 && !issue.isPr;
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  const { isUser, res } = useOrgTypes(repo, !issue.isPr);
  if (issue.isPr || (isUser && !issue.issueType) || (res.error && !issue.issueType)) return null;
  const types = (res.data ?? []).filter((t) => t.is_enabled || t.id === issue.issueType?.id);
  return (
    <section className={styles.sideSection} aria-label="Type">
      <SectionHeader title="Type" onEdit={writable ? () => setOpen(true) : undefined} anchor={ref} />
      <div className={styles.sideBody}>
        {issue.issueType ? <IssueTypeChip name={issue.issueType.name} color={issue.issueType.color} /> : <span className={styles.subtle}>No type</span>}
      </div>
      <SelectPanel
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        placement="bottom-end"
        title="Select an issue type"
        placeholder="Filter types"
        multiple={false}
        emptyText={res.loading ? 'Loading…' : 'No issue types'}
        items={[
          ...(issue.issueType ? [{ id: 'clear', text: 'Clear type', selected: false }] : []),
          ...types.map((t) => ({
            id: t.id,
            text: t.name,
            description: t.description ?? undefined,
            leading: <ColorDot color={typeHex(t.color)} />,
            selected: issue.issueType?.id === t.id,
          })),
        ]}
        onToggle={(id) => {
          setOpen(false);
          const t = types.find((x) => x.id === Number(id));
          if (id === 'clear' || !t || t.id === issue.issueType?.id) setIssueType(issue, null);
          else setIssueType(issue, { id: t.id, name: t.name, color: t.color });
        }}
      />
    </section>
  );
});

/** Dependencies in other repositories aren't in the store: fetch both lists once for display. */
function useRemoteDeps(repo: Repo, issue: Issue, missing: boolean): Map<ID, RestIssueRef & { repository?: string }> {
  const [map, setMap] = useState<Map<ID, RestIssueRef & { repository?: string }>>(new Map());
  const key = `${issue.id}:${(issue.blockedByIds ?? []).join(',')}:${(issue.blockingIds ?? []).join(',')}`;
  useEffect(() => {
    if (!missing || issue.id < 0) return;
    let cancelled = false;
    const base = v3('repos', repo.owner, repo.name, 'issues', issue.number, 'dependencies');
    Promise.all([api.get<RestIssueRef[]>(`${base}/blocked_by?per_page=100`), api.get<RestIssueRef[]>(`${base}/blocking?per_page=100`)]).then(
      ([a, b]) => {
        if (cancelled) return;
        const m = new Map<ID, RestIssueRef & { repository?: string }>();
        for (const r of [...a, ...b]) m.set(r.id, { ...r, repository: r.repository_url?.split('/repos/')[1] });
        setMap(m);
      },
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
  }, [missing, key, repo.owner, repo.name, issue.number, issue.id]);
  return map;
}

const DepRow = observer(function DepRow({ id, repo, remote, onRemove }: { id: ID; repo: Repo; remote?: RestIssueRef & { repository?: string }; onRemove?: () => void }) {
  const s = store();
  const local = s.get('issue', id);
  const localRepo = local ? s.get('repo', local.repoId) : undefined;
  const full = localRepo ? `${localRepo.owner}/${localRepo.name}` : remote?.repository;
  const number = local?.number ?? remote?.number;
  const title = local?.title ?? remote?.title;
  const same = full?.toLowerCase() === `${repo.owner}/${repo.name}`.toLowerCase();
  return (
    <li className={rel.depRow}>
      {local && <StateIcon issue={local} size={14} />}
      {full && number != null ? (
        <Link to={`/${full}/issues/${number}`} className={rel.depLink}>
          <span className={rel.depTitle}>{title}</span> <span className={styles.subtle}>{same ? `#${number}` : `${full}#${number}`}</span>
        </Link>
      ) : (
        <span className={styles.subtle}>An issue you can’t see</span>
      )}
      {onRemove && <IconButton icon={XIcon} size="sm" variant="ghost" label="Remove relationship" onClick={onRemove} />}
    </li>
  );
});

/** Sidebar "Relationships": blocked by / blocking. */
export const RelationshipsSection = observer(function RelationshipsSection({ issue, repo }: { issue: Issue; repo: Repo }) {
  const s = store();
  const writable = canTriage(repo.id) && issue.id > 0 && !issue.isPr;
  const blockedBy = issue.blockedByIds ?? [];
  const blocking = issue.blockingIds ?? [];
  const missing = [...blockedBy, ...blocking].some((id) => !s.get('issue', id));
  const remote = useRemoteDeps(repo, issue, missing);
  const [menu, setMenu] = useState(false);
  const [picking, setPicking] = useState<null | 'blocked_by' | 'blocking'>(null);
  const ref = useRef<HTMLButtonElement>(null);
  if (issue.isPr) return null;
  const exclude = picking === 'blocked_by' ? blockedClosure(issue) : new Set<ID>([issue.id]);
  const candidates =
    picking === null
      ? []
      : issuesForRepo(repo.id)
          .filter((i) => !i.isPr && i.id > 0 && !exclude.has(i.id) && !(picking === 'blocked_by' ? blockedBy : blocking).includes(i.id))
          // `i` blocked by this issue closes a cycle if `i` already (transitively) blocks it.
          .filter((i) => picking === 'blocked_by' || !blockedClosure(i).has(issue.id))
          .sort((a, b) => (a.state === b.state ? b.number - a.number : a.state === 'open' ? -1 : 1));
  return (
    <section className={styles.sideSection} aria-label="Relationships">
      <SectionHeader title="Relationships" onEdit={writable ? () => setMenu(true) : undefined} anchor={ref} />
      <div className={styles.sideBody}>
        {blockedBy.length === 0 && blocking.length === 0 && <span className={styles.subtle}>None yet</span>}
        {blockedBy.length > 0 && (
          <>
            <div className={rel.depHeading}>
              <BlockedIcon size={12} /> Blocked by
            </div>
            <ul className={rel.depList} aria-label="Blocked by">
              {blockedBy.map((id) => (
                <DepRow key={id} id={id} repo={repo} remote={remote.get(id)} onRemove={writable ? () => removeBlockedBy(issue, id) : undefined} />
              ))}
            </ul>
          </>
        )}
        {blocking.length > 0 && (
          <>
            <div className={rel.depHeading}>Blocking</div>
            <ul className={rel.depList} aria-label="Blocking">
              {blocking.map((id) => {
                const other = s.get('issue', id);
                return <DepRow key={id} id={id} repo={repo} remote={remote.get(id)} onRemove={writable && other ? () => removeBlockedBy(other, issue.id) : undefined} />;
              })}
            </ul>
          </>
        )}
      </div>
      <Menu
        open={menu}
        onClose={() => setMenu(false)}
        anchor={ref}
        placement="bottom-end"
        aria-label="Add relationship"
        items={[
          { id: 'blocked_by', label: 'Add blocked by', description: 'This issue can’t progress until…', icon: BlockedIcon, onSelect: () => setPicking('blocked_by') },
          { id: 'blocking', label: 'Add blocking', description: 'This issue blocks…', onSelect: () => setPicking('blocking') },
        ]}
      />
      <SelectPanel
        open={picking !== null}
        onClose={() => setPicking(null)}
        anchor={ref}
        placement="bottom-end"
        title={picking === 'blocking' ? 'Mark issues blocked by this one' : 'Mark this issue as blocked by'}
        placeholder="Search issues"
        multiple={false}
        emptyText="No issues to add"
        items={candidates.slice(0, 300).map((i) => ({ id: i.id, text: `#${i.number} ${i.title}`, leading: <StateIcon issue={i} size={14} />, selected: false }))}
        onToggle={(id) => {
          const other = s.get('issue', Number(id));
          const kind = picking;
          setPicking(null);
          if (!other) return;
          if (kind === 'blocking') {
            if (blockedClosure(other).has(issue.id) || (issue.blockedByIds ?? []).includes(other.id)) {
              toast({ kind: 'error', title: 'That would create a circular dependency' });
              return;
            }
            addBlocking(issue, other);
          } else addBlockedBy(issue, other);
        }}
      />
    </section>
  );
});

/** "Close as duplicate" picker: choose the original issue (same repository). */
export const DuplicatePicker = observer(function DuplicatePicker({
  issue,
  open,
  onClose,
  anchor,
  onDone,
}: {
  issue: Issue;
  open: boolean;
  onClose: () => void;
  anchor: RefObject<HTMLElement | null>;
  onDone?: () => void;
}) {
  const items = open
    ? issuesForRepo(issue.repoId)
        .filter((i) => !i.isPr && i.id > 0 && i.id !== issue.id)
        .sort((a, b) => (a.state === b.state ? b.number - a.number : a.state === 'open' ? -1 : 1))
        .slice(0, 300)
    : [];
  return (
    <SelectPanel
      open={open}
      onClose={onClose}
      anchor={anchor}
      placement="top-end"
      title="Duplicate of"
      placeholder="Search issues"
      multiple={false}
      emptyText="No other issues"
      items={items.map((i) => ({ id: i.id, text: `#${i.number} ${i.title}`, leading: <StateIcon issue={i} size={14} />, selected: false }))}
      onToggle={(id) => {
        const original = store().get('issue', Number(id));
        onClose();
        if (!original) return;
        onDone?.();
        closeAsDuplicate(issue, original);
        toast({ kind: 'success', title: `Closed #${issue.number} as a duplicate of #${original.number}` });
      }}
    />
  );
});

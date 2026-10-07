import { observer } from 'mobx-react-lite';
import { useMemo, useReducer, useRef, useState, type FormEvent, type ReactNode } from 'react';
import { invalidate, load, useResource } from '../../api/cache';
import { codeKeys, createBranch, deleteBranch, getBranchList, type BranchList, type BranchOverview } from '../../api/code';
import { browseKeys, getRefs, isSha } from '../../api/endpoints';
import { activeBranchRulesets, type Ruleset } from '../../api/rulesets';
import { RefPicker, refLabel } from '../../components/code/RefPicker';
import { Link, useParams } from '../../router';
import { compareUrl, repoRefOf, treeUrl } from '../../components/code/urls';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import type { Repo } from '../../sync/models';
import { repoByName, viewerPermission } from '../../sync/selectors';
import { Avatar, StateIcon } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { EmptyRepoState } from '../code/EmptyRepoState';
import { AlertIcon, CopyIcon, GitBranchIcon, GitPullRequestIcon, PlusIcon, SearchIcon, ShieldIcon, ShieldLockIcon, TrashIcon } from '../../ui/icons';
import { Field, Input } from '../../ui/Input';
import { RelativeTime } from '../../ui/RelativeTime';
import { TabNav } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { VirtualList } from '../../ui/VirtualList';
import { selectsRef } from '../rulesets/match';
import { OVERVIEW_LIMIT, barFraction, branchDate, classifyBranches, parseView, type BranchView } from './classify';
import styles from './Branches.module.css';

const VIRTUALIZE_OVER = 100;
const NO_RULESETS: Ruleset[] = [];

const VIEW_LABEL: Record<BranchView, string> = { overview: 'Overview', yours: 'Yours', active: 'Active', stale: 'Stale', all: 'All' };
const SECTION_TITLE: Record<Exclude<BranchView, 'overview'>, string> = {
  yours: 'Your branches',
  active: 'Active branches',
  stale: 'Stale branches',
  all: 'All branches',
};

function copy(text: string) {
  navigator.clipboard?.writeText(text).then(
    () => toast({ title: `Copied “${text}”` }),
    () => toast({ title: 'Copy failed', kind: 'error' }),
  );
}

/** `/:owner/:repo/branches[/:view]`. */
export default observer(function BranchesPage() {
  const params = useParams<{ owner: string; repo: string; view?: string }>();
  const repo = repoByName(params.owner, params.repo);
  if (!repo) return null;
  return <Branches repo={repo} view={parseView(params.view)} />;
});

/**
 * Branch list with optimistic deletes: `hidden` names are removed from the
 * view immediately; a failed delete un-hides them again.
 */
const Branches = observer(function Branches({ repo, view }: { repo: Repo; view: BranchView }) {
  const o = repo.owner;
  const r = repo.name;
  const key = codeKeys.branchList(o, r);
  const res = useResource<BranchList>(key, () => getBranchList(o, r));
  // Keep showing the last list while a refresh after a write is in flight.
  const last = useRef<BranchList | undefined>(undefined);
  if (res.data) last.current = res.data;
  const data = res.data ?? last.current;
  const [, bump] = useReducer((x: number) => x + 1, 0);
  const [hidden, setHidden] = useState<ReadonlySet<string>>(() => new Set());
  const [query, setQuery] = useState('');
  const [creating, setCreating] = useState(false);
  const search = useRef<HTMLInputElement>(null);

  const viewer = store().get('user', store().viewerId)?.login ?? null;
  const perm = viewerPermission(repo.id);
  const canPush = perm === 'write' || perm === 'maintain' || perm === 'admin';
  const base = `/${o}/${r}`;

  useShortcuts('Branches', {
    '/': {
      handler: () => {
        search.current?.focus();
      },
      description: 'Search branches',
      group: 'Branches',
    },
    ...(canPush ? { c: { handler: () => setCreating(true), description: 'New branch', group: 'Branches' } } : {}),
  });

  const sections = useMemo(() => {
    const list = (data?.branches ?? []).filter((b) => !hidden.has(b.name));
    return classifyBranches(list, { defaultBranch: data?.default_branch ?? repo.defaultBranch, viewer, query });
  }, [data, hidden, viewer, query, repo.defaultBranch]);

  const max = useMemo(() => Math.max(1, ...(data?.branches ?? []).flatMap((b) => [b.ahead, b.behind])), [data]);

  const refresh = () => {
    invalidate(key);
    invalidate(browseKeys.refs(o, r));
    // `useResource` keeps listening to the old entry; re-render once the reload lands.
    load(key, () => getBranchList(o, r)).then(bump, () => undefined);
  };

  const setHiddenName = (name: string, hide: boolean) =>
    setHidden((s) => {
      const n = new Set(s);
      if (hide) n.add(name);
      else n.delete(name);
      return n;
    });

  const restore = (b: BranchOverview) => {
    setHiddenName(b.name, false);
    createBranch(o, r, b.name, b.commit.sha).then(
      () => {
        toast({ title: `Branch ${b.name} restored`, kind: 'success' });
        refresh();
      },
      (e: unknown) => {
        setHiddenName(b.name, true);
        toast({ title: `Couldn’t restore ${b.name}`, description: e instanceof Error ? e.message : String(e), kind: 'error' });
      },
    );
  };

  const remove = (b: BranchOverview) => {
    setHiddenName(b.name, true);
    deleteBranch(o, r, b.name).then(
      () => {
        toast({ title: `Branch ${b.name} deleted`, action: { label: 'Restore', onClick: () => restore(b) }, duration: 8000 });
        refresh();
      },
      (e: unknown) => {
        setHiddenName(b.name, false);
        toast({ title: `Couldn’t delete ${b.name}`, description: e instanceof Error ? e.message : String(e), kind: 'error' });
      },
    );
  };

  const defaultBranch = data?.default_branch ?? repo.defaultBranch;
  // Active branch rulesets (own and inherited), matched per row for the ruleset badges.
  const rulesets = useResource<Ruleset[]>(`rulesets:active:${o}/${r}/`, () => activeBranchRulesets(o, r).catch(() => []));
  const rowProps = { repo, defaultBranch, max, canPush, onDelete: remove, rulesets: rulesets.data ?? NO_RULESETS, isAdmin: perm === 'admin' };

  let body: ReactNode;
  if (res.error && !data) {
    body = <EmptyState icon={AlertIcon} title="Couldn’t load branches" />;
  } else if (!data) {
    body = <SkeletonSection />;
  } else if (!data.branches.length) {
    // No branches at all: the repository has no commits yet.
    body = <EmptyRepoState repo={repo} />;
  } else if (view === 'overview') {
    const groups = (['yours', 'active', 'stale'] as const).filter((g) => (g !== 'yours' || viewer) && sections[g].length > 0);
    body = (
      <div className={styles.scroll}>
        {sections.default && (
          <Section title="Default">
            <BranchRow b={sections.default} {...rowProps} />
          </Section>
        )}
        {groups.map((g) => (
          <Section key={g} title={SECTION_TITLE[g]} more={sections[g].length > OVERVIEW_LIMIT ? { href: `${base}/branches/${g}`, count: sections[g].length } : undefined}>
            {sections[g].slice(0, OVERVIEW_LIMIT).map((b) => (
              <BranchRow key={b.name} b={b} {...rowProps} />
            ))}
          </Section>
        ))}
        {!sections.default && !groups.length && <EmptyState icon={GitBranchIcon} title={query ? 'No branches match your search' : 'No branches'} />}
      </div>
    );
  } else {
    const list = sections[view];
    if (!list.length) {
      body = (
        <EmptyState icon={GitBranchIcon} title={query ? 'No branches match your search' : `No ${VIEW_LABEL[view].toLowerCase()} branches`}>
          {view === 'yours' && !query ? 'Branches whose latest commit you authored show up here.' : undefined}
        </EmptyState>
      );
    } else if (list.length > VIRTUALIZE_OVER) {
      body = (
        <VirtualList
          className={styles.virtual}
          items={list}
          estimateSize={52}
          aria-label={SECTION_TITLE[view]}
          header={<div className={styles.sectionTitle}>{SECTION_TITLE[view]}</div>}
          getKey={(b) => b.name}
          renderItem={(b, i) => (
            <div className={cx(styles.vrow, i === 0 && styles.vfirst, i === list.length - 1 && styles.vlast)}>
              <BranchRow b={b} {...rowProps} />
            </div>
          )}
        />
      );
    } else {
      body = (
        <div className={styles.scroll}>
          <Section title={SECTION_TITLE[view]}>
            {list.map((b) => (
              <BranchRow key={b.name} b={b} {...rowProps} />
            ))}
          </Section>
        </div>
      );
    }
  }

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <h1 className={styles.title}>Branches</h1>
        {canPush && (
          <Button variant="primary" size="sm" leadingIcon={PlusIcon} onClick={() => setCreating(true)}>
            New branch
          </Button>
        )}
      </div>
      <div className={styles.toolbar}>
        <TabNav
          aria-label="Branch views"
          current={view}
          items={(['overview', 'yours', 'active', 'stale', 'all'] as const).map((v) => ({
            id: v,
            label: VIEW_LABEL[v],
            href: v === 'overview' ? `${base}/branches` : `${base}/branches/${v}`,
          }))}
        />
        <Input
          ref={search}
          size="sm"
          className={styles.search}
          leadingIcon={SearchIcon}
          placeholder="Search branches…"
          aria-label="Search branches"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Escape') {
              setQuery('');
              e.currentTarget.blur();
            }
          }}
        />
      </div>
      {body}
      {creating && (
        <NewBranchDialog
          repo={repo}
          defaultBranch={defaultBranch}
          onClose={() => setCreating(false)}
          onCreated={(name) => {
            setHiddenName(name, false);
            refresh();
          }}
        />
      )}
    </div>
  );
});

function Section({ title, children, more }: { title: string; children: ReactNode; more?: { href: string; count: number } }) {
  return (
    <section className={styles.section}>
      <h2 className={styles.sectionTitle}>{title}</h2>
      <div className={styles.box} role="list">
        {children}
      </div>
      {more && (
        <Link to={more.href} className={styles.viewMore}>
          View more {title.toLowerCase()} ({more.count})
        </Link>
      )}
    </section>
  );
}

function BranchRow({
  b,
  repo,
  defaultBranch,
  max,
  canPush,
  onDelete,
  rulesets,
  isAdmin,
}: {
  b: BranchOverview;
  repo: Repo;
  defaultBranch: string;
  max: number;
  canPush: boolean;
  onDelete: (b: BranchOverview) => void;
  rulesets: Ruleset[];
  isAdmin: boolean;
}) {
  const base = `/${repo.owner}/${repo.name}`;
  const isDefault = b.name === defaultBranch;
  const author = b.commit.author;
  const user = { login: author.login ?? author.name, avatarUrl: author.avatar_url ?? '', name: author.name };
  const protecting = rulesets.filter((rs) => selectsRef(rs.conditions?.ref_name, 'branch', `refs/heads/${b.name}`, defaultBranch));
  return (
    <div className={styles.row} role="listitem">
      <div className={styles.nameCell}>
        <Link to={treeUrl(repoRefOf(repo), b.name)} className={styles.name} title={b.name}>
          {b.name}
        </Link>
        <IconButton icon={CopyIcon} label="Copy branch name" size="sm" onClick={() => copy(b.name)} />
        {isDefault && <span className={styles.badge}>default</span>}
        {b.protected && !protecting.length && (
          <span className={styles.protected} title="Protected branch">
            <ShieldIcon size={14} />
          </span>
        )}
        {protecting.length > 0 && <RulesetBadge rulesets={protecting} href={isAdmin ? `${base}/settings/rules${protecting.length === 1 && protecting[0]!.source_type === 'Repository' ? `/${protecting[0]!.id}` : ''}` : null} />}
      </div>
      <div className={styles.updated}>
        <Avatar user={user} size={16} title={author.login ?? author.name} />
        <span>
          Updated <RelativeTime date={branchDate(b)} />
        </span>
      </div>
      <div className={styles.aheadBehind}>{isDefault ? <span className={styles.defaultNote}>Default</span> : <AheadBehind ahead={b.ahead} behind={b.behind} max={max} />}</div>
      <div className={styles.pr}>
        {b.pull ? (
          <Link to={`${base}/pull/${b.pull.number}`} className={styles.prLink} title={b.pull.title}>
            <StateIcon issue={{ isPr: true, state: b.pull.state, stateReason: null, merged: b.pull.merged, draft: b.pull.draft }} size={14} />#{b.pull.number}
          </Link>
        ) : (
          !isDefault && (
            <Link to={compareUrl(repoRefOf(repo), defaultBranch, b.name, { expand: true })} className={styles.newPr}>
              <GitPullRequestIcon size={14} />
              New pull request
            </Link>
          )
        )}
      </div>
      <div className={styles.actions}>
        {canPush && !isDefault && !b.protected && <IconButton icon={TrashIcon} label={`Delete ${b.name}`} size="sm" onClick={() => onDelete(b)} />}
      </div>
    </div>
  );
}

/** "Protected by rulesets" badge: the ruleset name (or count), linking to the rulesets for admins. */
function RulesetBadge({ rulesets, href }: { rulesets: Ruleset[]; href: string | null }) {
  const label = rulesets.length === 1 ? rulesets[0]!.name : `${rulesets.length} rulesets`;
  const title = `Protected by ${rulesets.length === 1 ? 'ruleset' : 'rulesets'}: ${rulesets.map((r) => r.name).join(', ')}`;
  const body = (
    <>
      <ShieldLockIcon size={12} />
      {label}
    </>
  );
  return href ? (
    <Link to={href} className={styles.ruleset} title={title} aria-label={title}>
      {body}
    </Link>
  ) : (
    <span className={styles.ruleset} title={title} aria-label={title}>
      {body}
    </span>
  );
}

/** GitHub-style behind | ahead bars around a center line. */
function AheadBehind({ ahead, behind, max }: { ahead: number; behind: number; max: number }) {
  return (
    <div className={styles.ab} title={`${behind} commit${behind === 1 ? '' : 's'} behind, ${ahead} commit${ahead === 1 ? '' : 's'} ahead of the default branch`}>
      <div className={styles.abSide}>
        <span className={styles.abCount}>{behind}</span>
        <span className={styles.abTrack}>
          <span className={cx(styles.abBar, styles.behind)} style={{ width: `${barFraction(behind, max) * 100}%` }} />
        </span>
      </div>
      <div className={cx(styles.abSide, styles.abAhead)}>
        <span className={styles.abTrack}>
          <span className={cx(styles.abBar, styles.ahead)} style={{ width: `${barFraction(ahead, max) * 100}%` }} />
        </span>
        <span className={styles.abCount}>{ahead}</span>
      </div>
    </div>
  );
}

function NewBranchDialog({ repo, defaultBranch, onClose, onCreated }: { repo: Repo; defaultBranch: string; onClose: () => void; onCreated: (name: string) => void }) {
  const [name, setName] = useState('');
  const [source, setSource] = useState(defaultBranch);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trimmed = name.trim();
  const valid = /^[^\s~^:?*[\\]+$/.test(trimmed) && !trimmed.startsWith('/') && !trimmed.endsWith('/') && !trimmed.includes('..') && !trimmed.endsWith('.lock');

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (!valid || busy) return;
    setBusy(true);
    setError(null);
    try {
      const refs = await load(browseKeys.refs(repo.owner, repo.name), () => getRefs(repo.owner, repo.name));
      const sha = [...refs.branches, ...refs.tags].find((x) => x.name === source)?.sha ?? (isSha(source) ? source : null);
      if (!sha) throw new Error(`Unknown source ${source}`);
      await createBranch(repo.owner, repo.name, trimmed, sha);
      toast({ title: `Branch ${trimmed} created`, kind: 'success' });
      onCreated(trimmed);
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title="Create a branch"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!valid} onClick={() => void submit()}>
            Create new branch
          </Button>
        </>
      }
    >
      <form className={styles.form} onSubmit={(e) => void submit(e)}>
        <Field label="New branch name" htmlFor="new-branch-name" error={error ?? (trimmed && !valid ? 'Not a valid branch name' : null)}>
          <Input id="new-branch-name" autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="feature/my-change" invalid={!!trimmed && !valid} />
        </Field>
        <Field label="Source">
          <div>
            <RefPicker owner={repo.owner} repo={repo.name} value={source} onSelect={(ref) => setSource(ref)} />
            <span className={styles.sourceHint}>Branch from {refLabel(source)}</span>
          </div>
        </Field>
      </form>
    </Dialog>
  );
}

function SkeletonSection() {
  return (
    <div className={styles.scroll} aria-busy="true">
      <section className={styles.section}>
        <h2 className={styles.sectionTitle}>
          <Skeleton width={120} />
        </h2>
        <div className={styles.box}>
          {Array.from({ length: 6 }, (_, i) => (
            <div key={i} className={styles.row}>
              <Skeleton width={`${30 + ((i * 13) % 30)}%`} />
              <Skeleton width={120} />
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}

import { observer } from 'mobx-react-lite';
import { useMemo, useRef, useState } from 'react';
import { useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import { compareRefs, getPullTemplate, listBranches, listForks } from '../../api/endpoints';
import type { RestBranch, RestCompare, RestFork } from '../../api/types';
import { toEntries } from '../../components/diff/DiffViewer';
import { DiffView, type DiffSource } from '../../components/diff/DiffView';
import { parsePatch, type DiffFile } from '../../components/diff/parseDiff';
import { Link, navigate, useParams, useQuery, setQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import { createPull } from '../../sync/pullMutations';
import { issuesForRepo, repoByName } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, ArrowLeftIcon, CheckIcon, ChevronDownIcon, GitBranchIcon, GitCommitIcon, GitPullRequestIcon, RepoForkedIcon, XIcon } from '../../ui/icons';
import { Input, Textarea } from '../../ui/Input';
import { Menu, SelectPanel } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { Spinner } from '../../ui/Spinner';
import { toast } from '../../ui/Toast';
import styles from './Compare.module.css';

/** `base...head` (or just `head`, compared with the default branch). Head may be `owner:branch`. */
export function parseSpec(spec: string, defaultBranch: string): { base: string; head: string } {
  const s = decodeURIComponent(spec);
  if (s.includes('...')) {
    const [base, head] = s.split('...');
    return { base: base || defaultBranch, head: head || '' };
  }
  return { base: defaultBranch, head: s };
}

function humanize(branch: string): string {
  const name = branch.split('/').pop() ?? branch;
  const t = name.replace(/[-_]+/g, ' ').trim();
  return t ? t[0]!.toUpperCase() + t.slice(1) : branch;
}

export default observer(function ComparePage() {
  const { owner, repo: name, '*': spec = '' } = useParams<{ owner: string; repo: string; '*'?: string }>();
  const repo = repoByName(owner, name);
  const query = useQuery();
  if (!repo) return null;
  const { base, head } = parseSpec(spec, repo.defaultBranch);
  const headOwner = head.includes(':') ? head.split(':')[0]! : repo.owner;
  const headBranch = head.includes(':') ? head.split(':').slice(1).join(':') : head;
  const prefix = `/${repo.owner}/${repo.name}/compare`;
  const go = (b: string, h: string) => navigate(`${prefix}/${b}...${h}${query.get('expand') ? '?expand=1' : ''}`, { replace: true });
  return (
    <div className={styles.page}>
      <h1 className={styles.title}>{query.get('expand') ? 'Open a pull request' : 'Comparing changes'}</h1>
      <p className={styles.subtle}>
        Choose two branches to see what’s changed or to start a new pull request. If you need to, you can also compare across forks.
      </p>
      <div className={styles.pickers}>
        <GitBranchIcon size={16} className={styles.subtle} />
        <BranchPicker label="base" owner={repo.owner} repo={repo.name} value={base} onChange={(b) => go(b, head)} />
        <ArrowLeftIcon size={16} className={styles.subtle} />
        <ForkPicker owner={repo.owner} repo={repo.name} value={headOwner} onChange={(o) => go(base, o === repo.owner ? headBranch : `${o}:${headBranch}`)} />
        <BranchPicker
          label="compare"
          owner={headOwner}
          repo={headOwner === repo.owner ? repo.name : (forkName(repo.owner, repo.name, headOwner) ?? repo.name)}
          value={headBranch}
          onChange={(b) => go(base, headOwner === repo.owner ? b : `${headOwner}:${b}`)}
        />
      </div>
      {!head ? (
        <EmptyState icon={GitPullRequestIcon} title="Compare changes across branches, commits, tags, and more">
          Pick a branch to compare with <code>{base}</code>.
        </EmptyState>
      ) : (
        <CompareBody repoId={repo.id} owner={repo.owner} name={repo.name} base={base} head={head} headOwner={headOwner} headBranch={headBranch} expand={!!query.get('expand')} />
      )}
    </div>
  );
});

/** Forks are listed lazily by the picker; remember their repo names for the head branch list. */
const forkNames = new Map<string, string>();
function forkName(owner: string, repo: string, forkOwner: string): string | undefined {
  return forkNames.get(`${owner}/${repo}:${forkOwner}`);
}

function ForkPicker({ owner, repo, value, onChange }: { owner: string; repo: string; value: string; onChange: (owner: string) => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  const { data: forks } = useResource<RestFork[]>(open || value !== owner ? `forks:${owner}/${repo}` : null, () => listForks(owner, repo), { ttlMs: 60_000 });
  for (const f of forks ?? []) forkNames.set(`${owner}/${repo}:${f.owner.login}`, f.name);
  return (
    <>
      <Button ref={ref} size="sm" leadingIcon={RepoForkedIcon} trailingIcon={ChevronDownIcon} onClick={() => setOpen(true)} aria-label="Head repository">
        <span className={styles.pickerLabel}>head repository:</span> {value === owner ? `${owner}/${repo}` : `${value}/${forkName(owner, repo, value) ?? repo}`}
      </Button>
      <SelectPanel
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        multiple={false}
        title="Choose a head repository"
        placeholder="Filter repos"
        emptyText={forks ? 'No forks' : 'Loading…'}
        items={[
          { id: owner, text: `${owner}/${repo}`, selected: value === owner },
          ...(forks ?? []).map((f) => ({ id: f.owner.login, text: f.full_name, selected: value === f.owner.login })),
        ]}
        onToggle={(id) => onChange(String(id))}
      />
    </>
  );
}

function BranchPicker({ label, owner, repo, value, onChange }: { label: string; owner: string; repo: string; value: string; onChange: (b: string) => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement>(null);
  const { data } = useResource<RestBranch[]>(`branches:${owner}/${repo}`, () => listBranches(owner, repo), { ttlMs: 30_000 });
  return (
    <>
      <Button ref={ref} size="sm" trailingIcon={ChevronDownIcon} onClick={() => setOpen(true)} aria-label={`${label} branch`}>
        <span className={styles.pickerLabel}>{label}:</span> {value || 'choose…'}
      </Button>
      <SelectPanel
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        multiple={false}
        title={`Choose a ${label} ref`}
        placeholder="Find a branch"
        emptyText={data ? 'Nothing to show' : 'Loading…'}
        items={(data ?? []).map((b) => ({ id: b.name, text: b.name, leading: <GitBranchIcon size={14} />, selected: b.name === value }))}
        onToggle={(id) => onChange(String(id))}
      />
    </>
  );
}

const CompareBody = observer(function CompareBody({
  repoId,
  owner,
  name,
  base,
  head,
  headOwner,
  headBranch,
  expand,
}: {
  repoId: number;
  owner: string;
  name: string;
  base: string;
  head: string;
  headOwner: string;
  headBranch: string;
  expand: boolean;
}) {
  const { data, error, loading } = useResource<RestCompare>(`compare:${owner}/${name}:${base}...${head}`, () => compareRefs(owner, name, base, head), { ttlMs: 15_000 });
  const existing = issuesForRepo(repoId).find((i) => i.isPr && i.state === 'open' && i.headRef === headBranch && i.baseRef === base && (headOwner === owner || store().get('repo', i.headRepoId)?.owner === headOwner));
  if (error) {
    const notFound = error instanceof ApiError && error.status === 404;
    return (
      <EmptyState icon={AlertIcon} title={notFound ? 'There isn’t anything to compare' : 'Couldn’t compare these refs'}>
        {notFound ? (
          <>
            <code>{base}</code> or <code>{head}</code> doesn’t exist.
          </>
        ) : (
          (error as Error).message
        )}
      </EmptyState>
    );
  }
  if (loading || !data) {
    return (
      <div className={styles.box}>
        <Skeleton width="40%" />
        <Skeleton width="70%" style={{ marginTop: 10 }} />
      </div>
    );
  }
  const identical = data.status === 'identical' || data.ahead_by === 0;
  return (
    <>
      <div className={styles.status}>
        {identical ? (
          <span>
            <strong>There isn’t anything to compare.</strong> <code>{base}</code> is up to date with all commits from <code>{head}</code>.
          </span>
        ) : (
          <span>
            <CheckIcon size={16} className={styles.ok} /> <strong>{data.ahead_by}</strong> commit{data.ahead_by === 1 ? '' : 's'} ahead
            {data.behind_by > 0 && (
              <>
                , <strong>{data.behind_by}</strong> behind
              </>
            )}{' '}
            <code>{base}</code>.
          </span>
        )}
        {existing && (
          <span className={styles.existing}>
            <GitPullRequestIcon size={16} /> <Link to={`/${owner}/${name}/pull/${existing.number}`}>#{existing.number} {existing.title}</Link> is already open for this branch.
          </span>
        )}
        <span style={{ flex: 1 }} />
        {!identical && !existing && !expand && (
          <Button variant="success" onClick={() => setQuery({ expand: '1' })}>
            Create pull request
          </Button>
        )}
      </div>
      {!identical && !existing && expand && <CreateForm repoId={repoId} owner={owner} name={name} base={base} head={head} headBranch={headBranch} compare={data} />}
      {!identical && <CompareDetails compare={data} owner={owner} name={name} sameRepo={headOwner === owner} />}
    </>
  );
});

const CreateForm = observer(function CreateForm({
  repoId,
  owner,
  name,
  base,
  head,
  headBranch,
  compare,
}: {
  repoId: number;
  owner: string;
  name: string;
  base: string;
  head: string;
  headBranch: string;
  compare: RestCompare;
}) {
  const repo = store().get('repo', repoId)!;
  const single = compare.commits.length === 1 ? compare.commits[0]!.commit.message : null;
  const { data: template } = useResource<string | null>(`pr-template:${owner}/${name}`, () => getPullTemplate(owner, name), { ttlMs: 300_000 });
  const [title, setTitle] = useState(() => (single ? single.split('\n')[0]! : humanize(headBranch)));
  const [body, setBody] = useState<string | null>(null);
  const [draft, setDraft] = useState(false);
  const [busy, setBusy] = useState(false);
  const [menu, setMenu] = useState(false);
  const menuRef = useRef<HTMLButtonElement>(null);
  const defaultBody = template ?? (single ? single.split('\n').slice(1).join('\n').trim() : '');
  const value = body ?? defaultBody;

  const submit = () => {
    if (!title.trim() || busy) return;
    setBusy(true);
    createPull(repo, { title: title.trim(), body: value, base, head, draft }).done.then(
      (res) => {
        const number = (res.data as { number?: number } | null)?.number;
        toast({ kind: 'success', title: `Opened pull request${number ? ` #${number}` : ''}` });
        navigate(number ? `/${owner}/${name}/pull/${number}` : `/${owner}/${name}/pulls`);
      },
      () => setBusy(false),
    );
  };
  useShortcuts('New pull request', { 'mod+enter': { handler: submit, description: 'Create pull request', group: 'Pull request', allowInInput: true } });

  return (
    <div className={styles.form}>
      <Avatar user={store().get('user', store().viewerId)} size={40} />
      <div className={styles.formMain}>
        <Input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Title" aria-label="Title" autoFocus size="lg" />
        <Textarea value={value} onChange={(e) => setBody(e.target.value)} rows={12} placeholder="Add a description (Markdown supported)" aria-label="Description" className={styles.body} />
        <div className={styles.formActions}>
          {template && body === null && <span className={styles.subtle}>Prefilled from the pull request template.</span>}
          <span style={{ flex: 1 }} />
          <span className={styles.split}>
            <Button variant="success" loading={busy} disabled={!title.trim()} onClick={submit}>
              {draft ? 'Draft pull request' : 'Create pull request'}
            </Button>
            <Button ref={menuRef} variant="success" aria-label="Pull request type" onClick={() => setMenu((m) => !m)} className={styles.splitToggle}>
              <ChevronDownIcon size={16} />
            </Button>
          </span>
          <Menu
            open={menu}
            onClose={() => setMenu(false)}
            anchor={menuRef}
            placement="bottom-end"
            items={[
              { id: 'pr', label: 'Create pull request', description: 'Open a pull request that is ready for review.', leading: <span className={styles.menuCheck}>{!draft && <CheckIcon size={16} />}</span>, onSelect: () => setDraft(false) },
              { id: 'draft', label: 'Create draft pull request', description: 'Cannot be merged until marked ready for review.', leading: <span className={styles.menuCheck}>{draft && <CheckIcon size={16} />}</span>, onSelect: () => setDraft(true) },
            ]}
          />
          <Button variant="ghost" leadingIcon={XIcon} onClick={() => setQuery({ expand: null })}>
            Cancel
          </Button>
        </div>
      </div>
    </div>
  );
});

function CompareDetails({ compare, owner, name, sameRepo }: { compare: RestCompare; owner: string; name: string; sameRepo: boolean }) {
  const mode = useQuery().get('diff') === 'split' ? 'split' : 'unified';
  const files: DiffFile[] = useMemo(
    () =>
      (compare.files ?? []).map((f) => ({
        oldPath: f.previous_filename ?? f.filename,
        newPath: f.filename,
        path: f.filename,
        status: f.status === 'removed' ? 'deleted' : f.status === 'added' ? 'added' : f.status === 'renamed' ? 'renamed' : 'modified',
        binary: !f.patch && f.additions + f.deletions === 0 && f.status !== 'renamed',
        additions: f.additions,
        deletions: f.deletions,
        hunks: f.patch ? parsePatch(f.patch) : [],
      })),
    [compare],
  );
  const entries = useMemo(() => toEntries(files), [files]);
  // Highlighting, context expansion, image/rich diffs (P37): needs both SHAs in this repository.
  const mergeBase = compare.merge_base_commit?.sha;
  const headSha = compare.commits.length === compare.total_commits ? compare.commits[compare.commits.length - 1]?.sha : undefined;
  const source = useMemo<DiffSource | undefined>(() => (sameRepo && mergeBase && headSha ? { owner, repo: name, oldRef: mergeBase, newRef: headSha } : undefined), [sameRepo, mergeBase, headSha, owner, name]);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const additions = files.reduce((a, f) => a + f.additions, 0);
  const deletions = files.reduce((a, f) => a + f.deletions, 0);
  return (
    <>
      <div className={styles.summary}>
        <span>
          <GitCommitIcon size={16} /> {compare.total_commits} commit{compare.total_commits === 1 ? '' : 's'}
        </span>
        <span>
          {files.length} file{files.length === 1 ? '' : 's'} changed
        </span>
        <span>
          <span className={styles.ok}>+{additions}</span> <span className={styles.fail}>−{deletions}</span>
        </span>
        <span style={{ flex: 1 }} />
        <span className={styles.toggle}>
          <button type="button" aria-pressed={mode === 'unified'} onClick={() => setQuery({ diff: null })}>
            Unified
          </button>
          <button type="button" aria-pressed={mode === 'split'} onClick={() => setQuery({ diff: 'split' })}>
            Split
          </button>
        </span>
      </div>
      <div className={styles.commits}>
        {compare.commits.map((c) => (
          <div key={c.sha} className={styles.commit}>
            <Avatar user={c.author ? { login: c.author.login, avatarUrl: c.author.avatar_url } : null} size={18} />
            <span className={styles.commitMsg}>{c.commit.message.split('\n')[0]}</span>
            <span className={styles.subtle}>
              {c.author?.login ?? c.commit.author.name} · <RelativeTime date={c.commit.author.date} />
            </span>
            <code className={styles.sha}>{c.sha.slice(0, 7)}</code>
          </div>
        ))}
      </div>
      <div className={styles.diff}>
        {files.length === 0 ? (
          <div className={styles.subtle}>
            <Spinner size={14} /> No file changes.
          </div>
        ) : (
          <DiffView files={entries} mode={mode} tree={files.length > 1} collapsed={collapsed} onToggleCollapsed={(p) => setCollapsed((c) => (c.has(p) ? new Set([...c].filter((x) => x !== p)) : new Set([...c, p])))} keyboard source={source} />
        )}
      </div>
    </>
  );
}

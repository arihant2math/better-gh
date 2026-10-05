import { observer } from 'mobx-react-lite';
import { useRef, useState, type MouseEvent } from 'react';
import { load, prefetch, useResource } from '../../api/cache';
import { browseKeys, getBlob, getHistory, getRefs, getTree, getTreeCommits, isSha } from '../../api/endpoints';
import type { BlobView, BrowseCommit, BrowseRefs, History, LastCommits, TreeEntry, TreeView } from '../../api/types';
import { Link, navigate, useLocation, useParams } from '../../router';
import type { Repo } from '../../sync/models';
import { repoByName } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import {
  AlertIcon,
  BookIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CopyIcon,
  FileDirectoryFillIcon,
  FileIcon,
  GitBranchIcon,
} from '../../ui/icons';
import { Menu } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import styles from './CodePage.module.css';

/** Content addressed by a commit SHA never changes. */
const opts = (ref: string) => ({ immutable: isSha(ref) });

function useTree(repo: Repo, ref: string, path: string) {
  return useResource<TreeView>(browseKeys.tree(repo.owner, repo.name, ref, path), () => getTree(repo.owner, repo.name, ref, path), opts(ref));
}

function prefetchTree(repo: Repo, ref: string, path: string) {
  prefetch(browseKeys.tree(repo.owner, repo.name, ref, path), () => getTree(repo.owner, repo.name, ref, path), opts(ref));
}

function prefetchBlob(repo: Repo, ref: string, path: string) {
  prefetch(browseKeys.blob(repo.owner, repo.name, ref, path), () => getBlob(repo.owner, repo.name, ref, path), opts(ref));
}

/**
 * Follow same-origin links inside server-rendered HTML (READMEs) with the
 * client router instead of a full page load.
 */
function routeLinks(e: MouseEvent<HTMLElement>) {
  if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  const a = (e.target as HTMLElement).closest('a');
  if (!a || a.target === '_blank') return;
  const url = new URL(a.href, window.location.href);
  if (url.origin !== window.location.origin || url.pathname.includes('/raw/')) return;
  e.preventDefault();
  navigate(url.pathname + url.search + url.hash);
}

/** Code browser: tree on the left, directory listing / file view on the right. */
export default observer(function CodePage() {
  const params = useParams<{ owner: string; repo: string; ref?: string; '*'?: string }>();
  const { pathname } = useLocation();
  const repo = repoByName(params.owner, params.repo);
  if (!repo) return null;
  const ref = params.ref ?? repo.defaultBranch;
  const path = (params['*'] ?? '').replace(/\/$/, '');
  const isBlob = pathname.includes('/blob/');
  return (
    <div className={styles.page}>
      <TreePanel repo={repo} refName={ref} current={path} />
      <div className={styles.main}>
        <div className={styles.bar}>
          <BranchPicker repo={repo} refName={ref} path={path} isBlob={isBlob} />
          <PathCrumbs repo={repo} refName={ref} path={path} />
        </div>
        <LastCommit repo={repo} refName={ref} path={path} />
        {isBlob ? <FileView repo={repo} refName={ref} path={path} /> : <DirView repo={repo} refName={ref} path={path} />}
      </div>
    </div>
  );
});

function PathCrumbs({ repo, refName, path }: { repo: Repo; refName: string; path: string }) {
  const parts = path ? path.split('/') : [];
  return (
    <nav className={styles.crumbs} aria-label="Path">
      <Link to={`/${repo.owner}/${repo.name}/tree/${refName}`} className={styles.crumbRoot}>
        {repo.name}
      </Link>
      {parts.map((p, i) => (
        <span key={i} className={styles.crumbPart}>
          <span className={styles.crumbSep}>/</span>
          {i === parts.length - 1 ? (
            <strong>{p}</strong>
          ) : (
            <Link to={`/${repo.owner}/${repo.name}/tree/${refName}/${parts.slice(0, i + 1).join('/')}`}>{p}</Link>
          )}
        </span>
      ))}
    </nav>
  );
}

function BranchPicker({ repo, refName, path, isBlob }: { repo: Repo; refName: string; path: string; isBlob: boolean }) {
  const ref = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const refs = useResource<BrowseRefs>(open ? browseKeys.refs(repo.owner, repo.name) : null, () => getRefs(repo.owner, repo.name));
  const item = (name: string, isDefault: boolean) => ({
    id: name,
    label: name,
    leading: <span style={{ width: 16, display: 'inline-flex', color: 'var(--accent-fg)' }}>{name === refName && <CheckIcon size={16} />}</span>,
    trailing: isDefault ? 'default' : undefined,
    onSelect: () => navigate(`/${repo.owner}/${repo.name}/${isBlob ? 'blob' : 'tree'}/${name}${path ? `/${path}` : ''}`),
  });
  return (
    <>
      <Button
        ref={ref}
        size="sm"
        leadingIcon={GitBranchIcon}
        trailingIcon={ChevronDownIcon}
        onClick={() => setOpen((o) => !o)}
        onMouseEnter={() => prefetch(browseKeys.refs(repo.owner, repo.name), () => getRefs(repo.owner, repo.name))}
        aria-expanded={open}
      >
        {isSha(refName) ? refName.slice(0, 7) : refName}
      </Button>
      <Menu
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        aria-label="Switch branch or tag"
        items={
          refs.data
            ? [
                { header: 'Branches', id: 'h-branches' },
                ...refs.data.branches.map((b) => item(b.name, b.name === refs.data!.default_branch)),
                ...(refs.data.tags.length ? [{ header: 'Tags', id: 'h-tags' }, ...refs.data.tags.map((t) => item(t.name, false))] : []),
              ]
            : [{ id: 'loading', label: 'Loading refs…', disabled: true }]
        }
      />
    </>
  );
}

function CommitAuthor({ c, size = 20 }: { c: BrowseCommit; size?: number }) {
  return (
    <>
      <Avatar user={c.author.login ? { login: c.author.login, avatarUrl: c.author.avatar_url ?? '' } : null} size={size} />
      <strong>{c.author.login ?? c.author.name}</strong>
    </>
  );
}

function LastCommit({ repo, refName, path }: { repo: Repo; refName: string; path: string }) {
  const { data } = useResource<History>(
    browseKeys.lastCommit(repo.owner, repo.name, refName, path),
    () => getHistory(repo.owner, repo.name, refName, path, { perPage: 1 }),
    opts(refName),
  );
  const c = data?.commits[0];
  return (
    <div className={styles.lastCommit}>
      {c ? (
        <>
          <CommitAuthor c={c} />
          <span className={styles.commitMsg}>{c.summary}</span>
          <code className={styles.sha}>{c.sha.slice(0, 7)}</code>
          <span className={styles.subtle}>
            <RelativeTime date={c.committer.date} />
          </span>
        </>
      ) : (
        <Skeleton width={320} />
      )}
    </div>
  );
}

function DirView({ repo, refName, path }: { repo: Repo; refName: string; path: string }) {
  const { data, error } = useTree(repo, refName, path);
  // Last commit per entry: inlined when the server has it cached, otherwise
  // fetched by commit SHA (immutable).
  const commits = useResource<LastCommits>(
    data && !data.last_commits ? browseKeys.treeCommits(repo.owner, repo.name, data.commit, path) : null,
    () => getTreeCommits(repo.owner, repo.name, data!.commit, path),
    { immutable: true },
  );
  if (error) return <EmptyState icon={AlertIcon} title="Path not found" />;
  if (!data) {
    return (
      <div className={styles.listing}>
        {Array.from({ length: 6 }, (_, i) => (
          <div key={i} className={styles.entry}>
            <Skeleton width={`${30 + ((i * 17) % 40)}%`} />
          </div>
        ))}
      </div>
    );
  }
  const last = data.last_commits ?? commits.data?.entries;
  return (
    <>
      <div className={styles.listing} role="list">
        {path && (
          <Link to={`/${repo.owner}/${repo.name}/tree/${refName}/${path.split('/').slice(0, -1).join('/')}`} className={styles.entry}>
            <FileDirectoryFillIcon size={16} className={styles.dirIcon} />
            <span>..</span>
          </Link>
        )}
        {data.entries.map((e) => (
          <EntryRow key={e.path} repo={repo} refName={refName} entry={e} commit={last?.[e.name]} pending={!last} />
        ))}
      </div>
      {data.readme && (
        <section className={styles.readme}>
          <header className={styles.readmeHeader}>
            <BookIcon size={16} /> {data.readme.name}
          </header>
          <div className={cx('markdown-body', styles.readmeBody)} onClick={routeLinks} dangerouslySetInnerHTML={{ __html: data.readme.html }} />
        </section>
      )}
    </>
  );
}

function EntryRow({ repo, refName, entry, commit, pending }: { repo: Repo; refName: string; entry: TreeEntry; commit?: BrowseCommit; pending: boolean }) {
  const isDir = entry.type === 'tree';
  const to = `/${repo.owner}/${repo.name}/${isDir ? 'tree' : 'blob'}/${refName}/${entry.path}`;
  return (
    <Link
      to={to}
      className={styles.entry}
      role="listitem"
      onMouseEnter={() => (isDir ? prefetchTree(repo, refName, entry.path) : prefetchBlob(repo, refName, entry.path))}
    >
      {isDir ? <FileDirectoryFillIcon size={16} className={styles.dirIcon} /> : <FileIcon size={16} className={styles.fileIcon} />}
      <span className={styles.entryName}>{entry.name}</span>
      <span className={styles.entryCommit}>{commit ? commit.summary : pending ? <Skeleton width={160} /> : null}</span>
      <span className={styles.entryTime}>{commit && <RelativeTime date={commit.committer.date} />}</span>
    </Link>
  );
}

function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function FileNotice({ children }: { children: React.ReactNode }) {
  return <div className={styles.notice}>{children}</div>;
}

function FileBody({ blob }: { blob: BlobView }) {
  if (blob.type === 'submodule') return <FileNotice>Submodule at commit {blob.sha.slice(0, 7)}.</FileNotice>;
  if (blob.symlink_target !== null) return <FileNotice>Symbolic link to {blob.symlink_target}</FileNotice>;
  if (blob.image) {
    return (
      <div className={styles.image}>
        <img src={blob.raw_url} alt={blob.name} />
      </div>
    );
  }
  if (blob.lfs) {
    return (
      <FileNotice>
        Stored with Git LFS ({formatSize(blob.lfs.size)}).{' '}
        {blob.lfs.stored ? <a href={blob.raw_url}>Download</a> : 'The object has not been uploaded.'}
      </FileNotice>
    );
  }
  if (blob.too_large) {
    return (
      <FileNotice>
        This file is too large to display. <a href={blob.raw_url}>View raw</a>
      </FileNotice>
    );
  }
  if (blob.binary || !blob.lines) {
    return (
      <FileNotice>
        Binary file not shown. <a href={blob.raw_url}>Download</a>
      </FileNotice>
    );
  }
  if (blob.rendered !== null) {
    return <div className={cx('markdown-body', styles.readmeBody)} onClick={routeLinks} dangerouslySetInnerHTML={{ __html: blob.rendered }} />;
  }
  return (
    <div className={styles.fileBody}>
      <table className={styles.code}>
        <tbody>
          {blob.lines.map((line, i) => (
            <tr key={i} id={`L${i + 1}`}>
              <td className={styles.lineNo}>{i + 1}</td>
              {/* Server-escaped, highlighted HTML (docs/SYNC_PROTOCOL.md §10). */}
              <td className={styles.lineCode} dangerouslySetInnerHTML={{ __html: line }} />
            </tr>
          ))}
        </tbody>
      </table>
      {blob.truncated && <FileNotice>Only the first part of this file is shown. <a href={blob.raw_url}>View raw</a></FileNotice>}
    </div>
  );
}

function FileView({ repo, refName, path }: { repo: Repo; refName: string; path: string }) {
  const { data: blob, error } = useResource<BlobView>(
    browseKeys.blob(repo.owner, repo.name, refName, path),
    () => getBlob(repo.owner, repo.name, refName, path),
    opts(refName),
  );
  if (error) return <EmptyState icon={AlertIcon} title="File not found" />;
  if (!blob) {
    return (
      <div className={styles.file}>
        <div className={styles.fileHeader}>
          <Skeleton width={180} />
        </div>
        <div className={styles.fileBody} style={{ padding: 16 }}>
          <Skeleton width="70%" />
        </div>
      </div>
    );
  }
  return (
    <div className={styles.file}>
      <div className={styles.fileHeader}>
        <span>
          {blob.lines ? `${blob.line_count} lines · ` : ''}
          {formatSize(blob.size)}
        </span>
        {blob.language && <span className={styles.lang}>{blob.language}</span>}
        <span style={{ flex: 1 }} />
        {blob.lines && (
          <IconButton
            icon={CopyIcon}
            label="Copy raw contents"
            size="sm"
            onClick={() => {
              void fetch(blob.raw_url, { credentials: 'same-origin' })
                .then((r) => r.text())
                .then((t) => navigator.clipboard?.writeText(t))
                .then(() => toast({ title: 'Copied to clipboard' }));
            }}
          />
        )}
        <Button size="sm" onClick={() => window.open(blob.raw_url, '_blank')}>
          Raw
        </Button>
      </div>
      <FileBody blob={blob} />
    </div>
  );
}

// ------------------------------------------------------------------ tree panel

function TreePanel({ repo, refName, current }: { repo: Repo; refName: string; current: string }) {
  return (
    <nav className={styles.tree} aria-label="Files">
      <div className={styles.treeTitle}>Files</div>
      <TreeDir repo={repo} refName={refName} path="" depth={0} current={current} />
    </nav>
  );
}

function TreeDir({ repo, refName, path, depth, current }: { repo: Repo; refName: string; path: string; depth: number; current: string }) {
  const { data } = useTree(repo, refName, path);
  if (!data) {
    return (
      <div style={{ paddingLeft: 12 + depth * 14 }} className={styles.treeLoading}>
        <Skeleton width={90} height={10} />
      </div>
    );
  }
  return (
    <>
      {data.entries.map((e) => (
        <TreeItem key={e.path} repo={repo} refName={refName} entry={e} depth={depth} current={current} />
      ))}
    </>
  );
}

function TreeItem({ repo, refName, entry, depth, current }: { repo: Repo; refName: string; entry: TreeEntry; depth: number; current: string }) {
  const isAncestor = current === entry.path || current.startsWith(`${entry.path}/`);
  const [open, setOpen] = useState(isAncestor);
  const [prevAncestor, setPrevAncestor] = useState(isAncestor);
  if (isAncestor !== prevAncestor) {
    setPrevAncestor(isAncestor);
    if (isAncestor) setOpen(true);
  }
  const indent = { paddingLeft: 8 + depth * 14 };
  if (entry.type === 'tree') {
    return (
      <>
        <button
          type="button"
          className={cx(styles.treeItem, current === entry.path && styles.treeActive)}
          style={indent}
          aria-expanded={open}
          onMouseEnter={() => prefetchTree(repo, refName, entry.path)}
          onClick={() => {
            setOpen((o) => !o);
            void load(browseKeys.tree(repo.owner, repo.name, refName, entry.path), () => getTree(repo.owner, repo.name, refName, entry.path), opts(refName));
          }}
        >
          {open ? <ChevronDownIcon size={12} /> : <ChevronRightIcon size={12} />}
          <FileDirectoryFillIcon size={14} className={styles.dirIcon} />
          <span className={styles.treeName}>{entry.name}</span>
        </button>
        {open && <TreeDir repo={repo} refName={refName} path={entry.path} depth={depth + 1} current={current} />}
      </>
    );
  }
  return (
    <Link
      to={`/${repo.owner}/${repo.name}/blob/${refName}/${entry.path}`}
      className={cx(styles.treeItem, current === entry.path && styles.treeActive)}
      style={{ paddingLeft: 8 + depth * 14 + 16 }}
      aria-current={current === entry.path ? 'page' : undefined}
      onMouseEnter={() => prefetchBlob(repo, refName, entry.path)}
    >
      <FileIcon size={14} />
      <span className={styles.treeName}>{entry.name}</span>
    </Link>
  );
}

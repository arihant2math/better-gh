import { observer } from 'mobx-react-lite';
import { useRef, useState } from 'react';
import { load, prefetch, useResource } from '../../api/cache';
import { decodeContent, getContents, getHighlightedBlob, listBranches, listCommits } from '../../api/endpoints';
import type { ContentEntry, ContentFile, Contents, HighlightedBlob, RestBranch, RestCommit } from '../../api/types';
import { Link, navigate, useLocation, useParams } from '../../router';
import type { Repo } from '../../sync/models';
import { repoByName } from '../../sync/selectors';
import { Avatar } from '../../ui/Badge';
import { Button, IconButton, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import {
  AlertIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronRightIcon,
  CopyIcon,
  FileDirectoryFillIcon,
  FileIcon,
  GitBranchIcon,
  BookIcon,
} from '../../ui/icons';
import { Markdown } from '../../ui/Markdown';
import { Menu } from '../../ui/Menu';
import { RelativeTime } from '../../ui/RelativeTime';
import { toast } from '../../ui/Toast';
import styles from './CodePage.module.css';

const contentsKey = (repo: Repo, ref: string, path: string) => `contents:${repo.owner}/${repo.name}@${ref}:${path}`;

function useContents(repo: Repo, ref: string, path: string) {
  return useResource<Contents>(contentsKey(repo, ref, path), () => getContents(repo.owner, repo.name, path, ref));
}

function prefetchPath(repo: Repo, ref: string, path: string) {
  prefetch(contentsKey(repo, ref, path), () => getContents(repo.owner, repo.name, path, ref));
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
  const branches = useResource<RestBranch[]>(open ? `branches:${repo.owner}/${repo.name}` : null, () => listBranches(repo.owner, repo.name));
  return (
    <>
      <Button ref={ref} size="sm" leadingIcon={GitBranchIcon} trailingIcon={ChevronDownIcon} onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        {refName}
      </Button>
      <Menu
        open={open}
        onClose={() => setOpen(false)}
        anchor={ref}
        aria-label="Switch branch"
        items={
          branches.data
            ? [
                { header: 'Branches', id: 'h' },
                ...branches.data.map((b) => ({
                  id: b.name,
                  label: b.name,
                  leading: <span style={{ width: 16, display: 'inline-flex', color: 'var(--accent-fg)' }}>{b.name === refName && <CheckIcon size={16} />}</span>,
                  trailing: b.name === repo.defaultBranch ? 'default' : undefined,
                  onSelect: () => navigate(`/${repo.owner}/${repo.name}/${isBlob ? 'blob' : 'tree'}/${b.name}${path ? `/${path}` : ''}`),
                })),
              ]
            : [{ id: 'loading', label: 'Loading branches…', disabled: true }]
        }
      />
    </>
  );
}

function LastCommit({ repo, refName, path }: { repo: Repo; refName: string; path: string }) {
  const { data } = useResource<RestCommit[]>(`lastcommit:${repo.owner}/${repo.name}@${refName}:${path}`, () =>
    listCommits(repo.owner, repo.name, { sha: refName, path, perPage: 1 }),
  );
  const c = data?.[0];
  return (
    <div className={styles.lastCommit}>
      {c ? (
        <>
          <Avatar user={c.author ? { login: c.author.login, avatarUrl: c.author.avatar_url } : null} size={20} />
          <strong>{c.author?.login ?? c.commit.author.name}</strong>
          <span className={styles.commitMsg}>{c.commit.message.split('\n')[0]}</span>
          <code className={styles.sha}>{c.sha.slice(0, 7)}</code>
          <span className={styles.subtle}>
            <RelativeTime date={c.commit.author.date} />
          </span>
        </>
      ) : (
        <Skeleton width={320} />
      )}
    </div>
  );
}

function DirView({ repo, refName, path }: { repo: Repo; refName: string; path: string }) {
  const { data, error } = useContents(repo, refName, path);
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
  if (!Array.isArray(data)) return <FileView repo={repo} refName={refName} path={path} />;
  const readme = data.find((e) => e.type === 'file' && /^readme(\.md)?$/i.test(e.name));
  return (
    <>
      <div className={styles.listing} role="list">
        {path && (
          <Link to={`/${repo.owner}/${repo.name}/tree/${refName}/${path.split('/').slice(0, -1).join('/')}`} className={styles.entry}>
            <FileDirectoryFillIcon size={16} className={styles.dirIcon} />
            <span>..</span>
          </Link>
        )}
        {data.map((e) => (
          <EntryRow key={e.path} repo={repo} refName={refName} entry={e} />
        ))}
      </div>
      {readme && <Readme repo={repo} refName={refName} path={readme.path} />}
    </>
  );
}

function EntryRow({ repo, refName, entry }: { repo: Repo; refName: string; entry: ContentEntry }) {
  const to = `/${repo.owner}/${repo.name}/${entry.type === 'dir' ? 'tree' : 'blob'}/${refName}/${entry.path}`;
  return (
    <Link to={to} className={styles.entry} role="listitem">
      {entry.type === 'dir' ? <FileDirectoryFillIcon size={16} className={styles.dirIcon} /> : <FileIcon size={16} className={styles.fileIcon} />}
      <span className={styles.entryName}>{entry.name}</span>
      {entry.type === 'file' && <span className={styles.subtle}>{formatSize(entry.size)}</span>}
    </Link>
  );
}

function Readme({ repo, refName, path }: { repo: Repo; refName: string; path: string }) {
  const { data } = useContents(repo, refName, path);
  const file = data && !Array.isArray(data) && data.type === 'file' ? (data as ContentFile) : null;
  return (
    <section className={styles.readme}>
      <header className={styles.readmeHeader}>
        <BookIcon size={16} /> README
      </header>
      <div className={styles.readmeBody}>{file ? <Markdown source={decodeContent(file.content)} repo={`${repo.owner}/${repo.name}`} /> : <Skeleton width="60%" />}</div>
    </section>
  );
}

function formatSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function FileView({ repo, refName, path }: { repo: Repo; refName: string; path: string }) {
  const { data, error } = useContents(repo, refName, path);
  const file = data && !Array.isArray(data) ? (data as ContentFile) : null;
  // Highlighting is keyed by blob sha → immutable, cached forever.
  const hl = useResource<HighlightedBlob | null>(file ? `hl:${file.sha}` : null, () => getHighlightedBlob(repo.owner, repo.name, file!.sha, path), {
    immutable: true,
  });
  if (error) return <EmptyState icon={AlertIcon} title="File not found" />;
  if (!file) {
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
  const text = decodeContent(file.content);
  const lines = text.replace(/\n$/, '').split('\n');
  const isMarkdown = /\.md$/i.test(path);
  return (
    <div className={styles.file}>
      <div className={styles.fileHeader}>
        <span>
          {lines.length} lines · {formatSize(file.size)}
        </span>
        {hl.data && <span className={styles.lang}>{hl.data.language}</span>}
        <span style={{ flex: 1 }} />
        <IconButton
          icon={CopyIcon}
          label="Copy raw contents"
          size="sm"
          onClick={() => {
            void navigator.clipboard?.writeText(text);
            toast({ title: 'Copied to clipboard' });
          }}
        />
        <Button size="sm" onClick={() => window.open(file.download_url ?? '#', '_blank')}>
          Raw
        </Button>
      </div>
      {isMarkdown ? (
        <div className={styles.readmeBody}>
          <Markdown source={text} repo={`${repo.owner}/${repo.name}`} />
        </div>
      ) : (
        <div className={styles.fileBody}>
          <table className={styles.code}>
            <tbody>
              {lines.map((line, i) => (
                <tr key={i} id={`L${i + 1}`}>
                  <td className={styles.lineNo}>{i + 1}</td>
                  {hl.data?.lines[i] !== undefined ? (
                    <td className={styles.lineCode} dangerouslySetInnerHTML={{ __html: hl.data.lines[i] }} />
                  ) : (
                    <td className={styles.lineCode}>{line}</td>
                  )}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
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
  const { data } = useContents(repo, refName, path);
  if (!data || !Array.isArray(data)) {
    return (
      <div style={{ paddingLeft: 12 + depth * 14 }} className={styles.treeLoading}>
        <Skeleton width={90} height={10} />
      </div>
    );
  }
  return (
    <>
      {data.map((e) => (
        <TreeEntry key={e.path} repo={repo} refName={refName} entry={e} depth={depth} current={current} />
      ))}
    </>
  );
}

function TreeEntry({ repo, refName, entry, depth, current }: { repo: Repo; refName: string; entry: ContentEntry; depth: number; current: string }) {
  const isAncestor = current === entry.path || current.startsWith(`${entry.path}/`);
  const [open, setOpen] = useState(isAncestor);
  const [prevAncestor, setPrevAncestor] = useState(isAncestor);
  if (isAncestor !== prevAncestor) {
    setPrevAncestor(isAncestor);
    if (isAncestor) setOpen(true);
  }
  const indent = { paddingLeft: 8 + depth * 14 };
  if (entry.type === 'dir') {
    return (
      <>
        <button
          type="button"
          className={cx(styles.treeItem, current === entry.path && styles.treeActive)}
          style={indent}
          aria-expanded={open}
          onMouseEnter={() => prefetchPath(repo, refName, entry.path)}
          onClick={() => {
            setOpen((o) => !o);
            void load(contentsKey(repo, refName, entry.path), () => getContents(repo.owner, repo.name, entry.path, refName));
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
    >
      <FileIcon size={14} />
      <span className={styles.treeName}>{entry.name}</span>
    </Link>
  );
}

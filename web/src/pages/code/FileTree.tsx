import { useState } from 'react';
import type { TreeEntry } from '../../api/types';
import { Link } from '../../router';
import { IconButton, cx } from '../../ui/Button';
import { Skeleton } from '../../ui/EmptyState';
import {
  ChevronDownIcon,
  ChevronRightIcon,
  FileDirectoryFillIcon,
  FileDirectoryOpenFillIcon,
  FileIcon,
  FileSubmoduleIcon,
  FileSymlinkFileIcon,
  SearchIcon,
  SidebarCollapseIcon,
} from '../../ui/icons';
import { Kbd } from '../../ui/Badge';
import styles from './Code.module.css';
import { prefetchBlob, prefetchFileList, prefetchTree, useTree } from './data';
import { codeUrl, isSettled, type CodeTarget } from './util';

/** Lazily expanding file tree (left panel); every directory is cached by commit SHA. */
export function FileTree({ t, onCollapse, onFind }: { t: CodeTarget; onCollapse: () => void; onFind: () => void }) {
  return (
    <nav className={styles.tree} aria-label="Files">
      <div className={styles.treeHead}>
        <span className={styles.treeTitle}>Files</span>
        <IconButton icon={SidebarCollapseIcon} label="Hide file tree" shortcut="shift+." size="sm" variant="ghost" onClick={onCollapse} />
      </div>
      <button type="button" className={styles.treeFind} onClick={onFind} onMouseEnter={() => prefetchFileList(t)}>
        <SearchIcon size={14} />
        <span>Go to file</span>
        <Kbd>t</Kbd>
      </button>
      <div className={styles.treeBody} role="tree">
        <TreeDir t={t} path="" depth={0} />
      </div>
    </nav>
  );
}

function TreeDir({ t, path, depth }: { t: CodeTarget; path: string; depth: number }) {
  const { data, error } = useTree(t, path, isSettled(t));
  if (error) return null;
  if (!data) {
    return (
      <div style={{ paddingLeft: 12 + depth * 12 }} className={styles.treeLoading}>
        <Skeleton width={90} height={10} />
      </div>
    );
  }
  return (
    <>
      {data.entries.map((e) => (
        <TreeItem key={e.path} t={t} entry={e} depth={depth} />
      ))}
    </>
  );
}

function TreeItem({ t, entry, depth }: { t: CodeTarget; entry: TreeEntry; depth: number }) {
  const current = t.path;
  const isAncestor = current === entry.path || current.startsWith(`${entry.path}/`);
  const [open, setOpen] = useState(isAncestor);
  const [prevAncestor, setPrevAncestor] = useState(isAncestor);
  if (isAncestor !== prevAncestor) {
    setPrevAncestor(isAncestor);
    if (isAncestor) setOpen(true);
  }
  const indent = { paddingLeft: 6 + depth * 12 };
  const active = current === entry.path;
  if (entry.type === 'tree') {
    return (
      <>
        <div className={cx(styles.treeItem, active && styles.treeActive)} style={indent} role="treeitem" aria-expanded={open}>
          <button
            type="button"
            className={styles.treeToggle}
            aria-label={open ? `Collapse ${entry.name}` : `Expand ${entry.name}`}
            onMouseEnter={() => prefetchTree(t, entry.path)}
            onClick={() => setOpen((o) => !o)}
          >
            {open ? <ChevronDownIcon size={12} /> : <ChevronRightIcon size={12} />}
          </button>
          <Link
            to={codeUrl(t, 'tree', t.ref, entry.path)}
            className={styles.treeLink}
            onMouseEnter={() => prefetchTree(t, entry.path)}
            onClick={() => setOpen(true)}
            aria-current={active ? 'page' : undefined}
          >
            {open ? <FileDirectoryOpenFillIcon size={14} className={styles.dirIcon} /> : <FileDirectoryFillIcon size={14} className={styles.dirIcon} />}
            <span className={styles.treeName}>{entry.name}</span>
          </Link>
        </div>
        {open && (
          <div role="group">
            <TreeDir t={t} path={entry.path} depth={depth + 1} />
          </div>
        )}
      </>
    );
  }
  const Icon = entry.type === 'symlink' ? FileSymlinkFileIcon : entry.type === 'commit' ? FileSubmoduleIcon : FileIcon;
  return (
    <div className={cx(styles.treeItem, active && styles.treeActive)} style={{ paddingLeft: indent.paddingLeft + 18 }} role="treeitem">
      <Link
        to={codeUrl(t, 'blob', t.ref, entry.path)}
        className={styles.treeLink}
        aria-current={active ? 'page' : undefined}
        onMouseEnter={() => entry.type !== 'commit' && prefetchBlob(t, entry.path)}
      >
        <Icon size={14} className={styles.fileIcon} />
        <span className={styles.treeName}>{entry.name}</span>
      </Link>
    </div>
  );
}

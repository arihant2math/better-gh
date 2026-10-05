import { observer } from 'mobx-react-lite';
import { useState } from 'react';
import { useResource } from '../../api/cache';
import { getTreeCommits, browseKeys } from '../../api/endpoints';
import type { BrowseCommit, LastCommits, TreeEntry } from '../../api/types';
import { Link, navigate } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import type { Repo } from '../../sync/models';
import { cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, BookIcon, FileDirectoryFillIcon, FileIcon, FileSubmoduleIcon, FileSymlinkFileIcon, LawIcon } from '../../ui/icons';
import { RelativeTime } from '../../ui/RelativeTime';
import styles from './Code.module.css';
import { prefetchBlob, prefetchTree, useTree } from './data';
import { LastCommitBar } from './LastCommitBar';
import { codeUrl, parentPath, routeLinks, type CodeTarget } from './util';

/** Directory listing (+ last commit per entry) and the rendered README. */
export const DirView = observer(function DirView({ t, repo, root }: { t: CodeTarget; repo: Repo; root: boolean }) {
  const { data, error } = useTree(t);
  // Last commit per entry: inlined when the server has it cached, otherwise
  // fetched by commit SHA (immutable).
  const commits = useResource<LastCommits>(
    data && !data.last_commits ? browseKeys.treeCommits(t.owner, t.repo, data.commit, data.path) : null,
    () => getTreeCommits(t.owner, t.repo, data!.commit, data!.path),
    { immutable: true },
  );
  const [cursor, setCursor] = useState(-1);
  const entries = data?.entries ?? [];
  const open = (e: TreeEntry) => navigate(codeUrl(t, e.type === 'tree' ? 'tree' : 'blob', t.ref, e.path));
  useShortcuts('Directory', {
    j: { handler: () => setCursor((c) => Math.min(entries.length - 1, c + 1)), description: 'Next entry', group: 'Code' },
    k: { handler: () => setCursor((c) => Math.max(0, c - 1)), description: 'Previous entry', group: 'Code' },
    enter: { handler: () => (cursor >= 0 && entries[cursor] ? open(entries[cursor]) : false), description: 'Open entry', group: 'Code' },
    o: () => (cursor >= 0 && entries[cursor] ? open(entries[cursor]) : false),
    backspace: { handler: () => (t.path ? navigate(codeUrl(t, 'tree', t.ref, parentPath(t.path))) : false), description: 'Parent directory', group: 'Code' },
  });

  if (error) {
    const empty = root && (error as { status?: number }).status === 404;
    return empty ? <EmptyRepo repo={repo} /> : <EmptyState icon={AlertIcon} title="This path does not exist" />;
  }
  const last = data?.last_commits ?? commits.data?.entries;
  return (
    <>
      <div className={styles.listingBox}>
        <LastCommitBar t={t} path={t.path} />
        <div className={styles.listing} role="list" aria-label="Files">
          {!data ? (
            Array.from({ length: 8 }, (_, i) => (
              <div key={i} className={styles.entry}>
                <Skeleton width={`${20 + ((i * 17) % 30)}%`} />
              </div>
            ))
          ) : (
            <>
              {t.path && (
                <Link to={codeUrl(t, 'tree', t.ref, parentPath(t.path))} className={styles.entry} aria-label="Parent directory">
                  <FileDirectoryFillIcon size={16} className={styles.dirIcon} />
                  <span className={styles.entryName}>..</span>
                </Link>
              )}
              {entries.map((e, i) => (
                <EntryRow key={e.path} t={t} entry={e} commit={last?.[e.name]} pending={!last} active={i === cursor} />
              ))}
            </>
          )}
        </div>
      </div>
      {data?.readme && (
        <section className={styles.readme} id="readme">
          <header className={styles.readmeHeader}>
            {/^license/i.test(data.readme.name) ? <LawIcon size={16} /> : <BookIcon size={16} />}
            <Link to={codeUrl(t, 'blob', t.ref, data.readme.path)}>{data.readme.name}</Link>
          </header>
          {/* Sanitized server-side HTML (bgh_core::markdown). */}
          <div className={cx('markdown-body', styles.readmeBody)} onClick={routeLinks} dangerouslySetInnerHTML={{ __html: data.readme.html }} />
        </section>
      )}
    </>
  );
});

function EntryRow({ t, entry, commit, pending, active }: { t: CodeTarget; entry: TreeEntry; commit?: BrowseCommit; pending: boolean; active: boolean }) {
  const isDir = entry.type === 'tree';
  const Icon = isDir ? FileDirectoryFillIcon : entry.type === 'symlink' ? FileSymlinkFileIcon : entry.type === 'commit' ? FileSubmoduleIcon : FileIcon;
  return (
    <div className={cx(styles.entry, active && styles.entryActive)} role="listitem" ref={(el) => void (active && el?.scrollIntoView({ block: 'nearest' }))}>
      <Icon size={16} className={isDir ? styles.dirIcon : styles.fileIcon} />
      <Link
        to={codeUrl(t, isDir ? 'tree' : 'blob', t.ref, entry.path)}
        className={styles.entryName}
        onMouseEnter={() => (isDir ? prefetchTree(t, entry.path) : entry.type !== 'commit' && prefetchBlob(t, entry.path))}
      >
        {entry.name}
      </Link>
      <span className={styles.entryCommit}>
        {commit ? (
          <Link to={`/${t.owner}/${t.repo}/commit/${commit.sha}`} className={styles.muted} title={commit.message}>
            {commit.summary}
          </Link>
        ) : pending ? (
          <Skeleton width={160} />
        ) : null}
      </span>
      <span className={styles.entryTime}>{commit && <RelativeTime date={commit.committer.date} />}</span>
    </div>
  );
}

function EmptyRepo({ repo }: { repo: Repo }) {
  const url = `${window.location.origin}/${repo.owner}/${repo.name}.git`;
  return (
    <div className={styles.emptyRepo}>
      <h2>Quick setup</h2>
      <p className={styles.muted}>This repository is empty. Push an existing repository from the command line:</p>
      <pre className={styles.snippet}>{`git remote add origin ${url}\ngit branch -M ${repo.defaultBranch}\ngit push -u origin ${repo.defaultBranch}`}</pre>
      <p className={styles.muted}>…or create a new repository on the command line:</p>
      <pre className={styles.snippet}>{`echo "# ${repo.name}" >> README.md\ngit init\ngit add README.md\ngit commit -m "first commit"\ngit branch -M ${repo.defaultBranch}\ngit remote add origin ${url}\ngit push -u origin ${repo.defaultBranch}`}</pre>
    </div>
  );
}

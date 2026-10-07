import { observer } from 'mobx-react-lite';
import { lazy, Suspense, useState } from 'react';
import { isSha } from '../../api/endpoints';
import { RefPicker } from '../../components/code/RefPicker';
import { Link, navigate, useLocation, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { store } from '../../sync';
import type { Repo } from '../../sync/models';
import { repoByName } from '../../sync/selectors';
import { Button, IconButton, cx } from '../../ui/Button';
import { CopyIcon, SearchIcon, SidebarExpandIcon } from '../../ui/icons';
import { toast } from '../../ui/Toast';
import { AboutSidebar } from './AboutSidebar';
import { AddFileMenu, CloneMenu } from './CloneMenu';
import styles from './Code.module.css';
import { prefetchFileList, useRefsData, useTree } from './data';
import { DirView } from './DirView';
import { FileTree } from './FileTree';
import { FileView } from './FileView';
import { copyText, resolveTarget, setHash, type CodeTarget } from './util';
import { codeUrl, historyUrl, parseCodeUrl } from '../../components/code/urls';

const FileFinder = lazy(() => import('./FileFinder'));

const TREE_KEY = 'bgh:code:tree';

/** Repository home, directory, file and blame views (Code tab). */
export default observer(function CodePage() {
  const params = useParams<{ owner: string; repo: string; ref?: string; '*'?: string }>();
  const { pathname } = useLocation();
  const repo = repoByName(params.owner, params.repo);
  // Re-resolve once the ref list arrives (pins the commit SHA for immutable caching).
  useRefsData(params.owner, params.repo);
  if (!repo) return null;
  const view = parseCodeUrl(pathname)?.view;
  const mode: 'tree' | 'blob' | 'blame' = view === 'blob' || view === 'blame' ? view : 'tree';
  const t = resolveTarget(repo.owner, repo.name, params.ref ?? repo.defaultBranch, params['*'] ?? '');
  return <CodeView key={repo.id} repo={repo} t={t} mode={mode} />;
});

const CodeView = observer(function CodeView({ repo, t, mode }: { repo: Repo; t: CodeTarget; mode: 'tree' | 'blob' | 'blame' }) {
  const [finder, setFinder] = useState(false);
  const [picker, setPicker] = useState(false);
  const [treeOpen, setTreeOpen] = useState(() => {
    try {
      return localStorage.getItem(TREE_KEY) !== '0';
    } catch {
      return true;
    }
  });
  const toggleTree = () => {
    setTreeOpen((o) => {
      try {
        localStorage.setItem(TREE_KEY, o ? '0' : '1');
      } catch {
        /* private mode */
      }
      return !o;
    });
  };
  const root = !t.path && mode === 'tree';
  // Shares DirView's request; `empty` is the server's "no commits yet" signal.
  const homeRes = useTree(t, '', root);
  const home = homeRes.data;
  const empty = root && home?.empty === true;
  const permission = store().get('viewerRepo', repo.id)?.permission;
  const canPush = permission === 'admin' || permission === 'maintain' || permission === 'write';
  const switchRef = (ref: string) => navigate(codeUrl(t, mode, ref, t.path) + (mode === 'tree' ? '' : window.location.hash));

  useShortcuts('Code', {
    t: { handler: () => (empty ? false : setFinder(true)), description: 'Go to file', group: 'Code' },
    w: { handler: () => setPicker(true), description: 'Switch branch or tag', group: 'Code' },
    y: {
      handler: () => {
        if (!t.commit || isSha(t.ref)) return false;
        const url = codeUrl(t, mode, t.commit, t.path) + window.location.hash;
        navigate(url, { replace: true });
        void copyText(window.location.origin + url);
        toast({ title: 'Permalink copied', description: `Pinned to ${t.commit.slice(0, 7)}` });
      },
      description: 'Expand URL to its canonical (commit SHA) form',
      group: 'Code',
    },
    b: {
      handler: () => {
        if (mode === 'tree') return false;
        navigate(codeUrl(t, mode === 'blame' ? 'blob' : 'blame', t.ref, t.path) + window.location.hash);
      },
      description: 'Toggle blame',
      group: 'Code',
    },
    '.': {
      handler: () => {
        if (mode !== 'blob' || !canPush) return false;
        navigate(codeUrl(t, 'edit', t.ref, t.path));
      },
      description: 'Edit file',
      group: 'Code',
    },
    'shift+.': { handler: () => toggleTree(), description: 'Toggle file tree', group: 'Code' },
    escape: {
      handler: () => {
        if (!window.location.hash) return false;
        setHash('');
      },
    },
  });

  const showTree = treeOpen && !root;
  return (
    <div className={cx(styles.page, showTree && styles.withTree)}>
      {showTree && <FileTree t={t} onCollapse={toggleTree} onFind={() => setFinder(true)} />}
      <div className={styles.main}>
        <div className={styles.bar}>
          {!showTree && !root && <IconButton icon={SidebarExpandIcon} label="Show file tree" shortcut="shift+." size="sm" variant="ghost" onClick={toggleTree} />}
          <RefPicker owner={t.owner} repo={t.repo} value={t.ref} onSelect={switchRef} allowCreate={canPush} open={picker} onOpenChange={setPicker} />
          {root ? (
            !empty && <RepoNav repo={repo} t={t} />
          ) : (
            <PathCrumbs t={t} mode={mode} />
          )}
          <span className={styles.grow} />
          {!empty && (
            <Button size="sm" leadingIcon={SearchIcon} kbd="t" onClick={() => setFinder(true)} onMouseEnter={() => prefetchFileList(t)}>
              Go to file
            </Button>
          )}
          {canPush && mode === 'tree' && <AddFileMenu t={t} />}
          {root && <CloneMenu repo={repo} t={t} />}
        </div>
        <div className={cx(root && styles.home)}>
          <div className={styles.content}>
            {mode === 'tree' ? <DirView t={t} repo={repo} root={root} /> : <FileView t={t} repo={repo} blame={mode === 'blame'} canPush={canPush} />}
          </div>
          {root && <AboutSidebar repo={repo} empty={home?.empty} loaded={!!home || !!homeRes.error} hasReadme={!!home?.readme} />}
        </div>
      </div>
      {finder && (
        <Suspense fallback={null}>
          <FileFinder t={t} open={finder} onClose={() => setFinder(false)} />
        </Suspense>
      )}
    </div>
  );
});

function RepoNav({ repo, t }: { repo: Repo; t: CodeTarget }) {
  const base = `/${repo.owner}/${repo.name}`;
  return (
    <nav className={styles.repoNav} aria-label="Repository">
      <Link to={`${base}/branches`}>Branches</Link>
      <Link to={`${base}/tags`}>Tags</Link>
      <Link to={historyUrl(t, t.ref)}>Commits</Link>
    </nav>
  );
}

function PathCrumbs({ t, mode }: { t: CodeTarget; mode: 'tree' | 'blob' | 'blame' }) {
  const parts = t.path ? t.path.split('/') : [];
  return (
    <nav className={styles.crumbs} aria-label="Path">
      <Link to={codeUrl(t, 'tree', t.ref)} className={styles.crumbRoot}>
        {t.repo}
      </Link>
      {parts.map((p, i) => (
        <span key={i} className={styles.crumbPart}>
          <span className={styles.crumbSep}>/</span>
          {i === parts.length - 1 ? (
            <strong>{p}</strong>
          ) : (
            <Link to={codeUrl(t, 'tree', t.ref, parts.slice(0, i + 1).join('/'))}>{p}</Link>
          )}
        </span>
      ))}
      {t.path && (
        <IconButton
          icon={CopyIcon}
          label="Copy path"
          size="sm"
          variant="ghost"
          onClick={() => void copyText(t.path).then(() => toast({ title: 'Path copied' }))}
        />
      )}
      {mode !== 'tree' && <span className={styles.crumbSep} />}
    </nav>
  );
}

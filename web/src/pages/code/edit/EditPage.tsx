import { observer } from 'mobx-react-lite';
import { useMemo, useState } from 'react';
import { useResource } from '../../../api/cache';
import { ApiError } from '../../../api/client';
import { fetchRaw } from '../../../api/code';
import { browseKeys, getBlob, getTree, isSha } from '../../../api/endpoints';
import type { BlobView } from '../../../api/types';
import { CommitDialog, CommitError, type CommitRequest } from '../../../components/code/CommitDialog';
import { navigate, useLocation } from '../../../router';
import { blobUrl, parseCodeUrl, repoRefOf, treeUrl } from '../../../components/code/urls';
import { useShortcuts } from '../../../shortcuts/useShortcuts';
import { Button } from '../../../ui/Button';
import { EmptyState } from '../../../ui/EmptyState';
import { AlertIcon, CodeIcon, EyeIcon, FileIcon, GitCommitIcon, TrashIcon } from '../../../ui/icons';
import { Select } from '../../../ui/Input';
import { Markdown } from '../../../ui/Markdown';
import { Spinner } from '../../../ui/Spinner';
import { Tabs } from '../../../ui/Tabs';
import { CodeEditor } from './CodeEditor';
import { basename, classifyError, commitFiles, dirname, finishCommit, formatBytes, joinPath, pathProblem, prepareTarget, removeFile, writeFile } from './commit';
import { DiffPreview } from './DiffPreview';
import { PathInput } from './PathInput';
import { NoPushNotice, Notice, directBlockedReason, useEditTarget, useUnloadGuard } from './shared';
import { defaultIndentFor, detectEol, detectIndent, isMarkdownPath, type IndentStyle } from './text';
import styles from './Edit.module.css';

type Mode = 'edit' | 'new' | 'delete';
type Target = ReturnType<typeof useEditTarget>;

/** `/:owner/:repo/{edit|new|delete}/:ref/*` — in-browser file editing. */
export default observer(function EditPage() {
  const { pathname } = useLocation();
  const t = useEditTarget();
  const view = parseCodeUrl(pathname)?.view;
  const mode: Mode = view === 'new' ? 'new' : view === 'delete' ? 'delete' : 'edit';
  return (
    <div className={styles.page}>
      {mode === 'new' ? (
        <FileEditor key={`new:${t.ref}:${t.path}`} t={t} mode="new" original="" eol={'\n'} originalPath={null} initialDir={t.path} />
      ) : mode === 'delete' ? (
        <DeleteFile key={`${t.ref}:${t.path}`} t={t} />
      ) : (
        <EditExisting key={`${t.ref}:${t.path}`} t={t} />
      )}
    </div>
  );
});

/** Blob view (same cache key as the code browser) + its raw text. */
function useBlobText(t: Target) {
  const blob = useResource<BlobView>(t.path ? browseKeys.blob(t.owner, t.name, t.ref, t.path) : null, () => getBlob(t.owner, t.name, t.ref, t.path), { immutable: isSha(t.ref) });
  const b = blob.data;
  const editable = !!b && b.type === 'file' && !b.binary && !b.too_large && !b.lfs;
  const raw = useResource<string>(b && editable ? `raw-blob:${t.owner}/${t.name}:${b.sha}` : null, () => fetchRaw(b!.raw_url), { immutable: true });
  return { blob, raw, editable };
}

function LoadError({ t, error }: { t: Target; error: unknown }) {
  const missing = error instanceof ApiError && error.status === 404;
  return (
    <EmptyState
      icon={AlertIcon}
      title={missing ? 'File not found' : 'Couldn’t load this file'}
      action={<Button onClick={() => navigate(treeUrl(repoRefOf(t), t.ref))}>Back to code</Button>}
    >
      {missing ? `${t.path} doesn’t exist on ${t.ref}.` : error instanceof Error ? error.message : String(error)}
    </EmptyState>
  );
}

const EditExisting = observer(function EditExisting({ t }: { t: Target }) {
  const { blob, raw, editable } = useBlobText(t);
  if (blob.error && !blob.data) return <LoadError t={t} error={blob.error} />;
  if (!blob.data || (editable && raw.data === undefined && !raw.error)) {
    return (
      <div className={styles.center}>
        <Spinner />
      </div>
    );
  }
  const b = blob.data;
  if (!editable) {
    const why = b.lfs ? 'stored with Git LFS' : b.binary ? 'a binary file' : b.too_large ? `too large to edit in the browser (${formatBytes(b.size)})` : `a ${b.type}`;
    return (
      <EmptyState
        icon={FileIcon}
        title="This file can’t be edited here"
        action={<Button onClick={() => navigate(blobUrl(repoRefOf(t), t.ref, t.path))}>View file</Button>}
      >
        {b.name} is {why}.
      </EmptyState>
    );
  }
  if (raw.error) return <LoadError t={t} error={raw.error} />;
  const text = raw.data!;
  return <FileEditor t={t} mode="edit" original={text.replace(/\r\n/g, '\n')} eol={detectEol(text)} originalPath={t.path} baseSha={b.sha} baseCommit={b.commit} initialDir={dirname(t.path)} />;
});

interface EditorProps {
  t: Target;
  mode: 'edit' | 'new';
  /** Original text (LF line endings). */
  original: string;
  eol: '\n' | '\r\n';
  originalPath: string | null;
  baseSha?: string;
  baseCommit?: string;
  initialDir: string;
}

const FileEditor = observer(function FileEditor({ t, mode, original, eol, originalPath, baseSha: initialBaseSha, baseCommit, initialDir }: EditorProps) {
  const { owner, name: repoName } = t;
  const [dir, setDir] = useState(initialDir);
  const [name, setName] = useState(originalPath ? basename(originalPath) : '');
  const [text, setText] = useState(original);
  const [tab, setTab] = useState<'edit' | 'preview'>('edit');
  const [indent, setIndent] = useState<IndentStyle>(() => detectIndent(original, defaultIndentFor(originalPath ?? '')));
  const [wrap, setWrap] = useState(() => isMarkdownPath(originalPath ?? ''));
  const [dialog, setDialog] = useState(false);
  const [baseSha, setBaseSha] = useState(initialBaseSha);
  const [conflict, setConflict] = useState<string | null>(null);
  const [pathError, setPathError] = useState<string | null>(null);
  const [overwriting, setOverwriting] = useState(false);

  const path = joinPath(dir, name);
  const renamed = mode === 'edit' && path !== originalPath;
  const changed = text !== original;
  const dirty = changed || renamed || (mode === 'new' && !!name);
  const problem = name || mode === 'edit' ? pathProblem(path) : null;
  const canCommit = t.canPush && !problem && !!name && (mode === 'new' || dirty);
  const markdown = isMarkdownPath(path);
  useUnloadGuard(dirty);

  const oldName = originalPath ? basename(originalPath) : '';
  const defaultMessage =
    mode === 'new' ? `Create ${name || 'new file'}` : renamed && changed ? `Update and rename ${oldName} to ${name}` : renamed ? `Rename ${oldName} to ${name}` : `Update ${name}`;

  const openDialog = () => {
    if (!canCommit) return;
    setDialog(true);
  };

  const cancel = () => {
    if (dirty && !window.confirm('You have unsaved changes. Discard them?')) return;
    navigate(mode === 'edit' ? blobUrl(repoRefOf(t), t.ref, originalPath ?? '') : treeUrl(repoRefOf(t), t.ref, initialDir));
  };

  useShortcuts('Editor', {
    'mod+s': { handler: () => openDialog(), description: 'Commit changes', group: 'Editor' },
    'mod+enter': { handler: () => openDialog(), description: 'Commit changes', group: 'Editor' },
    'mod+shift+p': { handler: () => setTab((x) => (x === 'edit' ? 'preview' : 'edit')), description: 'Toggle preview', group: 'Editor' },
    escape: () => {
      if (tab !== 'preview') return false;
      setTab('edit');
    },
  });

  const onCommit = async (req: CommitRequest) => {
    const content = eol === '\r\n' ? text.replace(/\n/g, '\r\n') : text;
    if (renamed) {
      // Refuse to silently overwrite another file.
      const exists = await getBlob(owner, repoName, req.branch, path).then(
        () => true,
        () => false,
      );
      if (exists) {
        setDialog(false);
        setPathError(`A file named ${name} already exists${dir ? ` in ${dir}` : ''}.`);
        return;
      }
    }
    const branch = await prepareTarget(owner, repoName, req, baseCommit);
    try {
      const result =
        mode === 'new'
          ? await writeFile(owner, repoName, branch, path, content, req.fullMessage)
          : renamed
            ? await commitFiles(owner, repoName, branch, [{ path, content }, { path: originalPath!, content: null }], req.fullMessage, { expect: baseSha ? { [originalPath!]: baseSha } : {} })
            : await writeFile(owner, repoName, branch, path, content, req.fullMessage, baseSha);
      finishCommit(owner, repoName, req, result, blobUrl(repoRefOf(t), result.branch, path));
    } catch (e) {
      const err = classifyError(e);
      if (err.kind === 'conflict') {
        setDialog(false);
        setConflict(`Someone has committed changes to ${originalPath ?? path} since you started editing.`);
        return;
      }
      if (err.kind === 'exists') {
        setDialog(false);
        setPathError(`A file named ${name} already exists${dir ? ` in ${dir}` : ''}.`);
        return;
      }
      throw err;
    }
  };

  const overwrite = async () => {
    if (!originalPath) return;
    setOverwriting(true);
    try {
      const latest = await getBlob(owner, repoName, t.ref, originalPath).catch((e: unknown) => {
        if (e instanceof ApiError && e.status === 404) return null;
        throw e;
      });
      setBaseSha(latest?.sha);
      setConflict(null);
      setDialog(true);
    } catch (e) {
      setConflict(classifyError(e).message);
    } finally {
      setOverwriting(false);
    }
  };

  const latestUrl = blobUrl(repoRefOf(t), t.ref, originalPath ?? path);

  return (
    <>
      <div className={styles.header}>
        <PathInput
          owner={owner}
          repo={repoName}
          refName={t.ref}
          dir={dir}
          name={name}
          onChange={(d, n) => {
            setDir(d);
            setName(n);
            setPathError(null);
          }}
          invalid={!!(pathError || problem)}
          autoFocus={mode === 'new'}
        />
        <div className={styles.headerActions}>
          <Button onClick={cancel}>Cancel changes</Button>
          <Button variant="primary" leadingIcon={GitCommitIcon} disabled={!canCommit} onClick={openDialog}>
            Commit changes…
          </Button>
        </div>
      </div>
      {(pathError || problem) && <div className={styles.pathError}>{pathError ?? problem}</div>}
      {!t.canPush && <NoPushNotice repo={`${owner}/${repoName}`} />}
      {conflict && (
        <Notice
          kind="danger"
          actions={
            <>
              <Button size="sm" onClick={() => window.open(latestUrl, '_blank', 'noopener')}>
                View latest
              </Button>
              <Button size="sm" variant="danger" loading={overwriting} onClick={() => void overwrite()}>
                Overwrite
              </Button>
            </>
          }
        >
          <strong>Conflict.</strong> {conflict} Review the latest version, or overwrite it with your changes.
        </Notice>
      )}
      <div className={styles.editorBox}>
        <div className={styles.toolbar}>
          <Tabs
            size="sm"
            value={tab}
            onChange={(v) => setTab(v as 'edit' | 'preview')}
            items={[
              { id: 'edit', label: mode === 'new' ? 'Edit new file' : 'Edit', icon: CodeIcon },
              { id: 'preview', label: markdown ? 'Preview' : 'Preview changes', icon: EyeIcon },
            ]}
          />
          {tab === 'edit' && (
            <div className={styles.settings}>
              <Select aria-label="Indent mode" value={indent.tabs ? 'tabs' : 'spaces'} onChange={(e) => setIndent({ ...indent, tabs: e.target.value === 'tabs' })}>
                <option value="spaces">Spaces</option>
                <option value="tabs">Tabs</option>
              </Select>
              <Select aria-label="Indent size" value={indent.size} onChange={(e) => setIndent({ ...indent, size: Number(e.target.value) })}>
                {[2, 4, 8].map((n) => (
                  <option key={n} value={n}>
                    {n}
                  </option>
                ))}
              </Select>
              <Select aria-label="Line wrap mode" value={wrap ? 'soft' : 'off'} onChange={(e) => setWrap(e.target.value === 'soft')}>
                <option value="off">No wrap</option>
                <option value="soft">Soft wrap</option>
              </Select>
            </div>
          )}
        </div>
        {tab === 'edit' ? (
          <CodeEditor value={text} onChange={setText} wrap={wrap} indent={indent} autoFocus={mode === 'edit'} aria-label={`Contents of ${path || 'new file'}`} />
        ) : markdown ? (
          <div className={styles.preview}>
            <Markdown source={text} repo={`${owner}/${repoName}`} />
          </div>
        ) : (
          <DiffPreview before={original} after={text} />
        )}
      </div>
      <CommitDialog
        open={dialog}
        onClose={() => setDialog(false)}
        owner={owner}
        repo={repoName}
        branch={t.ref}
        defaultMessage={defaultMessage}
        canCommitDirectly={t.isBranch}
        directBlockedReason={directBlockedReason(t)}
        onCommit={onCommit}
      />
    </>
  );
});

const DeleteFile = observer(function DeleteFile({ t }: { t: Target }) {
  const { owner, name: repoName } = t;
  const blob = useResource<BlobView>(t.path ? browseKeys.blob(owner, repoName, t.ref, t.path) : null, () => getBlob(owner, repoName, t.ref, t.path), { immutable: isSha(t.ref) });
  const [dialog, setDialog] = useState(false);
  const fileName = basename(t.path);
  const parent = dirname(t.path);
  const summary = useMemo(() => {
    const b = blob.data;
    if (!b) return '';
    return b.binary || b.too_large ? formatBytes(b.size) : `${b.line_count} line${b.line_count === 1 ? '' : 's'} · ${formatBytes(b.size)}`;
  }, [blob.data]);

  useShortcuts('Delete file', {
    'mod+enter': { handler: () => t.canPush && !!blob.data && setDialog(true), description: 'Commit changes', group: 'Editor' },
  });

  if (blob.error && !blob.data) return <LoadError t={t} error={blob.error} />;
  if (!blob.data) {
    return (
      <div className={styles.center}>
        <Spinner />
      </div>
    );
  }
  const b = blob.data;

  const onCommit = async (req: CommitRequest) => {
    const branch = await prepareTarget(owner, repoName, req, b.commit);
    try {
      const result = await removeFile(owner, repoName, branch, t.path, b.sha, req.fullMessage);
      // Deleting the last file of a directory removes the directory too.
      const dest = parent ? await getTree(owner, repoName, result.branch, parent).then(() => parent, () => '') : '';
      finishCommit(owner, repoName, req, result, treeUrl(repoRefOf(t), result.branch, dest));
    } catch (e) {
      const err = classifyError(e);
      if (err.kind === 'conflict') throw new CommitError(`${t.path} has changed since you opened it. Reload the page to see the latest version.`, 'conflict');
      throw err;
    }
  };

  return (
    <>
      <div className={styles.header}>
        <div className={styles.deleteTitle}>
          <TrashIcon size={16} />
          <span>
            Delete <code className={styles.mono}>{t.path}</code> in <code className={styles.branchPill}>{t.ref}</code>
          </span>
        </div>
        <div className={styles.headerActions}>
          <Button onClick={() => navigate(blobUrl(repoRefOf(t), t.ref, t.path))}>Cancel</Button>
          <Button variant="danger" leadingIcon={TrashIcon} disabled={!t.canPush} onClick={() => setDialog(true)}>
            Commit changes…
          </Button>
        </div>
      </div>
      {!t.canPush && <NoPushNotice repo={`${owner}/${repoName}`} />}
      <div className={styles.deleteBox}>
        <FileIcon size={20} className={styles.deleteIcon} />
        <div>
          <div className={styles.deleteName}>{fileName}</div>
          <div className={styles.muted}>{summary}</div>
        </div>
        <Button size="sm" leadingIcon={EyeIcon} className={styles.deleteView} onClick={() => navigate(blobUrl(repoRefOf(t), t.ref, t.path))}>
          View file
        </Button>
      </div>
      <p className={styles.muted}>This file will be removed from the {t.isBranch ? 'branch' : 'new branch'} when you commit. You can restore it later from the commit history.</p>
      <CommitDialog
        open={dialog}
        onClose={() => setDialog(false)}
        owner={owner}
        repo={repoName}
        branch={t.ref}
        defaultMessage={`Delete ${fileName}`}
        canCommitDirectly={t.isBranch}
        directBlockedReason={directBlockedReason(t)}
        title="Delete file"
        onCommit={onCommit}
      />
    </>
  );
});

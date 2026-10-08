import { observer } from 'mobx-react-lite';
import { useRef, useState, type DragEvent } from 'react';
import { CommitDialog, type CommitRequest } from '../../../components/code/CommitDialog';
import { navigate } from '../../../router';
import { treeUrl } from '../../../components/code/urls';
import { useShortcuts } from '../../../shortcuts/useShortcuts';
import { Button, IconButton, cx } from '../../../ui/Button';
import { AlertIcon, FileIcon, GitCommitIcon, UploadIcon, XIcon } from '../../../ui/icons';
import { MAX_FILE_BYTES, commitFiles, finishCommit, formatBytes, joinPath, pathProblem, prepareTarget, type FileChange } from './commit';
import { NoPushNotice, Notice, directBlockedReason, useEditTarget, useUnloadGuard } from './shared';
import styles from './Edit.module.css';

const MAX_FILES = 100;

interface Staged {
  /** Path relative to the target directory. */
  rel: string;
  file: File;
  error: string | null;
}

/** Recursively read a dropped directory (Chrome/Firefox/Safari `webkitGetAsEntry`). */
async function readEntry(entry: FileSystemEntry, prefix: string, out: { rel: string; file: File }[]): Promise<void> {
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) => (entry as FileSystemFileEntry).file(resolve, reject));
    out.push({ rel: prefix + entry.name, file });
    return;
  }
  if (!entry.isDirectory) return;
  const reader = (entry as FileSystemDirectoryEntry).createReader();
  for (;;) {
    const batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject));
    if (!batch.length) break;
    for (const e of batch) await readEntry(e, `${prefix}${entry.name}/`, out);
  }
}

/** `/:owner/:repo/upload/:ref/*` — upload files into a directory as one commit. */
export default observer(function UploadPage() {
  const t = useEditTarget();
  const { owner, name: repoName } = t;
  const dir = t.path;
  const [staged, setStaged] = useState<Staged[]>([]);
  const [over, setOver] = useState(false);
  const [dialog, setDialog] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number; label: string } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const folderInput = useRef<HTMLInputElement>(null);
  const valid = staged.filter((s) => !s.error);
  useUnloadGuard(staged.length > 0);

  const add = (items: { rel: string; file: File }[]) => {
    setNotice(null);
    setStaged((prev) => {
      const byPath = new Map(prev.map((s) => [s.rel, s]));
      for (const { rel, file } of items) {
        const clean = joinPath(rel);
        const error =
          file.size > MAX_FILE_BYTES
            ? `Yowza, that’s a big file. Try again with a file smaller than 25 MB (this one is ${formatBytes(file.size)}).`
            : pathProblem(joinPath(dir, clean));
        byPath.set(clean, { rel: clean, file, error });
      }
      let next = [...byPath.values()];
      if (next.length > MAX_FILES) {
        setNotice(`You can upload up to ${MAX_FILES} files at a time. Only the first ${MAX_FILES} were kept.`);
        next = next.slice(0, MAX_FILES);
      }
      return next;
    });
  };

  const onDrop = async (e: DragEvent) => {
    e.preventDefault();
    setOver(false);
    if (!t.canPush) return;
    const items = [...e.dataTransfer.items];
    const entries = items.map((i) => (i.kind === 'file' ? i.webkitGetAsEntry?.() : null));
    if (entries.some((x) => x?.isDirectory)) {
      const out: { rel: string; file: File }[] = [];
      for (const entry of entries) if (entry) await readEntry(entry, '', out);
      add(out);
    } else {
      add([...e.dataTransfer.files].map((file) => ({ rel: file.name, file })));
    }
  };

  const onPick = (files: FileList | null) => {
    if (!files) return;
    add([...files].map((file) => ({ rel: file.webkitRelativePath || file.name, file })));
  };

  const onCommit = async (req: CommitRequest) => {
    const branch = await prepareTarget(owner, repoName, req);
    setProgress({ done: 0, total: valid.length, label: 'Reading files' });
    try {
      const changes: FileChange[] = [];
      for (const s of valid) changes.push({ path: joinPath(dir, s.rel), content: new Uint8Array(await s.file.arrayBuffer()) });
      const result = await commitFiles(owner, repoName, branch, changes, req.fullMessage, {
        onProgress: (done, total, label) => setProgress({ done, total, label }),
      });
      setStaged([]);
      finishCommit(owner, repoName, req, result, treeUrl({ owner, repo: repoName }, result.branch, dir));
    } finally {
      setProgress(null);
    }
  };

  const openDialog = () => {
    if (t.canPush && valid.length) setDialog(true);
  };
  useShortcuts('Upload', {
    'mod+enter': { handler: openDialog, description: 'Commit changes', group: 'Editor' },
  });

  const totalBytes = valid.reduce((n, s) => n + s.file.size, 0);
  const folderProps = { webkitdirectory: '', directory: '' } as Record<string, string>;

  return (
    <div className={styles.page}>
      <div className={styles.header}>
        <div className={styles.pathInput}>
          <span className={styles.pathRepo}>{repoName}</span>
          {(dir ? dir.split('/') : []).map((p, i) => (
            <span key={i} className={styles.pathSeg}>
              <span className={styles.pathSep}>/</span>
              <span className={styles.pathDir}>{p}</span>
            </span>
          ))}
          <span className={styles.pathSep}>/</span>
          <span className={styles.pathIn}>Upload files to</span>
          <code className={styles.branchPill}>{t.ref}</code>
        </div>
        <div className={styles.headerActions}>
          <Button
            onClick={() => {
              if (staged.length && !window.confirm('Discard the selected files?')) return;
              navigate(treeUrl({ owner, repo: repoName }, t.ref, dir));
            }}
          >
            Cancel
          </Button>
          <Button variant="primary" leadingIcon={GitCommitIcon} disabled={!t.canPush || !valid.length} onClick={openDialog}>
            Commit changes…
          </Button>
        </div>
      </div>
      {!t.canPush && <NoPushNotice repo={`${owner}/${repoName}`} />}
      {notice && <Notice kind="warning">{notice}</Notice>}
      <div
        className={cx(styles.dropzone, over && styles.dropzoneOver, !t.canPush && styles.dropzoneDisabled)}
        onDragEnter={(e) => {
          e.preventDefault();
          setOver(true);
        }}
        onDragOver={(e) => {
          e.preventDefault();
          e.dataTransfer.dropEffect = t.canPush ? 'copy' : 'none';
        }}
        onDragLeave={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setOver(false);
        }}
        onDrop={(e) => void onDrop(e)}
      >
        <UploadIcon size={32} className={styles.dropIcon} />
        <div className={styles.dropTitle}>Drag files here to add them to your repository</div>
        <div className={styles.muted}>
          Or{' '}
          <button type="button" className={styles.linkButton} disabled={!t.canPush} onClick={() => input.current?.click()}>
            choose your files
          </button>{' '}
          ·{' '}
          <button type="button" className={styles.linkButton} disabled={!t.canPush} onClick={() => folderInput.current?.click()}>
            choose a folder
          </button>
        </div>
        <input
          ref={input}
          type="file"
          multiple
          hidden
          onChange={(e) => {
            onPick(e.target.files);
            e.target.value = '';
          }}
        />
        <input
          ref={folderInput}
          type="file"
          multiple
          hidden
          {...folderProps}
          onChange={(e) => {
            onPick(e.target.files);
            e.target.value = '';
          }}
        />
      </div>
      {staged.length > 0 && (
        <div className={styles.fileList}>
          <div className={styles.fileListHeader}>
            <span>
              {valid.length} file{valid.length === 1 ? '' : 's'} · {formatBytes(totalBytes)}
            </span>
            <Button size="sm" variant="ghost" onClick={() => setStaged([])}>
              Clear all
            </Button>
          </div>
          {staged.map((s) => (
            <div key={s.rel} className={cx(styles.fileRow, s.error && styles.fileRowError)}>
              {s.error ? <AlertIcon size={16} className={styles.fileErrorIcon} /> : <FileIcon size={16} className={styles.muted} />}
              <div className={styles.fileMain}>
                <span className={styles.mono}>{s.rel}</span>
                {s.error && <span className={styles.fileError}>{s.error}</span>}
              </div>
              <span className={styles.muted}>{formatBytes(s.file.size)}</span>
              <IconButton icon={XIcon} label={`Remove ${s.rel}`} size="sm" onClick={() => setStaged((prev) => prev.filter((x) => x.rel !== s.rel))} />
            </div>
          ))}
        </div>
      )}
      <CommitDialog
        open={dialog}
        onClose={() => !progress && setDialog(false)}
        owner={owner}
        repo={repoName}
        branch={t.ref}
        defaultMessage="Add files via upload"
        canCommitDirectly={t.isBranch}
        directBlockedReason={directBlockedReason(t)}
        title={`Commit ${valid.length} file${valid.length === 1 ? '' : 's'}`}
        onCommit={onCommit}
      >
        {progress && (
          <div className={styles.progress} role="status">
            <div className={styles.progressLabel}>
              <span>
                Uploading {Math.min(progress.done + 1, progress.total)} of {progress.total}
              </span>
              <span className={cx(styles.muted, styles.ellipsis)}>{progress.label}</span>
            </div>
            <div className={styles.progressTrack}>
              <div className={styles.progressBar} style={{ width: `${progress.total ? (progress.done / progress.total) * 100 : 0}%` }} />
            </div>
          </div>
        )}
      </CommitDialog>
    </div>
  );
});

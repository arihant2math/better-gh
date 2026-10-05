import { observer } from 'mobx-react-lite';
import { lazy, Suspense, useRef, useState } from 'react';
import { fetchRaw } from '../../api/code';
import type { BlobView } from '../../api/types';
import { Link, navigate, useQuery, setQuery } from '../../router';
import type { Repo } from '../../sync/models';
import { Button, IconButton, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, CopyIcon, DownloadIcon, KebabHorizontalIcon, LinkIcon, PencilIcon, TrashIcon, HistoryIcon } from '../../ui/icons';
import { Menu } from '../../ui/Menu';
import { toast } from '../../ui/Toast';
import { BlameBody } from './Blame';
import styles from './Code.module.css';
import { CodeLines } from './CodeLines';
import { prefetchBlame, useBlob } from './data';
import { LastCommitBar } from './LastCommitBar';
import { codeUrl, copyText, formatSize, lineHash, parseLineHash, renderModes, routeLinks, setHash, useHash, type CodeTarget, type LineRange, type RenderMode } from './util';

const CsvTable = lazy(() => import('./CsvTable'));

/** Single file: header, render-mode switch, code / rendered view or blame. */
export const FileView = observer(function FileView({ t, repo, blame, canPush }: { t: CodeTarget; repo: Repo; blame: boolean; canPush: boolean }) {
  const { data: blob, error } = useBlob(t);
  const query = useQuery();
  const hash = useHash();
  const selection = parseLineHash(hash);
  const anchor = useRef<LineRange | null>(null);

  if (error) {
    const status = (error as { status?: number }).status;
    return <EmptyState icon={AlertIcon} title={status === 404 ? 'File not found' : 'Could not load this file'}>{status === 404 ? `${t.path} does not exist at ${t.ref}.` : String((error as Error).message ?? error)}</EmptyState>;
  }

  const modes = blob ? renderModes(blob.path, blob.image) : ['code' as RenderMode];
  const plain = query.get('plain') === '1';
  const mode: RenderMode = blame ? 'code' : plain && modes.includes('code') ? 'code' : modes[0]!;
  const rendered = mode !== 'code';

  const onSelect = (_r: LineRange | null, line: number, extend: boolean) => {
    const cur = parseLineHash(window.location.hash);
    let next: LineRange;
    if (extend && cur) {
      const a = anchor.current?.start ?? cur.start;
      next = { start: Math.min(a, line), end: Math.max(a, line) };
    } else {
      next = { start: line, end: line };
      anchor.current = next;
    }
    setHash(lineHash(next));
  };

  return (
    <>
      <LastCommitBar t={t} path={t.path} />
      <div className={styles.file}>
        <div className={styles.fileHeader}>
          <div className={styles.segmented} role="tablist" aria-label="View">
            {modes.length > 1 && !blame && (
              <button type="button" role="tab" aria-selected={rendered} className={cx(styles.seg, rendered && styles.segOn)} onClick={() => setQuery({ plain: null })}>
                Preview
              </button>
            )}
            <button
              type="button"
              role="tab"
              aria-selected={!blame && !rendered}
              className={cx(styles.seg, !blame && !rendered && styles.segOn)}
              onClick={() => (blame ? navigate(codeUrl(t, 'blob', t.ref, t.path) + (modes.length > 1 ? '?plain=1' : '') + hash) : setQuery({ plain: modes.length > 1 ? '1' : null }))}
            >
              Code
            </button>
            {blob?.lines && (
              <Link
                to={codeUrl(t, 'blame', t.ref, t.path) + hash}
                role="tab"
                aria-selected={blame}
                className={cx(styles.seg, blame && styles.segOn)}
                onMouseEnter={() => prefetchBlame(t, t.path)}
              >
                Blame
              </Link>
            )}
          </div>
          <span className={styles.fileMeta}>{blob ? <FileMeta blob={blob} /> : <Skeleton width={140} />}</span>
          <span className={styles.grow} />
          {selection && blob?.lines && <SelectionActions t={t} blob={blob} range={selection} />}
          {blob && <FileActions t={t} blob={blob} canPush={canPush} repo={repo} />}
        </div>
        {!blob ? (
          <div className={styles.fileLoading}>
            {Array.from({ length: 12 }, (_, i) => (
              <Skeleton key={i} width={`${25 + ((i * 37) % 60)}%`} />
            ))}
          </div>
        ) : blame ? (
          <BlameBody t={t} blob={blob} selection={selection} onSelect={onSelect} />
        ) : (
          <FileBody blob={blob} mode={mode} selection={selection} onSelect={onSelect} />
        )}
      </div>
    </>
  );
});

function FileMeta({ blob }: { blob: BlobView }) {
  const parts: string[] = [];
  if (blob.lines) {
    const loc = blob.lines.filter((l) => l.replace(/<[^>]*>/g, '').trim() !== '').length;
    parts.push(`${blob.line_count} lines (${loc} loc)`);
  }
  parts.push(formatSize(blob.lfs?.size ?? blob.size));
  return (
    <>
      {parts.join(' · ')}
      {blob.language && <span className={styles.lang}>{blob.language}</span>}
      {blob.lfs && <span className={styles.lang}>Stored with Git LFS</span>}
    </>
  );
}

function SelectionActions({ t, blob, range }: { t: CodeTarget; blob: BlobView; range: LineRange }) {
  const label = range.start === range.end ? `Line ${range.start}` : `Lines ${range.start}–${range.end}`;
  const permalink = () => `${window.location.origin}${codeUrl(t, 'blob', blob.commit, t.path)}${lineHash(range)}`;
  return (
    <span className={styles.selectionBar}>
      <span className={styles.muted}>{label}</span>
      <IconButton
        icon={LinkIcon}
        label="Copy permalink"
        shortcut="y"
        size="sm"
        variant="ghost"
        onClick={() => void copyText(permalink()).then(() => toast({ title: 'Permalink copied', description: `${t.path}${lineHash(range)} @ ${blob.commit.slice(0, 7)}` }))}
      />
      <IconButton
        icon={CopyIcon}
        label="Copy lines"
        size="sm"
        variant="ghost"
        onClick={() =>
          void fetchRaw(blob.raw_url)
            .then((text) => copyText(text.split('\n').slice(range.start - 1, range.end).join('\n')))
            .then(() => toast({ title: `Copied ${label.toLowerCase()}` }))
        }
      />
    </span>
  );
}

function FileActions({ t, blob, canPush, repo }: { t: CodeTarget; blob: BlobView; canPush: boolean; repo: Repo }) {
  const more = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const textual = !!blob.lines && !blob.binary && !blob.lfs;
  const onBranch = t.kind === 'branch' || (t.kind === 'unknown' && t.ref === repo.defaultBranch);
  return (
    <span className={styles.fileActions}>
      <Button size="sm" onClick={() => window.open(blob.raw_url, '_blank', 'noopener')}>
        Raw
      </Button>
      {textual && (
        <IconButton
          icon={CopyIcon}
          label="Copy raw file"
          size="sm"
          onClick={() => void fetchRaw(blob.raw_url).then(copyText).then(() => toast({ title: 'File copied to clipboard' }), () => toast({ title: 'Copy failed', kind: 'error' }))}
        />
      )}
      <a className={styles.iconLink} href={blob.raw_url} download={blob.name} aria-label="Download raw file" title="Download raw file">
        <DownloadIcon size={16} />
      </a>
      {canPush && textual && onBranch && (
        <IconButton icon={PencilIcon} label="Edit this file" shortcut="." size="sm" onClick={() => navigate(codeUrl(t, 'edit', t.ref, t.path))} />
      )}
      <IconButton ref={more} icon={KebabHorizontalIcon} label="More file actions" size="sm" onClick={() => setOpen((o) => !o)} />
      <Menu
        open={open}
        onClose={() => setOpen(false)}
        anchor={more}
        placement="bottom-end"
        aria-label="More file actions"
        items={[
          {
            id: 'permalink',
            label: 'Copy permalink',
            icon: LinkIcon,
            onSelect: () => void copyText(`${window.location.origin}${codeUrl(t, 'blob', blob.commit, t.path)}${window.location.hash}`).then(() => toast({ title: 'Permalink copied' })),
          },
          { id: 'path', label: 'Copy path', icon: CopyIcon, onSelect: () => void copyText(t.path).then(() => toast({ title: 'Path copied' })) },
          { id: 'history', label: 'View history', icon: HistoryIcon, onSelect: () => navigate(`/${t.owner}/${t.repo}/commits/${t.ref}/${t.path}`) },
          ...(canPush && onBranch
            ? [
                { separator: true as const, id: 'sep' },
                { id: 'delete', label: 'Delete file', icon: TrashIcon, danger: true, onSelect: () => navigate(codeUrl(t, 'delete', t.ref, t.path)) },
              ]
            : []),
        ]}
      />
    </span>
  );
}

function Notice({ children }: { children: React.ReactNode }) {
  return <div className={styles.notice}>{children}</div>;
}

function FileBody({ blob, mode, selection, onSelect }: { blob: BlobView; mode: RenderMode; selection: LineRange | null; onSelect: (r: LineRange | null, line: number, extend: boolean) => void }) {
  if (blob.type === 'submodule') return <Notice>Submodule at commit {blob.sha.slice(0, 7)}.</Notice>;
  if (blob.symlink_target !== null) return <Notice>Symbolic link to {blob.symlink_target}</Notice>;
  if (mode === 'image' || mode === 'svg') {
    return (
      <div className={styles.image}>
        <img src={blob.raw_url} alt={blob.name} />
      </div>
    );
  }
  if (mode === 'pdf') {
    return (
      <div className={styles.pdf}>
        <object data={blob.raw_url} type="application/pdf" aria-label={blob.name}>
          <Notice>
            PDF preview is not available in this browser. <a href={blob.raw_url}>Download</a>
          </Notice>
        </object>
      </div>
    );
  }
  if (blob.lfs) {
    return (
      <Notice>
        Stored with Git LFS ({formatSize(blob.lfs.size)}). {blob.lfs.stored ? <a href={blob.raw_url}>Download</a> : 'The object has not been uploaded.'}
      </Notice>
    );
  }
  if (blob.too_large) {
    return (
      <Notice>
        This file is too large to display ({formatSize(blob.size)}). <a href={blob.raw_url}>View raw</a>
      </Notice>
    );
  }
  if (blob.binary || !blob.lines) {
    return (
      <Notice>
        Binary file not shown. <a href={blob.raw_url}>Download</a>
      </Notice>
    );
  }
  if (mode === 'markdown' && blob.rendered !== null) {
    return <div className={cx('markdown-body', styles.readmeBody)} onClick={routeLinks} dangerouslySetInnerHTML={{ __html: blob.rendered }} />;
  }
  if (mode === 'csv') {
    return (
      <Suspense fallback={<div className={styles.fileLoading}><Skeleton width="60%" /></div>}>
        <CsvTable blob={blob} />
      </Suspense>
    );
  }
  if (blob.line_count === 0) return <Notice>This file is empty.</Notice>;
  return (
    <div className={styles.codeScroll}>
      <CodeLines lines={blob.lines} selection={selection} onSelect={onSelect} />
      {blob.truncated && (
        <Notice>
          Only the first part of this file is shown. <a href={blob.raw_url}>View raw</a>
        </Notice>
      )}
    </div>
  );
}

import { useLayoutEffect, useRef, useState, type ClipboardEvent, type DragEvent, type RefObject } from 'react';
import { uploadAttachment } from '../../api/uploads';
import { repoByName } from '../../sync/selectors';
import { IconButton } from '../../ui/Button';
import { PaperclipIcon } from '../../ui/icons';
import { toast } from '../../ui/Toast';
import { attachFiles, filesOf } from './attachments';
import type { Edit } from './format';

export interface Attachments {
  onPaste(e: ClipboardEvent): void;
  onDrop(e: DragEvent): void;
  onDragOver(e: DragEvent): void;
  onDragLeave(): void;
  /** Files are being dragged over the textarea. */
  dragging: boolean;
  /** Overall progress (0..1) while uploads run, else null. */
  progress: number | null;
  /** Number of uploads in flight. */
  pending: number;
  /** "Attach files" button + hidden file input. */
  button: React.ReactNode;
}

/**
 * Paste / drop / "Attach files" uploads for a markdown textarea
 * (POST /_bgh/uploads). `repo` is `owner/name`: private repositories gate
 * access to their attachments. `apply` writes an edit (value + selection);
 * defaults to `onChange` plus restoring the selection.
 */
export function useAttachments({
  textarea,
  value,
  onChange,
  repo,
  apply,
}: {
  textarea: RefObject<HTMLTextAreaElement | null>;
  value: string;
  onChange: (v: string) => void;
  repo?: string;
  apply?: (e: Edit) => void;
}): Attachments {
  const latest = useRef(value);
  latest.current = value;
  const pendingSel = useRef<[number, number] | null>(null);
  const progress = useRef(new Map<File, number>());
  const [, setTick] = useState(0);
  const [dragging, setDragging] = useState(false);
  const input = useRef<HTMLInputElement | null>(null);

  useLayoutEffect(() => {
    const el = textarea.current;
    if (!apply && el && pendingSel.current && document.activeElement === el) el.setSelectionRange(...pendingSel.current);
    pendingSel.current = null;
  });

  const write = (e: Edit) => {
    latest.current = e.value;
    if (apply) apply(e);
    else {
      pendingSel.current = [e.selStart, e.selEnd];
      onChange(e.value);
    }
  };
  const read = (): Edit => {
    const el = textarea.current;
    const v = latest.current;
    const start = el && el.value === v ? el.selectionStart : v.length;
    const end = el && el.value === v ? el.selectionEnd : v.length;
    return { value: v, selStart: start, selEnd: end };
  };

  const upload = (files: File[]) => {
    if (!files.length) return;
    const [owner, name] = (repo ?? '').split('/');
    const repositoryId = owner && name ? repoByName(owner, name)?.id : undefined;
    const target = repositoryId != null && repositoryId > 0 ? { repositoryId } : { repository: repo };
    for (const f of files) progress.current.set(f, 0);
    setTick((t) => t + 1);
    void attachFiles(files, {
      read,
      write,
      upload: (file, onProgress) => uploadAttachment(file, target, onProgress),
      onProgress: (file, p) => {
        progress.current.set(file, p);
        setTick((t) => t + 1);
      },
      onError: (file, err) => toast({ kind: 'error', title: `Failed to upload ${file.name}`, description: err.message }),
    }).finally(() => {
      for (const f of files) progress.current.delete(f);
      setTick((t) => t + 1);
    });
  };

  const sizes = [...progress.current.keys()].reduce((n, f) => n + Math.max(f.size, 1), 0);
  const done = [...progress.current.entries()].reduce((n, [f, p]) => n + Math.max(f.size, 1) * p, 0);
  const pending = progress.current.size;

  return {
    onPaste: (e) => {
      const files = filesOf(e.clipboardData.files);
      if (!files.length) return;
      e.preventDefault();
      upload(files);
    },
    onDrop: (e) => {
      setDragging(false);
      const files = filesOf(e.dataTransfer.files);
      if (!files.length) return;
      e.preventDefault();
      textarea.current?.focus();
      upload(files);
    },
    onDragOver: (e) => {
      if (!e.dataTransfer.types.includes('Files')) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = 'copy';
      if (!dragging) setDragging(true);
    },
    onDragLeave: () => setDragging(false),
    dragging,
    progress: pending ? done / sizes : null,
    pending,
    button: (
      <>
        <IconButton
          icon={PaperclipIcon}
          size="sm"
          label="Attach files"
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => input.current?.click()}
        />
        <input
          ref={input}
          type="file"
          multiple
          hidden
          aria-label="Attach files"
          onChange={(e) => {
            const files = filesOf(e.target.files);
            e.target.value = '';
            upload(files);
          }}
        />
      </>
    ),
  };
}

/** "Uploading 2 files… 40%" line under an editor. */
export function UploadStatus({ a, className }: { a: Attachments; className?: string }) {
  if (a.progress == null) return null;
  return (
    <span className={className} role="status">
      Uploading {a.pending === 1 ? '1 file' : `${a.pending} files`}… {Math.round(a.progress * 100)}%
    </span>
  );
}

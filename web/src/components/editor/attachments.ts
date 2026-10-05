/**
 * Attachment uploads into a markdown textarea (paste, drop, "Attach
 * files"): a `![Uploading name…]()` placeholder goes in at the caret and is
 * replaced by the server's markdown once the upload finishes, or removed
 * when it fails. Pure logic, independent of React (see useAttachments).
 */
import type { Attachment } from '../../api/uploads';
import type { Edit } from './format';

export function placeholderFor(name: string): string {
  return `![Uploading ${name.replace(/[[\]]/g, '')}…]()`;
}

/** Insert `text` at the selection on its own line(s). */
export function insertBlock(e: Edit, text: string): Edit {
  const before = e.value.slice(0, e.selStart);
  const after = e.value.slice(e.selEnd);
  const pre = before && !before.endsWith('\n') ? '\n' : '';
  const post = after && !after.startsWith('\n') ? '\n' : '';
  const value = before + pre + text + post + after;
  const at = before.length + pre.length + text.length;
  return { value, selStart: at, selEnd: at };
}

/** Replace the first occurrence of `placeholder` (unchanged if gone), keeping the selection in place. */
export function replacePlaceholder(e: Edit, placeholder: string, replacement: string): Edit {
  const i = e.value.indexOf(placeholder);
  if (i < 0) return e;
  return splice(e, i, i + placeholder.length, replacement);
}

/** Remove `placeholder` and the line break it was inserted with. */
export function removePlaceholder(e: Edit, placeholder: string): Edit {
  const i = e.value.indexOf(placeholder);
  if (i < 0) return e;
  let start = i;
  let end = i + placeholder.length;
  if (e.value[end] === '\n') end++;
  else if (start > 0 && e.value[start - 1] === '\n') start--;
  return splice(e, start, end, '');
}

/** Replace `[start, end)` with `text`, shifting a selection that lies after it. */
function splice(e: Edit, start: number, end: number, text: string): Edit {
  const delta = text.length - (end - start);
  const move = (p: number) => (p >= end ? p + delta : p > start ? start + text.length : p);
  return { value: e.value.slice(0, start) + text + e.value.slice(end), selStart: move(e.selStart), selEnd: move(e.selEnd) };
}

export interface AttachDeps {
  /** Current editor state (read at every step: the user keeps typing). */
  read(): Edit;
  write(e: Edit): void;
  upload(file: File, onProgress: (fraction: number) => void): Promise<Attachment>;
  onProgress?(file: File, fraction: number): void;
  onError?(file: File, error: Error): void;
}

/**
 * Upload `files` into the editor. Resolves when every upload settled;
 * returns how many succeeded.
 */
export async function attachFiles(files: File[], deps: AttachDeps): Promise<number> {
  if (!files.length) return 0;
  const placeholders = files.map((f) => placeholderFor(f.name));
  deps.write(insertBlock(deps.read(), placeholders.join('\n')));
  const results = await Promise.all(
    files.map(async (file, i) => {
      const ph = placeholders[i]!;
      try {
        const a = await deps.upload(file, (p) => deps.onProgress?.(file, p));
        const cur = deps.read();
        deps.write(replacePlaceholder(cur, ph, a.markdown));
        return true;
      } catch (err) {
        const cur = deps.read();
        deps.write(removePlaceholder(cur, ph));
        deps.onError?.(file, err instanceof Error ? err : new Error(String(err)));
        return false;
      }
    }),
  );
  return results.filter(Boolean).length;
}

/** Files from a paste or drop (ignores plain-text pastes). */
export function filesOf(list: FileList | null | undefined): File[] {
  return list ? Array.from(list).filter((f) => f.size > 0 || f.type) : [];
}

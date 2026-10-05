/**
 * Task-list editing: ticking the nth rendered checkbox rewrites the nth
 * task item marker (`- [ ]` ↔ `- [x]`) in the Markdown source, the way
 * GitHub does for users who can edit the comment.
 */

const FENCE = /^ {0,3}(`{3,}|~{3,})/;
/** List item marker (optionally inside blockquotes) followed by `[ ]` / `[x]`. */
const TASK = /^((?:[ \t]*>)*[ \t]*(?:[-*+]|\d{1,9}[.)])[ \t]+\[)([ xX])(\](?:[ \t]|$))/;

/** Offsets of the `[ ]`/`[x]` state characters, in document order. */
function taskOffsets(source: string): number[] {
  const out: number[] = [];
  let fence: string | null = null;
  let offset = 0;
  for (const line of source.split('\n')) {
    const f = FENCE.exec(line.replace(/^(?:[ \t]*>)*[ \t]?/, ''));
    if (fence) {
      if (f && f[1]![0] === fence[0] && f[1]!.length >= fence.length) fence = null;
    } else if (f) {
      fence = f[1]!;
    } else {
      const m = TASK.exec(line);
      if (m) out.push(offset + m[1]!.length);
    }
    offset += line.length + 1;
  }
  return out;
}

/** Number of task items in `source`. */
export function countTasks(source: string): number {
  return taskOffsets(source).length;
}

/**
 * `source` with task `index` set to `checked`, or `null` when there is no
 * such task item.
 */
export function setTask(source: string, index: number, checked: boolean): string | null {
  const at = taskOffsets(source)[index];
  if (at === undefined) return null;
  return source.slice(0, at) + (checked ? 'x' : ' ') + source.slice(at + 1);
}

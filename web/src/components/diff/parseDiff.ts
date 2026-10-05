/** Unified diff (git format) → structured files/hunks/lines. */

export interface DiffLine {
  type: 'add' | 'del' | 'ctx' | 'meta';
  text: string;
  oldNo?: number;
  newNo?: number;
}

export interface DiffHunk {
  header: string;
  oldStart: number;
  oldLines: number;
  newStart: number;
  newLines: number;
  lines: DiffLine[];
}

export interface DiffFile {
  oldPath: string;
  newPath: string;
  /** Display path (new path, or old path for deletions). */
  path: string;
  status: 'added' | 'deleted' | 'modified' | 'renamed';
  binary: boolean;
  additions: number;
  deletions: number;
  hunks: DiffHunk[];
}

const HUNK_RE = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$/;

function stripPrefix(p: string): string {
  if (p === '/dev/null') return p;
  return p.replace(/^[ab]\//, '');
}

export function parseDiff(text: string): DiffFile[] {
  const files: DiffFile[] = [];
  let file: DiffFile | null = null;
  let hunk: DiffHunk | null = null;
  let oldNo = 0;
  let newNo = 0;

  const lines = text.split('\n');
  if (lines[lines.length - 1] === '') lines.pop();

  for (const line of lines) {
    if (line.startsWith('diff --git ')) {
      const m = /^diff --git a\/(.+) b\/(.+)$/.exec(line);
      file = {
        oldPath: m?.[1] ?? '',
        newPath: m?.[2] ?? '',
        path: m?.[2] ?? '',
        status: 'modified',
        binary: false,
        additions: 0,
        deletions: 0,
        hunks: [],
      };
      files.push(file);
      hunk = null;
      continue;
    }
    if (!file) continue;
    if (!hunk) {
      if (line.startsWith('new file mode')) file.status = 'added';
      else if (line.startsWith('deleted file mode')) file.status = 'deleted';
      else if (line.startsWith('rename from ')) {
        file.status = 'renamed';
        file.oldPath = line.slice(12);
      } else if (line.startsWith('rename to ')) file.newPath = file.path = line.slice(10);
      else if (line.startsWith('Binary files')) file.binary = true;
      else if (line.startsWith('--- ')) {
        const p = stripPrefix(line.slice(4));
        if (p === '/dev/null') file.status = 'added';
        else file.oldPath = p;
      } else if (line.startsWith('+++ ')) {
        const p = stripPrefix(line.slice(4));
        if (p === '/dev/null') {
          file.status = 'deleted';
          file.path = file.oldPath;
        } else file.newPath = file.path = p;
      }
    }
    const hm = HUNK_RE.exec(line);
    if (hm) {
      hunk = {
        header: line,
        oldStart: Number(hm[1]),
        oldLines: hm[2] === undefined ? 1 : Number(hm[2]),
        newStart: Number(hm[3]),
        newLines: hm[4] === undefined ? 1 : Number(hm[4]),
        lines: [],
      };
      oldNo = hunk.oldStart;
      newNo = hunk.newStart;
      file.hunks.push(hunk);
      continue;
    }
    if (!hunk) continue;
    const c = line[0];
    const body = line.slice(1);
    if (c === '+') {
      hunk.lines.push({ type: 'add', text: body, newNo: newNo++ });
      file.additions++;
    } else if (c === '-') {
      hunk.lines.push({ type: 'del', text: body, oldNo: oldNo++ });
      file.deletions++;
    } else if (c === '\\') {
      hunk.lines.push({ type: 'meta', text: line });
    } else {
      hunk.lines.push({ type: 'ctx', text: c === ' ' ? body : line, oldNo: oldNo++, newNo: newNo++ });
    }
  }
  return files;
}

/** Parse GitHub's per-file `patch` (hunks only, from the first `@@`). */
export function parsePatch(patch: string): DiffHunk[] {
  const file = parseDiff(`diff --git a/x b/x\n--- a/x\n+++ b/x\n${patch}\n`)[0];
  return file?.hunks ?? [];
}

export type SplitCell = { line: DiffLine } | null;

/** Pair a hunk's lines for side-by-side view: context on both sides, del/add blocks zipped. */
export function splitHunk(hunk: DiffHunk): { left: SplitCell; right: SplitCell }[] {
  const out: { left: SplitCell; right: SplitCell }[] = [];
  const lines = hunk.lines;
  let i = 0;
  while (i < lines.length) {
    const l = lines[i]!;
    if (l.type === 'ctx') {
      out.push({ left: { line: l }, right: { line: l } });
      i++;
    } else if (l.type === 'meta') {
      i++;
    } else {
      const dels: DiffLine[] = [];
      const adds: DiffLine[] = [];
      while (i < lines.length && lines[i]!.type === 'del') dels.push(lines[i++]!);
      while (i < lines.length && (lines[i]!.type === 'add' || lines[i]!.type === 'meta')) {
        if (lines[i]!.type === 'add') adds.push(lines[i]!);
        i++;
      }
      for (let k = 0; k < Math.max(dels.length, adds.length); k++) {
        out.push({ left: dels[k] ? { line: dels[k]! } : null, right: adds[k] ? { line: adds[k]! } : null });
      }
    }
  }
  return out;
}

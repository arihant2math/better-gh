import { describe, expect, it } from 'vitest';
import { parseDiff } from './parseDiff';

const DIFF = `diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@
 use std::io;
-fn old() {}
+fn new() {}
+fn extra() {}

 fn main() {}
@@ -10 +11 @@ impl Foo {
-    a
+    b
\\ No newline at end of file
diff --git a/docs/new.md b/docs/new.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1,2 @@
+# Title
+text
diff --git a/old.txt b/old.txt
deleted file mode 100644
--- a/old.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
`;

describe('parseDiff', () => {
  const files = parseDiff(DIFF);

  it('splits files and detects status', () => {
    expect(files.map((f) => [f.path, f.status])).toEqual([
      ['src/lib.rs', 'modified'],
      ['docs/new.md', 'added'],
      ['old.txt', 'deleted'],
    ]);
  });

  it('counts additions/deletions and numbers lines', () => {
    const f = files[0]!;
    expect([f.additions, f.deletions]).toEqual([3, 2]);
    expect(f.hunks).toHaveLength(2);
    const h = f.hunks[0]!;
    expect(h.lines.map((l) => [l.type, l.oldNo, l.newNo])).toEqual([
      ['ctx', 1, 1],
      ['del', 2, undefined],
      ['add', undefined, 2],
      ['add', undefined, 3],
      ['ctx', 3, 4],
      ['ctx', 4, 5],
    ]);
    expect(f.hunks[1]!.oldLines).toBe(1);
    expect(f.hunks[1]!.lines.at(-1)!.type).toBe('meta');
  });
});

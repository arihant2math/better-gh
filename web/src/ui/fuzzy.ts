/**
 * Tiny fuzzy matcher (subsequence with bonuses). Returns 0 for no match,
 * higher is better. Fast enough to score ~50k short strings per keystroke.
 */
export function fuzzyScore(query: string, text: string): number {
  if (!query) return 1;
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  const direct = t.indexOf(q);
  if (direct >= 0) {
    // Contiguous match: prefer prefix and word-start matches, shorter texts.
    const wordStart = direct === 0 || /[\s/_\-.#]/.test(t[direct - 1]!);
    return 1000 + (direct === 0 ? 400 : 0) + (wordStart ? 200 : 0) - Math.min(t.length, 200);
  }
  let score = 0;
  let ti = 0;
  let streak = 0;
  for (let qi = 0; qi < q.length; qi++) {
    const c = q[qi]!;
    if (c === ' ') continue;
    const found = t.indexOf(c, ti);
    if (found < 0) return 0;
    const atWord = found === 0 || /[\s/_\-.#]/.test(t[found - 1]!);
    streak = found === ti ? streak + 1 : 0;
    score += 10 + streak * 6 + (atWord ? 12 : 0) - Math.min(found - ti, 10);
    ti = found + 1;
  }
  return Math.max(1, score - Math.min(t.length, 100) / 4);
}

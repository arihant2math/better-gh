import { describe, expect, it } from 'vitest';
import { applySuggestion, splitQuery, suggest, tokenAt, type ValueSource } from './qualifiers';

const source: ValueSource = {
  values(kind, prefix) {
    const all: Record<string, string[]> = { user: ['grace', 'guido', 'ada'], label: ['bug', 'good first issue'], repo: ['acme/api', 'acme/web'] };
    return (all[kind] ?? []).filter((v) => v.startsWith(prefix)).map((value) => ({ value }));
  },
};

describe('tokenAt', () => {
  it('finds the token under the caret', () => {
    expect(tokenAt('is:open lab', 11)).toMatchObject({ start: 8, end: 11, text: 'lab' });
    expect(tokenAt('is:open label:bu', 16)).toMatchObject({ key: 'label', value: 'bu', negated: false });
    expect(tokenAt('-label:"good fi', 15)).toMatchObject({ start: 0, key: 'label', value: 'good fi', negated: true });
  });
});

describe('suggest', () => {
  it('offers qualifier names for a bare word', () => {
    const s = suggest('issues', 'fix la', 6, source);
    expect(s.map((x) => x.label)).toContain('label:');
    expect(s.every((x) => x.kind === 'qualifier')).toBe(true);
  });

  it('offers values once the key is typed, incl. @me for users', () => {
    expect(suggest('issues', 'author:g', 8, source).map((x) => x.label)).toEqual(['grace', 'guido']);
    expect(suggest('issues', 'author:', 7, source)[0]!.label).toBe('@me');
    expect(suggest('issues', 'is:o', 4, source).map((x) => x.label)).toEqual(['open']);
  });

  it('quotes values with spaces and keeps negation', () => {
    const [s] = suggest('issue-list', '-label:goo', 10, source);
    expect(s!.insert).toBe('-label:"good first issue"');
  });

  it('only offers negatable qualifiers after "-"', () => {
    const keys = suggest('issues', '-', 1, source, 50).map((x) => x.label);
    expect(keys).toContain('-label:');
    expect(keys).not.toContain('-is:');
  });

  it('does not hijack free text', () => {
    expect(suggest('code', '"conn pool', 10, source)).toEqual([]);
  });
});

describe('applySuggestion', () => {
  it('replaces the token and adds a space after a completed value', () => {
    const [s] = suggest('issues', 'is:open author:gr', 17, source);
    expect(applySuggestion('is:open author:gr', 17, s!)).toEqual({ value: 'is:open author:grace ', caret: 21 });
  });

  it('completes a qualifier name without a trailing space', () => {
    const s = suggest('issues', 'lab', 3, source).find((x) => x.label === 'label:')!;
    expect(applySuggestion('lab', 3, s)).toEqual({ value: 'label:', caret: 6 });
  });

  it('edits a token in the middle of the query', () => {
    const [s] = suggest('issues', 'is:o bug', 4, source);
    expect(applySuggestion('is:o bug', 4, s!)).toEqual({ value: 'is:open bug', caret: 8 });
  });
});

describe('splitQuery', () => {
  it('separates qualifiers from text', () => {
    expect(splitQuery('fix -label:"needs triage" repo:acme/api crash')).toEqual({
      qualifiers: [
        { key: 'label', value: 'needs triage', negated: true },
        { key: 'repo', value: 'acme/api', negated: false },
      ],
      text: 'fix crash',
    });
  });
});

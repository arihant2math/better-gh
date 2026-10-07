import { describe, expect, it } from 'vitest';
import { classify, JobLog, LogSearch, parseTimestamp, searchLog, StepLog } from './parse';
import { buildLayout, GroupState, VisibleCache } from './rows';

const TS = '2026-10-05T08:33:13.1234567Z';
const at = (s: string) => `${TS} ${s}\n`;

describe('parseTimestamp', () => {
  it('parses runner timestamps with 7 fractional digits', () => {
    const r = parseTimestamp(`${TS} hello`);
    expect(r).not.toBeNull();
    expect(r![0]).toBe(Date.UTC(2026, 9, 5, 8, 33, 13, 123));
    expect(`${TS} hello`.slice(r![1])).toBe('hello');
  });

  it('accepts no fraction and a timestamp-only line', () => {
    expect(parseTimestamp('2026-10-05T08:33:13Z')![0]).toBe(Date.UTC(2026, 9, 5, 8, 33, 13));
    expect(parseTimestamp('2026-10-05T08:33:13.5Z x')![0]).toBe(Date.UTC(2026, 9, 5, 8, 33, 13, 500));
  });

  it('rejects lines without a timestamp', () => {
    expect(parseTimestamp('hello world, this is long enough')).toBeNull();
    expect(parseTimestamp('2026-10-05 08:33:13Z x')).toBeNull();
  });
});

describe('classify', () => {
  it('recognizes ##[...] and ::...:: workflow commands', () => {
    expect(classify('##[group]Run actions/checkout@v4')).toEqual({ kind: 'group', text: 'Run actions/checkout@v4' });
    expect(classify('##[endgroup]')).toEqual({ kind: 'endgroup', text: '' });
    expect(classify('##[error]Process completed with exit code 1.')).toEqual({ kind: 'error', text: 'Process completed with exit code 1.' });
    expect(classify('##[warning]careful')).toEqual({ kind: 'warning', text: 'careful' });
    expect(classify('##[notice]fyi')).toEqual({ kind: 'notice', text: 'fyi' });
    expect(classify('##[debug]dbg')).toEqual({ kind: 'debug', text: 'dbg' });
    expect(classify('##[command]/usr/bin/git version')).toEqual({ kind: 'command', text: '/usr/bin/git version' });
    expect(classify('::group::Install')).toEqual({ kind: 'group', text: 'Install' });
    expect(classify('::endgroup::')).toEqual({ kind: 'endgroup', text: '' });
    expect(classify('::error file=app.js,line=1,col=5::Missing semicolon')).toEqual({ kind: 'error', text: 'Missing semicolon' });
    expect(classify('::debug::x')).toEqual({ kind: 'debug', text: 'x' });
    expect(classify('# heading')).toEqual({ kind: 'normal', text: '# heading' });
    expect(classify('::unknown::x')).toEqual({ kind: 'normal', text: '::unknown::x' });
  });
});

describe('StepLog', () => {
  it('splits timestamps and classifies lines', () => {
    const s = new StepLog(1);
    s.push(at('plain') + at('##[error]boom') + 'no timestamp\n');
    expect(s.lines).toEqual([
      { ts: Date.UTC(2026, 9, 5, 8, 33, 13, 123), text: 'plain', kind: 'normal' },
      { ts: Date.UTC(2026, 9, 5, 8, 33, 13, 123), text: 'boom', kind: 'error' },
      { ts: null, text: 'no timestamp', kind: 'normal' },
    ]);
  });

  it('builds groups and drops endgroup lines', () => {
    const s = new StepLog(2);
    s.push(at('before') + at('##[group]Run npm test') + at('npm test') + at('##[warning]w') + at('##[endgroup]') + at('after'));
    expect(s.lines.map((l) => [l.text, l.kind, l.groupId, l.inGroup])).toEqual([
      ['before', 'normal', undefined, undefined],
      ['Run npm test', 'group', 1, undefined],
      ['npm test', 'normal', undefined, 1],
      ['w', 'warning', undefined, 1],
      ['after', 'normal', undefined, undefined],
    ]);
    expect(s.groups).toEqual([1]);
  });

  it('closes an open group when a new one starts (no nesting)', () => {
    const s = new StepLog(1);
    s.push(at('::group::A') + at('a') + at('::group::B') + at('b') + at('::endgroup::'));
    expect(s.lines.map((l) => [l.text, l.groupId, l.inGroup])).toEqual([
      ['A', 0, undefined],
      ['a', undefined, 0],
      ['B', 2, undefined],
      ['b', undefined, 2],
    ]);
  });

  it('handles chunks split mid-line and mid-timestamp', () => {
    const text = at('one') + at('##[group]two') + at('three') + at('##[endgroup]') + at('four');
    const whole = new StepLog(1);
    whole.push(text);
    for (const size of [1, 3, 7, 29, 64]) {
      const s = new StepLog(1);
      let added = 0;
      for (let i = 0; i < text.length; i += size) added += s.push(text.slice(i, i + size));
      expect(s.lines).toEqual(whole.lines);
      expect(added).toBe(4);
    }
  });

  it('flushes a trailing partial line on finish and handles CRLF / progress CR', () => {
    const log = new JobLog();
    log.append(1, `${TS} a\r\n${TS} 10%\r50%\r100%\n${TS} tail`);
    expect(log.lineCount).toBe(2);
    log.finish();
    expect(log.lineCount).toBe(3);
    expect(log.steps.get(1)!.lines.map((l) => l.text)).toEqual(['a', '100%', 'tail']);
  });

  it('tracks the widest line without ANSI codes', () => {
    const s = new StepLog(1);
    s.push(at('\x1b[31mabc\x1b[0m') + at('ab'));
    expect(s.maxWidth).toBe(3);
  });
});

describe('search', () => {
  it('is case-insensitive, ignores ANSI codes and follows step order', () => {
    const log = new JobLog();
    log.append(2, at('Hello \x1b[31mWORLD\x1b[0m') + at('nothing'));
    log.append(1, at('world first'));
    expect(searchLog(log, 'o w', [1, 2])).toEqual([{ step: 2, line: 0 }]);
    expect(searchLog(log, 'WORLD', [1, 2])).toEqual([
      { step: 1, line: 0 },
      { step: 2, line: 0 },
    ]);
    expect(searchLog(log, '[31m', [1, 2])).toEqual([]);
    expect(searchLog(log, '', [1, 2])).toEqual([]);
  });

  it('updates incrementally as lines stream in', () => {
    const log = new JobLog();
    const search = new LogSearch();
    log.append(1, at('error one'));
    search.update(log, 'error');
    expect(search.result(log, [1]).total).toBe(1);
    log.append(1, at('ok') + at('another ERROR'));
    search.update(log, 'error');
    const r = search.result(log, [1]);
    expect(r.total).toBe(2);
    expect(r.at(1)).toEqual({ step: 1, line: 2 });
    expect(r.indexFrom(1, 1)).toBe(1);
    // A reset (reconnect) yields fresh StepLogs: no stale hits.
    log.reset();
    log.append(1, at('error'));
    search.update(log, 'error');
    expect(search.result(log, [1]).total).toBe(1);
  });

  it('parses and searches 100k lines quickly', () => {
    const chunk: string[] = [];
    for (let i = 0; i < 100_000; i++) {
      if (i % 1000 === 0) chunk.push(at(`##[group]Group ${i}`));
      chunk.push(at(i % 997 === 0 ? `\x1b[32mneedle\x1b[0m line ${i}` : `ordinary output line number ${i} with some text`));
      if (i % 1000 === 999) chunk.push(at('##[endgroup]'));
    }
    const text = chunk.join('');
    const t0 = performance.now();
    const log = new JobLog();
    for (let i = 0; i < text.length; i += 65536) log.append(1, text.slice(i, i + 65536));
    log.finish();
    const t1 = performance.now();
    const hits = searchLog(log, 'NEEDLE', [1]);
    const t2 = performance.now();
    expect(log.steps.get(1)!.lines.length).toBe(100_100);
    expect(hits.length).toBe(Math.ceil(100_000 / 997));
    expect(t1 - t0).toBeLessThan(1000);
    expect(t2 - t1).toBeLessThan(300);
  });
});

describe('layout', () => {
  it('flattens steps, hides collapsed groups and maps rows both ways', () => {
    const log = new JobLog();
    log.append(1, at('setup'));
    log.append(2, at('##[group]Run x') + at('in group') + at('##[endgroup]') + at('out'));
    const groups = new GroupState();
    const cache = new VisibleCache();
    const sections = (exp: boolean) => [
      { step: 1, expanded: false, log: log.steps.get(1), waiting: false },
      { step: 2, expanded: exp, log: log.steps.get(2), waiting: false },
      { step: 3, expanded: true, log: undefined, waiting: true },
    ];
    let l = buildLayout(sections(true), groups, cache);
    // step1 header, step2 header, group header, "out", step3 header, waiting
    expect(l.total).toBe(6);
    expect([0, 1, 2, 3, 4, 5].map((i) => l.rowAt(i))).toEqual([
      { kind: 'step', step: 1 },
      { kind: 'step', step: 2 },
      { kind: 'line', step: 2, line: 0 },
      { kind: 'line', step: 2, line: 2 },
      { kind: 'step', step: 3 },
      { kind: 'waiting', step: 3 },
    ]);
    expect(l.indexOf(2, 1)).toBe(-1);
    expect(l.indexOf(2, 2)).toBe(3);
    expect(l.headerIndex(3)).toBe(4);
    expect(l.stepAt(3)).toBe(2);

    groups.set(2, 0, true);
    l = buildLayout(sections(true), groups, cache);
    expect(l.total).toBe(7);
    expect(l.indexOf(2, 1)).toBe(3);

    groups.set(2, 0, false);
    log.append(2, at('more'));
    l = buildLayout(sections(true), groups, cache);
    expect(l.rowAt(4)).toEqual({ kind: 'line', step: 2, line: 3 });

    l = buildLayout(sections(false), groups, cache);
    expect(l.total).toBe(4);
  });
});

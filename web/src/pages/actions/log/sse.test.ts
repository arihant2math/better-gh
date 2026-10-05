import { afterEach, describe, expect, it } from 'vitest';
import { browserTransport, setTransport, type Transport } from '../../../api/transport';
import { SseParser, streamJobLog, type SseEvent } from './sse';

function parseAll(chunks: string[]): SseEvent[] {
  const out: SseEvent[] = [];
  const p = new SseParser((e) => out.push(e));
  for (const c of chunks) p.push(c);
  return out;
}

describe('SseParser', () => {
  it('parses named events, multi-line data and ignores comments', () => {
    const events = parseAll([': keep-alive\n\nevent: log\ndata: {"step":1,\ndata:"text":"a"}\nid: 7\n\n:ping\nevent: done\ndata: {}\n\n']);
    expect(events).toEqual([
      { event: 'log', data: '{"step":1,\n"text":"a"}', id: '7' },
      { event: 'done', data: '{}', id: '7' },
    ]);
  });

  it('handles chunk boundaries anywhere, including inside CRLF', () => {
    const text = 'event: log\r\ndata: {"step":2,"text":"x\\n"}\r\n\r\nevent: done\r\ndata: {}\r\n\r\n';
    const expected = parseAll([text]);
    expect(expected).toHaveLength(2);
    for (let i = 1; i < text.length; i++) {
      expect(parseAll([text.slice(0, i), text.slice(i)])).toEqual(expected);
    }
    expect(parseAll(text.split(''))).toEqual(expected);
  });

  it('supports bare CR line endings, missing space after colon and data-less events', () => {
    expect(parseAll(['data:a\rdata: b\r\r', 'event: done\n\n', 'data\n\n'])).toEqual([
      { event: 'message', data: 'a\nb', id: undefined },
      { event: 'done', data: '', id: undefined },
      { event: 'message', data: '', id: undefined },
    ]);
  });

  it('does not dispatch an unterminated event', () => {
    expect(parseAll(['event: log\ndata: {}\n'])).toEqual([]);
  });

  it('is linear on one huge event split into many chunks', () => {
    const big = `data: ${JSON.stringify({ step: 1, text: 'x'.repeat(4_000_000) })}\n\n`;
    const t0 = performance.now();
    const events = parseAll(big.match(/[\s\S]{1,16384}/g)!);
    expect(events).toHaveLength(1);
    expect(events[0]!.data.length).toBe(big.length - 8);
    expect(performance.now() - t0).toBeLessThan(500);
  });
});

function sseResponse(chunks: string[]): Response {
  const enc = new TextEncoder();
  const body = new ReadableStream<Uint8Array>({
    start(ctrl) {
      for (const c of chunks) ctrl.enqueue(enc.encode(c));
      ctrl.close();
    },
  });
  return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } });
}

const fake = (fetch: Transport['fetch']) => setTransport({ ...browserTransport, fetch });

const log = (step: number, text: string) => `event: log\ndata: ${JSON.stringify({ step, text })}\n\n`;

describe('streamJobLog', () => {
  afterEach(() => setTransport(browserTransport));

  it('streams log events through the transport and stops at done', async () => {
    const seen: string[] = [];
    let accept: string | null = null;
    fake(async (_path, init) => {
      accept = new Headers(init?.headers).get('accept');
      return sseResponse([log(1, 'a\n'), ': ping\n\n', log(2, 'b\n'), 'event: done\ndata: {}\n\n', log(3, 'ignored\n')]);
    });
    await streamJobLog('/x', { onReset: () => seen.push('reset'), onLog: (s, t) => seen.push(`${s}:${t}`), onDone: () => seen.push('done') }, new AbortController().signal);
    expect(accept).toBe('text/event-stream');
    expect(seen).toEqual(['reset', '1:a\n', '2:b\n', 'done']);
  });

  it('reconnects after a drop, resetting state first', async () => {
    const seen: string[] = [];
    let calls = 0;
    fake(async () => (++calls === 1 ? sseResponse([log(1, 'partial\n')]) : sseResponse([log(1, 'partial\n'), log(1, 'rest\n'), 'event: done\ndata: {}\n\n'])));
    await streamJobLog('/x', { onReset: () => seen.push('reset'), onLog: (_s, t) => seen.push(t.trim()), onDone: () => seen.push('done') }, new AbortController().signal);
    expect(calls).toBe(2);
    expect(seen).toEqual(['reset', 'partial', 'reset', 'partial', 'rest', 'done']);
  });

  it('gives up on client errors and stops when aborted', async () => {
    const errors: number[] = [];
    fake(async () => new Response('nope', { status: 404 }));
    await streamJobLog('/x', { onReset() {}, onLog() {}, onDone() {}, onError: (s) => errors.push(s) }, new AbortController().signal);
    expect(errors).toEqual([404]);

    const ctrl = new AbortController();
    fake(async () => {
      ctrl.abort();
      throw new DOMException('aborted', 'AbortError');
    });
    await streamJobLog('/x', { onReset() {}, onLog() {}, onDone() {} }, ctrl.signal);
  });
});

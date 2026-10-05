import { describe, expect, it } from 'vitest';
import { MockServer } from '../server';

describe('upload mocks', () => {
  const s = new MockServer(null, {});
  const up = (name: string, body: BodyInit) => s.fetch(`/_bgh/uploads?name=${encodeURIComponent(name)}`, { method: 'POST', body });

  it('stores images as renderable markdown', async () => {
    const r = await up('shot.png', new Blob([new Uint8Array([137, 80, 78, 71])], { type: 'image/png' }));
    expect(r.status).toBe(201);
    const a = (await r.json()) as { markdown: string; href: string };
    expect(a.markdown).toBe(`![shot.png](${a.href})`);
    expect(a.href.startsWith('data:image/png;base64,')).toBe(true);
  });

  it('serves files and rejects disallowed types', async () => {
    const r = await up('build.log', new Blob(['hello'], { type: 'text/plain' }));
    const a = (await r.json()) as { markdown: string; href: string };
    expect(a.markdown).toBe(`[build.log](${a.href})`);
    const got = await s.fetch(new URL(a.href).pathname, {});
    expect(await got.text()).toBe('hello');
    expect((await up('setup.exe', new Blob(['MZ']))).status).toBe(422);
  });
});

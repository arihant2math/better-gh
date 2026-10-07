import { describe, expect, it, vi } from 'vitest';
import { ShortcutManager } from './manager';

function key(k: string, mods: Partial<Pick<KeyboardEvent, 'ctrlKey' | 'metaKey' | 'shiftKey' | 'altKey'>> = {}) {
  let prevented = false;
  const e = {
    key: k,
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    altKey: false,
    isComposing: false,
    target: null,
    get defaultPrevented() {
      return prevented;
    },
    preventDefault: () => {
      prevented = true;
    },
    ...mods,
  };
  return e as unknown as KeyboardEvent;
}

describe('ShortcutManager', () => {
  it('fires single keys and sequences', () => {
    const m = new ShortcutManager();
    const j = vi.fn();
    const gi = vi.fn();
    m.register('t', [
      { keys: 'j', handler: j },
      { keys: 'g i', handler: gi },
    ]);
    m.handleKeyDown(key('j'));
    m.handleKeyDown(key('g'));
    m.handleKeyDown(key('i'));
    expect(j).toHaveBeenCalledOnce();
    expect(gi).toHaveBeenCalledOnce();
  });

  it('gives the most recent scope priority and lets `false` fall through', () => {
    const m = new ShortcutManager();
    const outer = vi.fn();
    const inner = vi.fn(() => false);
    m.register('outer', [{ keys: 'x', handler: outer }]);
    const dispose = m.register('inner', [{ keys: 'x', handler: inner }]);
    m.handleKeyDown(key('x'));
    expect(inner).toHaveBeenCalledOnce();
    expect(outer).toHaveBeenCalledOnce();
    dispose();
    m.handleKeyDown(key('x'));
    expect(inner).toHaveBeenCalledOnce();
  });

  it('normalizes mod and shifted symbols', () => {
    const m = new ShortcutManager();
    const k = vi.fn();
    const help = vi.fn();
    m.register('t', [
      { keys: 'mod+k', handler: k },
      { keys: '?', handler: help },
    ]);
    m.handleKeyDown(key('k', { ctrlKey: true, metaKey: true }));
    m.handleKeyDown(key('?', { shiftKey: true }));
    expect(k).toHaveBeenCalledOnce();
    expect(help).toHaveBeenCalledOnce();
  });
});

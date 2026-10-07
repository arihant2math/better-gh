/**
 * Keyboard shortcut manager.
 *
 * Key syntax: space-separated sequence of chords. A chord is `mod+shift+k`
 * style (`mod` = ⌘ on macOS, Ctrl elsewhere), or a single key: `j`, `?`,
 * `escape`, `enter`, `arrowdown`. Sequences: `g i` (press g, then i within 1s).
 *
 * Bindings live in scopes; the most recently registered scope wins, so a
 * page's `j/k` shadows nothing global while a dialog can capture `escape`.
 * Shortcuts don't fire while typing in inputs unless they use `mod` or the
 * binding sets `allowInInput`.
 *
 * Modal layers: while a modal is open (`pushLayer()`, done by `Dialog`), only
 * scopes registered after it opened fire, so page and global keys (`g h`,
 * `j`, `v`…) can't act underneath it. A scope belongs to the topmost layer
 * at registration time; when a layer closes, its leftover scopes drop to the
 * layer below.
 */

export interface Binding {
  keys: string;
  handler: (e: KeyboardEvent) => void | boolean;
  description?: string;
  group?: string;
  allowInInput?: boolean;
  /** Hide from the help dialog. */
  hidden?: boolean;
}

interface Scope {
  id: number;
  name: string;
  bindings: Binding[];
  /** Modal layer the scope belongs to (0 = page). */
  layer: number;
}

export const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

const SEQUENCE_TIMEOUT = 1000;

function normalizeChord(chord: string): string {
  const parts = chord.toLowerCase().split('+');
  const key = parts.pop()!;
  const mods = new Set(parts.map((m) => (m === 'cmd' || m === 'ctrl' || m === 'meta' ? 'mod' : m)));
  return [...['mod', 'alt', 'shift'].filter((m) => mods.has(m)), key].join('+');
}

export function chordFromEvent(e: KeyboardEvent): string | null {
  const key = e.key;
  if (!key || key === 'Shift' || key === 'Control' || key === 'Meta' || key === 'Alt' || key === 'Dead') return null;
  let k = key.length === 1 ? key.toLowerCase() : key.toLowerCase();
  if (k === ' ') k = 'space';
  const mods: string[] = [];
  if (isMac ? e.metaKey : e.ctrlKey) mods.push('mod');
  if (e.altKey) mods.push('alt');
  // Shift is implicit for printable symbols ("?" is shift+/); keep it for letters & named keys.
  if (e.shiftKey && (key.length > 1 || /[a-z]/i.test(key))) mods.push('shift');
  return [...mods, k].join('+');
}

export function isEditable(el: EventTarget | null): boolean {
  if (typeof HTMLElement === 'undefined' || !(el instanceof HTMLElement)) return false;
  if (el.isContentEditable) return true;
  const tag = el.tagName;
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true;
  if (tag === 'INPUT') {
    const type = (el as HTMLInputElement).type;
    return !['checkbox', 'radio', 'button', 'submit', 'reset'].includes(type);
  }
  return false;
}

/** Human-readable rendering: `mod+k` → `⌘K` / `Ctrl+K`. */
export function formatKeys(keys: string): string[] {
  return keys.split(' ').map((chord) =>
    chord
      .split('+')
      .map((p) => {
        if (p === 'mod') return isMac ? '⌘' : 'Ctrl';
        if (p === 'shift') return isMac ? '⇧' : 'Shift';
        if (p === 'alt') return isMac ? '⌥' : 'Alt';
        if (p === 'enter') return '↵';
        if (p === 'escape') return 'Esc';
        if (p === 'arrowup') return '↑';
        if (p === 'arrowdown') return '↓';
        if (p === 'backspace') return '⌫';
        return p.length === 1 ? p.toUpperCase() : p[0]!.toUpperCase() + p.slice(1);
      })
      .join(isMac ? '' : '+'),
  );
}

export class ShortcutManager {
  private scopes: Scope[] = [];
  private seq = 0;
  private buffer: string[] = [];
  private bufferTimer: ReturnType<typeof setTimeout> | null = null;
  private listeners = new Set<() => void>();
  private layers: number[] = [];

  private get topLayer(): number {
    return this.layers[this.layers.length - 1] ?? 0;
  }

  /** Open a modal layer that suspends all scopes registered before it. */
  pushLayer(): () => void {
    const layer = ++this.seq;
    this.layers.push(layer);
    this.buffer = [];
    return () => {
      const i = this.layers.indexOf(layer);
      if (i < 0) return;
      this.layers.splice(i, 1);
      const below = this.layers[i - 1] ?? 0;
      for (const s of this.scopes) if (s.layer === layer) s.layer = below;
      this.buffer = [];
    };
  }

  register(name: string, bindings: Binding[]): () => void {
    const scope: Scope = {
      id: ++this.seq,
      name,
      layer: this.topLayer,
      bindings: bindings.map((b) => ({ ...b, keys: b.keys.split(' ').map(normalizeChord).join(' ') })),
    };
    this.scopes.push(scope);
    this.emit();
    return () => {
      this.scopes = this.scopes.filter((s) => s !== scope);
      this.emit();
    };
  }

  /** Visible bindings, top scope first (for the help dialog). */
  list(): { scope: string; bindings: Binding[] }[] {
    return [...this.scopes].reverse().map((s) => ({ scope: s.name, bindings: s.bindings.filter((b) => !b.hidden) }));
  }

  subscribe(fn: () => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit() {
    this.listeners.forEach((l) => l());
  }

  handleKeyDown = (e: KeyboardEvent): void => {
    if (e.defaultPrevented || e.isComposing) return;
    const chord = chordFromEvent(e);
    if (!chord) return;
    const editable = isEditable(e.target);
    const layer = this.topLayer;
    const attempt = (seq: string[]): 'fired' | 'prefix' | 'none' => {
      const keys = seq.join(' ');
      let prefix = false;
      for (let i = this.scopes.length - 1; i >= 0; i--) {
        const scope = this.scopes[i]!;
        if (scope.layer !== layer) continue;
        for (const b of scope.bindings) {
          const usable = !editable || b.allowInInput || b.keys.startsWith('mod+') || b.keys === 'escape';
          if (!usable) continue;
          if (b.keys === keys) {
            const result = b.handler(e);
            if (result !== false) {
              e.preventDefault();
              return 'fired';
            }
          } else if (b.keys.startsWith(`${keys} `)) {
            prefix = true;
          }
        }
      }
      return prefix ? 'prefix' : 'none';
    };

    let r = attempt([...this.buffer, chord]);
    if (r === 'none' && this.buffer.length) {
      this.buffer = [];
      r = attempt([chord]);
    }
    if (r === 'prefix') {
      this.buffer.push(chord);
      if (this.bufferTimer) clearTimeout(this.bufferTimer);
      this.bufferTimer = setTimeout(() => (this.buffer = []), SEQUENCE_TIMEOUT);
    } else {
      this.buffer = [];
    }
  };
}

export const shortcuts = new ShortcutManager();

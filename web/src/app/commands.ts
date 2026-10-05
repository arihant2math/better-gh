/**
 * Command registry for the command palette (⌘K). Register global commands
 * once at startup, or contextual ones from a page with `useCommands`.
 */
import { makeAutoObservable } from 'mobx';
import { useEffect, useRef } from 'react';
import type { Icon } from '../ui/icons';

export interface Command {
  id: string;
  title: string;
  /** Section in the palette ("Navigation", "Issue", "Repository"...). */
  group: string;
  icon?: Icon;
  /** Display-only shortcut hint (register the actual key with useShortcuts). */
  shortcut?: string;
  /** Extra search terms. */
  keywords?: string;
  run: () => void;
}

class CommandRegistry {
  private entries = new Map<string, Command>();
  version = 0;

  constructor() {
    makeAutoObservable<CommandRegistry, 'entries'>(this, { entries: false });
  }

  register(cmds: Command[]): () => void {
    for (const c of cmds) this.entries.set(c.id, c);
    this.version++;
    return () => {
      for (const c of cmds) if (this.entries.get(c.id) === c) this.entries.delete(c.id);
      this.bump();
    };
  }

  bump() {
    this.version++;
  }

  list(): Command[] {
    void this.version;
    return [...this.entries.values()].reverse();
  }
}

export const commands = new CommandRegistry();

/** Register contextual commands while the component is mounted. */
export function useCommands(cmds: Command[], deps: unknown[]): void {
  const ref = useRef(cmds);
  useEffect(() => {
    ref.current = cmds;
  });
  useEffect(() => {
    return commands.register(ref.current.map((c) => ({ ...c, run: () => ref.current.find((x) => x.id === c.id)?.run() })));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- caller-controlled deps
  }, deps);
}

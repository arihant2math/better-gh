import { makeAutoObservable } from 'mobx';
import { currentMatch } from '../router';
import { repoByName } from '../sync/selectors';
import type { Repo } from '../sync/models';

/** App-level UI state (dialogs, sidebar). */
class UiState {
  paletteOpen = false;
  paletteMode: 'all' | 'commands' = 'all';
  helpOpen = false;
  newIssueRepoId: number | null = null;
  /** Repository whose watch settings dialog is open. */
  watchRepoId: number | null = null;
  sidebarCollapsed = false;

  constructor() {
    makeAutoObservable(this);
    try {
      this.sidebarCollapsed = localStorage.getItem('bgh.sidebar') === 'collapsed';
    } catch {
      /* ignore */
    }
  }

  openPalette(mode: 'all' | 'commands' = 'all') {
    this.paletteMode = mode;
    this.paletteOpen = true;
  }

  closePalette() {
    this.paletteOpen = false;
  }

  setHelp(open: boolean) {
    this.helpOpen = open;
  }

  openNewIssue(repoId: number) {
    this.newIssueRepoId = repoId;
  }

  closeNewIssue() {
    this.newIssueRepoId = null;
  }

  openWatch(repoId: number) {
    this.watchRepoId = repoId;
  }

  closeWatch() {
    this.watchRepoId = null;
  }

  toggleSidebar() {
    this.sidebarCollapsed = !this.sidebarCollapsed;
    try {
      localStorage.setItem('bgh.sidebar', this.sidebarCollapsed ? 'collapsed' : 'open');
    } catch {
      /* ignore */
    }
  }
}

export const ui = new UiState();

/** Path in the current repo (`/:owner/:repo<suffix>`), else `fallback`. */
export function repoPath(suffix: string, fallback: string): string {
  const r = currentRepo();
  return r ? `/${r.owner}/${r.name}${suffix}` : fallback;
}

/** The repo of the current URL, if any (for contextual commands). */
export function currentRepo(m = currentMatch()): Repo | undefined {
  const { owner, repo } = m?.params ?? {};
  return owner && repo ? repoByName(owner, repo) : undefined;
}

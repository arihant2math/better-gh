import { makeAutoObservable } from 'mobx';

export type ThemePref = 'system' | 'light' | 'dark';
export type Density = 'comfortable' | 'compact';

const KEY = 'bgh.theme';
const DENSITY_KEY = 'bgh.density';

function readDensity(): Density {
  try {
    return localStorage.getItem(DENSITY_KEY) === 'compact' ? 'compact' : 'comfortable';
  } catch {
    return 'comfortable';
  }
}

function read(): ThemePref {
  try {
    const v = localStorage.getItem(KEY);
    return v === 'light' || v === 'dark' ? v : 'system';
  } catch {
    return 'system';
  }
}

class Theme {
  pref: ThemePref = read();
  /** Row/control density; applied as `html[data-density]` (index.html sets it before first paint). */
  density: Density = readDensity();
  systemDark = typeof matchMedia !== 'undefined' && matchMedia('(prefers-color-scheme: dark)').matches;

  constructor() {
    makeAutoObservable(this);
    if (typeof matchMedia !== 'undefined') {
      matchMedia('(prefers-color-scheme: dark)').addEventListener('change', (e) => this.setSystemDark(e.matches));
    }
  }

  get resolved(): 'light' | 'dark' {
    return this.pref === 'system' ? (this.systemDark ? 'dark' : 'light') : this.pref;
  }

  setSystemDark(v: boolean) {
    this.systemDark = v;
  }

  set(pref: ThemePref) {
    this.pref = pref;
    try {
      if (pref === 'system') localStorage.removeItem(KEY);
      else localStorage.setItem(KEY, pref);
    } catch {
      /* ignore */
    }
    const root = document.documentElement;
    // Suppress transitions while swapping palettes.
    root.classList.add('theme-switching');
    if (pref === 'system') delete root.dataset.theme;
    else root.dataset.theme = pref;
    requestAnimationFrame(() => requestAnimationFrame(() => root.classList.remove('theme-switching')));
  }

  setDensity(density: Density) {
    this.density = density;
    try {
      if (density === 'comfortable') localStorage.removeItem(DENSITY_KEY);
      else localStorage.setItem(DENSITY_KEY, density);
    } catch {
      /* ignore */
    }
    if (typeof document === 'undefined') return;
    if (density === 'comfortable') delete document.documentElement.dataset.density;
    else document.documentElement.dataset.density = density;
  }

  toggle() {
    this.set(this.resolved === 'dark' ? 'light' : 'dark');
  }
}

export const theme = new Theme();

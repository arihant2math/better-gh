/**
 * Minimal router (~250 lines, no dependency) built for this app's needs:
 *
 * - route table with `:params` and a trailing `*` splat, first match wins;
 * - every route is a lazy chunk (`load`) plus an optional data `prefetch`;
 * - `<Link>` preloads the chunk and runs `prefetch` on hover/focus/touch, so
 *   by the time the click lands the page renders synchronously from the store;
 * - navigation never shows a blank page: the previous page stays until the
 *   next chunk is loaded (usually already preloaded);
 * - optional persistent layouts (`layout`) that stay mounted across sibling
 *   routes (e.g. the repo header across Issues/PRs/Code tabs);
 * - scroll restoration on back/forward.
 */
import {
  createContext,
  use,
  useEffect,
  useLayoutEffect,
  useState,
  useSyncExternalStore,
  type AnchorHTMLAttributes,
  type ComponentType,
  type MouseEvent,
  type ReactNode,
} from 'react';
import { isChunkLoadError, reloadForChunkError } from './chunkError';

export type Params = Record<string, string>;

type Module<P = object> = { default: ComponentType<P> };

export interface RouteDef {
  /** Pattern, e.g. `/:owner/:repo/issues/:number` or `/:owner/:repo/tree/:ref/*`. */
  path: string;
  load: () => Promise<Module>;
  /** Persistent layout wrapping the page; receives `children`. */
  layout?: () => Promise<Module<{ children: ReactNode }>>;
  /** Warm data for this route (store partial sync, resource cache). Must not throw. */
  prefetch?: (params: Params, query: URLSearchParams) => void;
  /** Document title. */
  title?: (params: Params) => string;
}

interface CompiledRoute extends RouteDef {
  re: RegExp;
  keys: string[];
}

export interface Match {
  route: CompiledRoute;
  params: Params;
}

// ----------------------------------------------------------------- matching

let routes: CompiledRoute[] = [];

function compile(def: RouteDef): CompiledRoute {
  const keys: string[] = [];
  const src = def.path
    .split('/')
    .filter(Boolean)
    .map((seg) => {
      if (seg === '*') {
        keys.push('*');
        return '(?:/(.*))?';
      }
      if (seg.startsWith(':')) {
        keys.push(seg.slice(1));
        return '/([^/]+)';
      }
      return `/${seg.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}`;
    })
    .join('');
  return { ...def, re: new RegExp(`^${src || '/'}/?$`, 'i'), keys };
}

/** Segment ranks: static > `:param` > `*` (splat). */
function rank(path: string): number[] {
  return path
    .split('/')
    .filter(Boolean)
    .map((seg) => (seg === '*' ? 1 : seg.startsWith(':') ? 2 : 3));
}

/** Negative when `a` is more specific than `b` (segment by segment; a splat-free prefix first). */
function bySpecificity(a: number[], b: number[]): number {
  for (let i = 0; i < Math.min(a.length, b.length); i++) if (a[i] !== b[i]) return b[i]! - a[i]!;
  return a.length - b.length;
}

/**
 * Register the route table. The most specific route wins (static segments
 * beat `:params`, which beat `*`), so feature areas can append routes in any
 * order; equally specific routes keep their table order.
 */
export function defineRoutes(defs: RouteDef[]): void {
  routes = defs
    .map((d, i) => ({ d, i, r: rank(d.path) }))
    .sort((x, y) => bySpecificity(x.r, y.r) || x.i - y.i)
    .map((x) => compile(x.d));
}

export function matchPath(pathname: string): Match | null {
  for (const route of routes) {
    const m = route.re.exec(pathname);
    if (!m) continue;
    const params: Params = {};
    route.keys.forEach((k, i) => {
      params[k] = m[i + 1] ? decodeURIComponent(m[i + 1]!) : '';
    });
    return { route, params };
  }
  return null;
}

// ----------------------------------------------------------------- module cache

const modules = new Map<() => Promise<Module<never>>, { promise: Promise<unknown>; mod?: Module<never> }>();

function loadModule<P>(loader: () => Promise<Module<P>>): Promise<Module<P>> {
  let e = modules.get(loader as never);
  if (!e) {
    const entry: { promise: Promise<unknown>; mod?: Module<never> } = { promise: Promise.resolve() };
    entry.promise = loader().then((mod) => {
      entry.mod = mod as Module<never>;
      return mod;
    });
    entry.promise.catch(() => modules.delete(loader as never));
    modules.set(loader as never, entry);
    e = entry;
  }
  return e.promise as Promise<Module<P>>;
}

function loaded<P>(loader?: () => Promise<Module<P>>): Module<P> | undefined {
  return loader ? (modules.get(loader as never)?.mod as Module<P> | undefined) : undefined;
}

function matchReady(m: Match): boolean {
  return !!loaded(m.route.load) && (!m.route.layout || !!loaded(m.route.layout));
}

async function loadMatch(m: Match): Promise<void> {
  await Promise.all([loadModule(m.route.load), m.route.layout && loadModule(m.route.layout)]);
}

const prefetched = new Map<string, number>();

/** Preload the chunk and data for `href` (called on hover/focus of links). */
export function prefetch(href: string): void {
  const url = new URL(href, window.location.origin);
  if (url.origin !== window.location.origin) return;
  const m = matchPath(url.pathname);
  if (!m) return;
  void loadMatch(m).catch(() => undefined);
  // Data prefetch at most every 10s per URL.
  const key = url.pathname + url.search;
  const last = prefetched.get(key) ?? 0;
  if (Date.now() - last < 10_000) return;
  prefetched.set(key, Date.now());
  try {
    m.route.prefetch?.(m.params, url.searchParams);
  } catch {
    /* prefetch is best-effort */
  }
}

// ----------------------------------------------------------------- history

type Listener = () => void;
const listeners = new Set<Listener>();
let scrollKey = 0;
const scrollPositions = new Map<number, number>();
/** Stack of registered scroll containers; the innermost (last) one is used. */
const scrollContainers: HTMLElement[] = [];
const currentScroller = (): HTMLElement | null => scrollContainers[scrollContainers.length - 1] ?? null;
let pendingScroll: { restore?: number } | null = null;

function notify() {
  listeners.forEach((l) => l());
}

if (typeof window !== 'undefined') {
  if ('scrollRestoration' in history) history.scrollRestoration = 'manual';
  scrollKey = (history.state as { k?: number } | null)?.k ?? Date.now();
  if (!(history.state as { k?: number } | null)?.k) history.replaceState({ k: scrollKey }, '');
  window.addEventListener('popstate', (e) => {
    const sc = currentScroller();
    if (sc) scrollPositions.set(scrollKey, sc.scrollTop);
    scrollKey = (e.state as { k?: number } | null)?.k ?? Date.now();
    pendingScroll = { restore: scrollPositions.get(scrollKey) ?? 0 };
    notify();
  });
}

export interface NavigateOptions {
  replace?: boolean;
  /** Keep the scroll position (e.g. changing a filter in the query string). */
  keepScroll?: boolean;
}

/**
 * `to` as a same-origin `pathname + search + hash`, or null when it resolves
 * elsewhere (`//x`, `/\x`, `/\t/x`, `/..//x`, `https://x`, `javascript:`). Use it for
 * any untrusted target such as `?return_to=`.
 */
export function sameOriginPath(to: string, origin: string): string | null {
  let url: URL;
  try {
    url = new URL(to, origin);
  } catch {
    return null;
  }
  // Dot segments can normalize to a protocol-relative path (`/..//x` → `//x`).
  if (url.origin !== origin || url.pathname.startsWith('//')) return null;
  return url.pathname + url.search + url.hash;
}

/** Where to go after signing in: a same-origin `return_to`, else home. */
export function returnTo(search = window.location.search, origin = window.location.origin): string {
  const ret = new URLSearchParams(search).get('return_to');
  return (ret && sameOriginPath(ret, origin)) || '/';
}

export function navigate(to: string, opts: NavigateOptions = {}): void {
  const url = new URL(to, window.location.href);
  if (url.origin !== window.location.origin) {
    // Full-page loads only for web URLs: never `javascript:`/`data:` targets.
    if (url.protocol === 'http:' || url.protocol === 'https:') window.location.href = url.href;
    return;
  }
  const next = url.pathname + url.search + url.hash;
  if (next === window.location.pathname + window.location.search + window.location.hash) return;
  const sc = currentScroller();
  if (sc) scrollPositions.set(scrollKey, sc.scrollTop);
  if (opts.replace) {
    history.replaceState({ k: scrollKey }, '', next);
  } else {
    scrollKey = Date.now();
    history.pushState({ k: scrollKey }, '', next);
    pendingScroll = opts.keepScroll ? null : { restore: 0 };
  }
  notify();
}

function subscribe(l: Listener) {
  listeners.add(l);
  return () => listeners.delete(l);
}

const getHref = () => window.location.pathname + window.location.search;

/** Current `pathname + search`; re-renders on navigation. */
export function useLocation(): { pathname: string; search: string; href: string } {
  const href = useSyncExternalStore(subscribe, getHref, () => '/');
  const i = href.indexOf('?');
  return { href, pathname: i < 0 ? href : href.slice(0, i), search: i < 0 ? '' : href.slice(i) };
}

export function useQuery(): URLSearchParams {
  const { search } = useLocation();
  return new URLSearchParams(search);
}

/** Merge `patch` into the query string (null/'' deletes). */
export function setQuery(patch: Record<string, string | null | undefined>, opts: NavigateOptions = { replace: true, keepScroll: true }): void {
  const q = new URLSearchParams(window.location.search);
  for (const [k, v] of Object.entries(patch)) {
    if (v == null || v === '') q.delete(k);
    else q.set(k, v);
  }
  const s = q.toString();
  navigate(window.location.pathname + (s ? `?${s}` : ''), opts);
}

// ----------------------------------------------------------------- rendering

const RouteContext = createContext<Match | null>(null);

export function useParams<P extends Params = Params>(): P {
  return (use(RouteContext)?.params ?? {}) as P;
}

export function useMatch(): Match | null {
  return use(RouteContext);
}

/** Register the element whose scroll position is saved/restored per history entry. */
export function useScrollContainer(el: HTMLElement | null): void {
  useEffect(() => {
    if (!el) return;
    scrollContainers.push(el);
    return () => {
      const i = scrollContainers.lastIndexOf(el);
      if (i >= 0) scrollContainers.splice(i, 1);
    };
  }, [el]);
}

export function RouterView({ notFound: NotFound }: { notFound: ComponentType }) {
  const { pathname, href } = useLocation();
  const match = matchPath(pathname);
  // The match currently on screen; we only swap once the next one's code is loaded.
  const [shown, setShown] = useState<{ match: Match | null; href: string }>(() => ({ match, href }));
  const [, force] = useState(0);
  // A route chunk that failed to load (and wasn't fixed by a reload): thrown
  // during render so the surrounding error boundary shows its fallback.
  const [failed, setFailed] = useState<{ error: unknown } | null>(null);
  const target = match && !matchReady(match) ? null : { match, href };

  if (target && (shown.href !== target.href || shown.match?.route !== target.match?.route)) {
    setShown(target);
  }

  useEffect(() => {
    if (match && !matchReady(match)) {
      let cancelled = false;
      loadMatch(match).then(
        () => !cancelled && force((n) => n + 1),
        (err: unknown) => {
          console.error('[router] failed to load route', err);
          if (cancelled) return;
          // A stale chunk after a deploy → one hard reload to get fresh assets;
          // otherwise (or if that already happened) show the error UI.
          if (!(isChunkLoadError(err) && reloadForChunkError())) setFailed({ error: err });
        },
      );
      return () => {
        cancelled = true;
      };
    }
  }, [match, href]);

  useLayoutEffect(() => {
    const sc = currentScroller();
    if (pendingScroll && sc) {
      sc.scrollTop = pendingScroll.restore ?? 0;
      pendingScroll = null;
    }
    const m = shown.match;
    if (m?.route.title) document.title = `${m.route.title(m.params)} · Better GitHub`;
  }, [shown]);

  if (failed) throw failed.error;
  const m = shown.match;
  if (!m) return <NotFound />;
  const Page = loaded(m.route.load)?.default as ComponentType | undefined;
  const Layout = loaded(m.route.layout)?.default;
  if (!Page) return null;
  const page = <Page />;
  return <RouteContext value={m}>{Layout ? <Layout>{page}</Layout> : page}</RouteContext>;
}

// ----------------------------------------------------------------- links

export interface LinkProps extends AnchorHTMLAttributes<HTMLAnchorElement> {
  to: string;
  replace?: boolean;
  /** Prefetch chunk + data on hover/focus (default true). */
  prefetch?: boolean;
  children?: ReactNode;
}

function isModified(e: MouseEvent) {
  return e.metaKey || e.ctrlKey || e.shiftKey || e.altKey || e.button !== 0;
}

export function Link({ to, replace, prefetch: doPrefetch = true, onClick, onMouseEnter, onFocus, onTouchStart, children, ...rest }: LinkProps) {
  return (
    <a
      href={to}
      {...rest}
      onClick={(e) => {
        onClick?.(e);
        if (e.defaultPrevented || isModified(e) || rest.target) return;
        e.preventDefault();
        navigate(to, { replace });
      }}
      onMouseEnter={(e) => {
        onMouseEnter?.(e);
        if (doPrefetch) prefetch(to);
      }}
      onFocus={(e) => {
        onFocus?.(e);
        if (doPrefetch) prefetch(to);
      }}
      onTouchStart={(e) => {
        onTouchStart?.(e);
        if (doPrefetch) prefetch(to);
      }}
    >
      {children}
    </a>
  );
}

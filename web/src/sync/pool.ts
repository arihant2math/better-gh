import { createAtom, observable, runInAction, type IAtom } from 'mobx';
import type { ID, ModelMap, ModelName } from './models';
import { applyOps, type OverlayOp } from './overlay';
import type { Delta, ModelRows } from './protocol';
import { MODEL_NAMES, SCHEMA, type ScopeLookup } from './schema';

type Row = Record<string, unknown> & { id: ID };
type Ro<T> = Readonly<T>;

const rowKey = (model: ModelName, id: ID) => `${model}:${id}`;

function sameValue(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (Array.isArray(a) && Array.isArray(b)) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) if (!Object.is(a[i], b[i])) return false;
    return true;
  }
  if (a && b && typeof a === 'object' && typeof b === 'object') {
    const ka = Object.keys(a);
    if (ka.length !== Object.keys(b).length) return false;
    return ka.every((k) => Object.is((a as Record<string, unknown>)[k], (b as Record<string, unknown>)[k]));
  }
  return false;
}

export type BaseChangeListener = (model: ModelName, id: ID, row: Row | null) => void;

/**
 * The normalized, reactive object pool.
 *
 * - `base`: confirmed server state (plain objects; what gets persisted).
 * - overlays: pending optimistic transactions, applied in order on top of base.
 * - `live`: the visible rows (MobX observable objects, shallow) = base ⊕ overlays.
 *   UI code only ever sees live rows, via `get` / `all` / `byIndex` / `byKey`.
 *
 * Every read is tracked by MobX at a fine granularity: a field read tracks
 * that field, `get` tracks row existence, `byIndex` tracks one index bucket.
 */
export class ObjectPool {
  private base = new Map<ModelName, Map<ID, Row>>();
  private live = new Map<ModelName, Map<ID, Row>>();
  private overlays: { tx: string; ops: OverlayOp[] }[] = [];
  private overlayRefs = new Map<string, Set<string>>();
  private versions = new Map<string, number>();
  private collAtoms = new Map<ModelName, IAtom>();
  private existAtoms = new Map<string, IAtom>();
  private idx = new Map<string, Map<unknown, Set<ID>>>();
  private idxAtoms = new Map<string, IAtom>();
  private keyMaps = new Map<string, Map<string, ID>>();
  private baseListeners = new Set<BaseChangeListener>();

  constructor(public viewerId: ID) {
    for (const m of MODEL_NAMES) {
      this.base.set(m, new Map());
      this.live.set(m, new Map());
      this.collAtoms.set(m, createAtom(`pool.${m}`));
    }
  }

  // ---------------------------------------------------------------- reads

  get<M extends ModelName>(model: M, id: ID | null | undefined): Ro<ModelMap[M]> | undefined {
    if (id == null) return undefined;
    this.existAtom(model, id).reportObserved();
    return this.live.get(model)!.get(id) as ModelMap[M] | undefined;
  }

  all<M extends ModelName>(model: M): Ro<ModelMap[M]>[] {
    this.collAtoms.get(model)!.reportObserved();
    return [...this.live.get(model)!.values()] as unknown as ModelMap[M][];
  }

  count(model: ModelName): number {
    this.collAtoms.get(model)!.reportObserved();
    return this.live.get(model)!.size;
  }

  byIndex<M extends ModelName>(model: M, field: string, value: unknown): Ro<ModelMap[M]>[] {
    this.idxAtom(model, field, value).reportObserved();
    const ids = this.idx.get(`${model}.${field}`)?.get(value);
    if (!ids) return [];
    const live = this.live.get(model)!;
    const out: ModelMap[M][] = [];
    for (const id of ids) {
      const r = live.get(id);
      if (r) out.push(r as unknown as ModelMap[M]);
    }
    return out;
  }

  byKey<M extends ModelName>(model: M, keyName: string, key: string): Ro<ModelMap[M]> | undefined {
    this.idxAtom(model, `#${keyName}`, key).reportObserved();
    const id = this.keyMaps.get(`${model}.${keyName}`)?.get(key);
    return id == null ? undefined : (this.live.get(model)!.get(id) as ModelMap[M] | undefined);
  }

  /** Confirmed (server) row, ignoring overlays. Not reactive. */
  baseRow<M extends ModelName>(model: M, id: ID): ModelMap[M] | undefined {
    return this.base.get(model)!.get(id) as ModelMap[M] | undefined;
  }

  baseRows(model: ModelName): Row[] {
    return [...this.base.get(model)!.values()];
  }

  version(model: ModelName, id: ID): number {
    return this.versions.get(rowKey(model, id)) ?? 0;
  }

  onBaseChange(fn: BaseChangeListener): () => void {
    this.baseListeners.add(fn);
    return () => this.baseListeners.delete(fn);
  }

  // ---------------------------------------------------------------- server writes

  /** Bulk upsert server rows (bootstrap / hydrate / partial). */
  loadRows(rows: ModelRows, opts: { notNewerThan?: number; persist?: boolean } = {}): void {
    runInAction(() => {
      for (const model of Object.keys(rows) as ModelName[]) {
        if (!SCHEMA[model]) continue;
        for (const row of rows[model] ?? []) {
          const r = row as unknown as Row;
          if (opts.notNewerThan != null && this.version(model, r.id) > opts.notNewerThan) continue;
          this.upsertBase(model, r, opts.persist ?? true);
        }
      }
    });
  }

  /**
   * Apply deltas atomically (one MobX action → one render).
   * `onTx` is called (inside the same action) for every echoed client tx so
   * the transaction queue can drop its overlay without a flicker.
   */
  applyDeltas(deltas: readonly Delta[], onTx?: (tx: string) => void): void {
    runInAction(() => {
      for (const d of deltas) {
        if (!SCHEMA[d.model]) continue;
        for (const u of d.refs?.user ?? []) this.upsertBase('user', u as unknown as Row, true);
        if (d.a === 'D') this.deleteBase(d.model, d.mid);
        else this.upsertBase(d.model, { ...(d.d ?? {}), id: d.mid } as Row, true);
        this.versions.set(rowKey(d.model, d.mid), d.id);
        if (d.tx && onTx) onTx(d.tx);
      }
    });
  }

  /** Drop every row belonging to `scope` (revoke). */
  removeScope(scope: string): void {
    runInAction(() => {
      // Collect first: scopes may be derived from other rows (project children → project → org).
      const doomed: [ModelName, ID][] = [];
      const lookup = this.lookupBase;
      for (const model of MODEL_NAMES) {
        const scopeFn = SCHEMA[model].scope as ((r: Row, v: ID, l: typeof lookup) => string) | null;
        if (!scopeFn) continue;
        for (const row of this.base.get(model)!.values()) {
          if (scopeFn(row, this.viewerId, lookup) === scope) doomed.push([model, row.id]);
        }
      }
      for (const [model, id] of doomed) this.deleteBase(model, id);
    });
  }

  /** Base-row lookup for scope functions. */
  readonly lookupBase = (<M extends ModelName>(model: M, id: ID) => this.base.get(model)!.get(id) as ModelMap[M] | undefined) as ScopeLookup;

  /**
   * Remove confirmed rows that are not covered by a synced scope (e.g. rows
   * merged from a project snapshot that disappeared server-side).
   */
  removeRows(model: ModelName, ids: readonly ID[]): void {
    if (!ids.length) return;
    runInAction(() => {
      for (const id of ids) this.deleteBase(model, id);
    });
  }

  clear(): void {
    runInAction(() => {
      this.overlays = [];
      this.overlayRefs.clear();
      this.versions.clear();
      for (const m of MODEL_NAMES) {
        for (const id of [...this.live.get(m)!.keys()]) {
          this.base.get(m)!.delete(id);
          this.setLive(m, id, undefined);
        }
        this.base.get(m)!.clear();
      }
    });
  }

  private upsertBase(model: ModelName, row: Row, persist: boolean): void {
    const map = this.base.get(model)!;
    const prev = map.get(row.id);
    const next = prev ? { ...prev, ...row } : { ...row };
    map.set(row.id, next);
    if (persist) for (const l of this.baseListeners) l(model, row.id, next);
    this.recompute(model, row.id);
  }

  private deleteBase(model: ModelName, id: ID): void {
    const map = this.base.get(model)!;
    const existed = map.delete(id);
    if (existed) for (const l of this.baseListeners) l(model, id, null);
    this.recompute(model, id);
    for (const c of SCHEMA[model].cascade ?? []) {
      const ids = this.idx.get(`${c.model}.${c.field}`)?.get(id);
      const baseChildren = [...this.base.get(c.model)!.values()].filter((r) => r[c.field] === id);
      const all = new Set<ID>([...(ids ?? []), ...baseChildren.map((r) => r.id)]);
      for (const cid of all) this.deleteBase(c.model, cid);
    }
  }

  // ---------------------------------------------------------------- overlays

  addOverlay(tx: string, opsList: OverlayOp[]): void {
    runInAction(() => {
      this.overlays.push({ tx, ops: opsList });
      for (const o of opsList) {
        const k = rowKey(o.model, o.id);
        let s = this.overlayRefs.get(k);
        if (!s) this.overlayRefs.set(k, (s = new Set()));
        s.add(tx);
      }
      for (const o of uniqueRows(opsList)) this.recompute(o.model, o.id);
    });
  }

  removeOverlay(tx: string): boolean {
    const i = this.overlays.findIndex((o) => o.tx === tx);
    if (i < 0) return false;
    runInAction(() => {
      const [removed] = this.overlays.splice(i, 1);
      for (const o of removed!.ops) {
        const k = rowKey(o.model, o.id);
        const s = this.overlayRefs.get(k);
        s?.delete(tx);
        if (s && s.size === 0) this.overlayRefs.delete(k);
      }
      for (const o of uniqueRows(removed!.ops)) this.recompute(o.model, o.id);
    });
    return true;
  }

  hasOverlay(tx: string): boolean {
    return this.overlays.some((o) => o.tx === tx);
  }

  /** Is this row affected by a pending optimistic transaction? (not reactive) */
  isPending(model: ModelName, id: ID): boolean {
    return this.overlayRefs.has(rowKey(model, id));
  }

  // ---------------------------------------------------------------- internals

  private recompute(model: ModelName, id: ID): void {
    const k = rowKey(model, id);
    const txs = this.overlayRefs.get(k);
    const base = this.base.get(model)!.get(id);
    if (!txs) {
      this.setLive(model, id, base);
      return;
    }
    const rowOps: OverlayOp[] = [];
    for (const o of this.overlays) {
      if (!txs.has(o.tx)) continue;
      for (const op of o.ops) if (op.model === model && op.id === id) rowOps.push(op);
    }
    this.setLive(model, id, applyOps(base, rowOps) as Row | undefined);
  }

  private setLive(model: ModelName, id: ID, next: Row | undefined): void {
    const liveMap = this.live.get(model)!;
    const cur = liveMap.get(id);
    if (!next) {
      if (!cur) return;
      this.unindex(model, cur);
      liveMap.delete(id);
      this.collAtoms.get(model)!.reportChanged();
      this.existAtoms.get(rowKey(model, id))?.reportChanged();
      return;
    }
    if (!cur) {
      const init: Record<string, unknown> = {};
      for (const f of SCHEMA[model].lazyFields ?? []) init[f] = undefined;
      const obs = observable({ ...init, ...next }, undefined, { deep: false }) as Row;
      liveMap.set(id, obs);
      this.index(model, obs);
      this.collAtoms.get(model)!.reportChanged();
      this.existAtoms.get(rowKey(model, id))?.reportChanged();
      return;
    }
    const changes: [string, unknown][] = [];
    for (const key of Object.keys(next)) {
      if (!sameValue(cur[key], next[key])) changes.push([key, next[key]]);
    }
    for (const key of Object.keys(cur)) {
      if (!(key in next) && cur[key] !== undefined) changes.push([key, undefined]);
    }
    if (changes.length === 0) return;
    const before = this.indexSnapshot(model, cur);
    for (const [key, value] of changes) (cur as Record<string, unknown>)[key] = value;
    this.reindex(model, cur, before);
  }

  /** Current index memberships of a row: `field → values` and `keyName → key`. */
  private indexSnapshot(model: ModelName, row: Row): Map<string, unknown[]> {
    const snap = new Map<string, unknown[]>();
    for (const field of SCHEMA[model].indexes ?? []) snap.set(field, valuesOf(row[field]));
    for (const [keyName, fn] of Object.entries(SCHEMA[model].keys ?? {})) {
      snap.set(`#${keyName}`, [(fn as (r: Row) => string)(row)]);
    }
    return snap;
  }

  /** Move a row between index buckets, notifying only buckets whose membership changed. */
  private reindex(model: ModelName, row: Row, before: Map<string, unknown[]>): void {
    const after = this.indexSnapshot(model, row);
    for (const [name, newVals] of after) {
      const oldVals = before.get(name) ?? [];
      const removed = oldVals.filter((v) => !newVals.includes(v));
      const added = newVals.filter((v) => !oldVals.includes(v));
      if (!removed.length && !added.length) continue;
      if (name.startsWith('#')) {
        const map = this.keyMaps.get(`${model}.${name.slice(1)}`)!;
        for (const k of removed) {
          if (map.get(k as string) === row.id) map.delete(k as string);
          this.idxAtoms.get(`${model}.${name}:${String(k)}`)?.reportChanged();
        }
        for (const k of added) {
          map.set(k as string, row.id);
          this.idxAtoms.get(`${model}.${name}:${String(k)}`)?.reportChanged();
        }
      } else {
        let map = this.idx.get(`${model}.${name}`);
        if (!map) this.idx.set(`${model}.${name}`, (map = new Map()));
        for (const v of removed) {
          map.get(v)?.delete(row.id);
          this.idxAtoms.get(`${model}.${name}:${String(v)}`)?.reportChanged();
        }
        for (const v of added) {
          let set = map.get(v);
          if (!set) map.set(v, (set = new Set()));
          set.add(row.id);
          this.idxAtoms.get(`${model}.${name}:${String(v)}`)?.reportChanged();
        }
      }
    }
  }

  private index(model: ModelName, row: Row): void {
    for (const field of SCHEMA[model].indexes ?? []) {
      const name = `${model}.${field}`;
      let map = this.idx.get(name);
      if (!map) this.idx.set(name, (map = new Map()));
      for (const v of valuesOf(row[field])) {
        let set = map.get(v);
        if (!set) map.set(v, (set = new Set()));
        set.add(row.id);
        this.idxAtoms.get(`${model}.${field}:${String(v)}`)?.reportChanged();
      }
    }
    for (const [keyName, fn] of Object.entries(SCHEMA[model].keys ?? {})) {
      const name = `${model}.${keyName}`;
      let map = this.keyMaps.get(name);
      if (!map) this.keyMaps.set(name, (map = new Map()));
      const k = (fn as (r: Row) => string)(row);
      map.set(k, row.id);
      this.idxAtoms.get(`${model}.#${keyName}:${k}`)?.reportChanged();
    }
  }

  private unindex(model: ModelName, row: Row): void {
    for (const field of SCHEMA[model].indexes ?? []) {
      const map = this.idx.get(`${model}.${field}`);
      for (const v of valuesOf(row[field])) {
        map?.get(v)?.delete(row.id);
        this.idxAtoms.get(`${model}.${field}:${String(v)}`)?.reportChanged();
      }
    }
    for (const [keyName, fn] of Object.entries(SCHEMA[model].keys ?? {})) {
      const map = this.keyMaps.get(`${model}.${keyName}`);
      const k = (fn as (r: Row) => string)(row);
      if (map?.get(k) === row.id) map.delete(k);
      this.idxAtoms.get(`${model}.#${keyName}:${k}`)?.reportChanged();
    }
  }

  private idxAtom(model: ModelName, field: string, value: unknown): IAtom {
    const name = `${model}.${field}:${String(value)}`;
    let a = this.idxAtoms.get(name);
    if (!a) this.idxAtoms.set(name, (a = createAtom(name)));
    return a;
  }

  private existAtom(model: ModelName, id: ID): IAtom {
    const name = rowKey(model, id);
    let a = this.existAtoms.get(name);
    if (!a) this.existAtoms.set(name, (a = createAtom(name)));
    return a;
  }
}

function valuesOf(v: unknown): unknown[] {
  if (Array.isArray(v)) return v;
  if (v === undefined) return [];
  return [v];
}

function uniqueRows(opsList: OverlayOp[]): { model: ModelName; id: ID }[] {
  const seen = new Set<string>();
  const out: { model: ModelName; id: ID }[] = [];
  for (const o of opsList) {
    const k = rowKey(o.model, o.id);
    if (!seen.has(k)) {
      seen.add(k);
      out.push({ model: o.model, id: o.id });
    }
  }
  return out;
}

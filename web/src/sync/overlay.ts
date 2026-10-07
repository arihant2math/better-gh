import type { ID, ModelMap, ModelName } from './models';

/** Array patch that composes with concurrent edits: `{ $add: [3], $remove: [5] }`. */
export interface ArrayPatch<T> {
  $add?: T[];
  $remove?: T[];
}

/**
 * Object patch that composes with concurrent edits of other keys:
 * `{ $merge: { fieldA: 1, fieldB: null } }` shallow-merges; `null` removes the key.
 */
export interface ObjectPatch<V> {
  $merge: Record<string, V | null>;
}

export type Patch<R> = {
  [K in keyof R]?:
    | R[K]
    | (NonNullable<R[K]> extends (infer E)[] ? ArrayPatch<E> : NonNullable<R[K]> extends Record<string, infer V> ? ObjectPatch<V> : never);
};

export type OverlayOp =
  | { op: 'update'; model: ModelName; id: ID; patch: Record<string, unknown> }
  | { op: 'insert'; model: ModelName; id: ID; row: Record<string, unknown> }
  | { op: 'delete'; model: ModelName; id: ID };

/** Typed constructors (keep call sites type-checked). */
export const ops = {
  update<M extends ModelName>(model: M, id: ID, patch: Patch<ModelMap[M]>): OverlayOp {
    return { op: 'update', model, id, patch };
  },
  insert<M extends ModelName>(model: M, row: ModelMap[M]): OverlayOp {
    return { op: 'insert', model, id: row.id, row: row as unknown as Record<string, unknown> };
  },
  delete(model: ModelName, id: ID): OverlayOp {
    return { op: 'delete', model, id };
  },
};

function isArrayPatch(v: unknown): v is ArrayPatch<unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v) && ('$add' in v || '$remove' in v);
}

function isObjectPatch(v: unknown): v is ObjectPatch<unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v) && '$merge' in v;
}

/** Apply a patch to a row (returns a new object; never mutates). */
export function applyPatch<R extends Record<string, unknown>>(row: R, patch: Record<string, unknown>): R {
  const out: Record<string, unknown> = { ...row };
  for (const [k, v] of Object.entries(patch)) {
    if (isArrayPatch(v)) {
      const cur = Array.isArray(out[k]) ? (out[k] as unknown[]) : [];
      const remove = new Set(v.$remove ?? []);
      const next = cur.filter((x) => !remove.has(x));
      for (const a of v.$add ?? []) if (!next.includes(a)) next.push(a);
      out[k] = next;
    } else if (isObjectPatch(v)) {
      const cur = out[k] && typeof out[k] === 'object' ? { ...(out[k] as Record<string, unknown>) } : {};
      for (const [mk, mv] of Object.entries(v.$merge)) {
        if (mv === null) delete cur[mk];
        else cur[mk] = mv;
      }
      out[k] = cur;
    } else {
      out[k] = v;
    }
  }
  return out as R;
}

/** Compute the visible row: base ⊕ ops (in order). `undefined` = row doesn't exist. */
export function applyOps(
  base: Record<string, unknown> | undefined,
  opsForRow: readonly OverlayOp[],
): Record<string, unknown> | undefined {
  let row = base;
  for (const o of opsForRow) {
    if (o.op === 'insert') row = { ...o.row };
    else if (o.op === 'delete') row = undefined;
    else if (row) row = applyPatch(row, o.patch);
  }
  return row;
}

let tempCounter = 0;
/** Temporary negative id for optimistic inserts (unique per session). */
export function tempId(): ID {
  tempCounter += 1;
  return -(Date.now() * 1000 + (tempCounter % 1000));
}

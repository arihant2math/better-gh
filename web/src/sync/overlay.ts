import type { ID, ModelMap, ModelName } from './models';

/** Array patch that composes with concurrent edits: `{ $add: [3], $remove: [5] }`. */
export interface ArrayPatch<T> {
  $add?: T[];
  $remove?: T[];
}

export type Patch<R> = { [K in keyof R]?: R[K] | (R[K] extends (infer E)[] ? ArrayPatch<E> : never) };

export type OverlayOp =
  | { op: 'update'; model: ModelName; id: ID; patch: Record<string, unknown> }
  | { op: 'insert'; model: ModelName; id: ID; row: Record<string, unknown> }
  | { op: 'delete'; model: ModelName; id: ID };

/** Typed constructors (keep call sites type-checked). */
export const ops = {
  update<M extends ModelName>(model: M, id: ID, patch: Patch<ModelMap[M]>): OverlayOp {
    return { op: 'update', model, id, patch: patch as Record<string, unknown> };
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

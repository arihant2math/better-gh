/** Drag-and-drop helpers shared by the table and the board (HTML5 DnD). */
import type { ID, ProjectItem } from '../../sync/models';
import { itemOrderKey, keyBetween } from '../../sync/projects';

export const DND_TYPE = 'application/x-bgh-project-item';

/** Key that places an item at `index` of `list` (which must not contain the moved item). */
export function keyForIndex(list: readonly Pick<ProjectItem, 'position' | 'viewPositions'>[], index: number, viewId: ID): string {
  const before = index > 0 ? itemOrderKey(list[index - 1]!, viewId) : null;
  const after = index < list.length ? itemOrderKey(list[index]!, viewId) : null;
  return keyBetween(before, after);
}

/** Is the pointer in the lower half of `el`? */
export function isLowerHalf(e: { clientY: number }, el: Element): boolean {
  const r = el.getBoundingClientRect();
  return e.clientY > r.top + r.height / 2;
}

let dragging: ID | null = null;

export function setDragging(id: ID | null): void {
  dragging = id;
}

/** Id of the item being dragged in this tab (dataTransfer is unreadable during dragover). */
export function draggingId(): ID | null {
  return dragging;
}

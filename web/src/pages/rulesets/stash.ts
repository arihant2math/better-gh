import type { RulesetForm } from './model';

/** An imported ruleset waiting for the "new ruleset" editor (cleared once saved). */
let pending: RulesetForm | null = null;

export function setPendingImport(f: RulesetForm): void {
  pending = f;
}

export function pendingImport(): RulesetForm | null {
  return pending;
}

export function clearPendingImport(): void {
  pending = null;
}

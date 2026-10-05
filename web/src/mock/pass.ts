/**
 * Returned by a mock route handler to defer to the next matching route
 * (several mock modules serve the same REST path for different fake data,
 * e.g. the mock git backend of the code tab vs. the generated pull request
 * branches of mock/pulls.ts).
 */
export const PASS_STATUS = 0;

export function pass(): { status: number } {
  return { status: PASS_STATUS };
}

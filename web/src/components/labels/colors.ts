/** GitHub's label color presets (the color picker's swatches). */
export const LABEL_PRESETS = [
  'b60205', 'd93f0b', 'fbca04', '0e8a16', '006b75', '1d76db', '0052cc', '5319e7',
  'e99695', 'f9d0c4', 'fef2c0', 'c2e0c6', 'bfdadc', 'c5def5', 'bfd4f2', 'd4c5f9',
];

export function randomLabelColor(): string {
  return LABEL_PRESETS[Math.floor(Math.random() * LABEL_PRESETS.length)]!;
}

/** Normalize user input (`#AbC`, `aabbcc`) to 6 lowercase hex digits, or null. */
export function normalizeColor(input: string): string | null {
  let v = input.trim().replace(/^#/, '').toLowerCase();
  if (/^[0-9a-f]{3}$/.test(v)) v = v.replace(/./g, (c) => c + c);
  return /^[0-9a-f]{6}$/.test(v) ? v : null;
}

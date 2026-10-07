/**
 * Pixel position of a caret inside a textarea (relative to the textarea's
 * border box), via an off-screen mirror element with the same text metrics.
 */
const PROPS = [
  'boxSizing', 'width', 'height', 'overflowX', 'overflowY', 'borderTopWidth', 'borderRightWidth', 'borderBottomWidth', 'borderLeftWidth',
  'paddingTop', 'paddingRight', 'paddingBottom', 'paddingLeft', 'fontStyle', 'fontVariant', 'fontWeight', 'fontStretch', 'fontSize',
  'lineHeight', 'fontFamily', 'textAlign', 'textTransform', 'textIndent', 'letterSpacing', 'wordSpacing', 'tabSize',
] as const;

export function caretCoordinates(el: HTMLTextAreaElement, position: number): { top: number; left: number; height: number } {
  const div = document.createElement('div');
  const style = div.style;
  const computed = getComputedStyle(el);
  style.position = 'absolute';
  style.visibility = 'hidden';
  style.whiteSpace = 'pre-wrap';
  style.overflowWrap = 'break-word';
  for (const p of PROPS) style[p] = computed[p];
  div.textContent = el.value.slice(0, position);
  const span = document.createElement('span');
  span.textContent = el.value.slice(position) || '.';
  div.appendChild(span);
  document.body.appendChild(div);
  const lineHeight = parseFloat(computed.lineHeight) || parseFloat(computed.fontSize) * 1.4;
  const top = span.offsetTop + parseFloat(computed.borderTopWidth) - el.scrollTop;
  const left = span.offsetLeft + parseFloat(computed.borderLeftWidth) - el.scrollLeft;
  document.body.removeChild(div);
  return { top, left, height: lineHeight };
}

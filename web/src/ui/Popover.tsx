import { useEffect, useLayoutEffect, useRef, type CSSProperties, type ReactNode, type RefObject } from 'react';
import { cx } from './Button';
import styles from './Overlay.module.css';

export type Placement = 'bottom-start' | 'bottom-end' | 'top-start' | 'top-end' | 'top' | 'bottom' | 'right-start';

/** Position a fixed element next to an anchor rect, flipping to stay in the viewport. */
export function placeElement(el: HTMLElement, anchor: DOMRect, placement: Placement, gap = 6): void {
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const w = el.offsetWidth;
  const h = el.offsetHeight;
  const [initialSide, align] = placement.split('-') as [string, string | undefined];
  let side = initialSide;
  if (side === 'bottom' && anchor.bottom + gap + h > vh - 8 && anchor.top - gap - h > 8) side = 'top';
  else if (side === 'top' && anchor.top - gap - h < 8 && anchor.bottom + gap + h < vh - 8) side = 'bottom';
  let top: number;
  let left: number;
  if (side === 'right') {
    left = anchor.right + gap;
    top = anchor.top;
  } else {
    top = side === 'bottom' ? anchor.bottom + gap : anchor.top - gap - h;
    left = align === 'start' ? anchor.left : align === 'end' ? anchor.right - w : anchor.left + anchor.width / 2 - w / 2;
  }
  left = Math.max(8, Math.min(left, vw - w - 8));
  top = Math.max(8, Math.min(top, vh - h - 8));
  el.style.left = `${Math.round(left)}px`;
  el.style.top = `${Math.round(top)}px`;
}

export interface PopoverProps {
  open: boolean;
  onClose: () => void;
  anchor: RefObject<HTMLElement | null>;
  placement?: Placement;
  className?: string;
  style?: CSSProperties;
  children: ReactNode;
  /** Return focus to the anchor when closing (default true). */
  restoreFocus?: boolean;
  role?: string;
  'aria-label'?: string;
}

/**
 * Anchored popover rendered in the browser's top layer (`popover="manual"`),
 * with outside-click and Escape dismissal. Children mount only while open.
 */
export function Popover({ open, onClose, anchor, placement = 'bottom-start', className, style, children, restoreFocus = true, role, ...aria }: PopoverProps) {
  const ref = useRef<HTMLDivElement>(null);
  const onCloseRef = useRef(onClose);
  useLayoutEffect(() => {
    onCloseRef.current = onClose;
  });

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (open) {
      if (!el.matches(':popover-open')) el.showPopover();
      const a = anchor.current;
      if (a) placeElement(el, a.getBoundingClientRect(), placement);
    } else if (el.matches(':popover-open')) {
      el.hidePopover();
    }
  }, [open, anchor, placement]);

  useEffect(() => {
    if (!open) return;
    const el = ref.current;
    const a = anchor.current;
    const reposition = () => {
      if (el && a) placeElement(el, a.getBoundingClientRect(), placement);
    };
    const onPointerDown = (e: PointerEvent) => {
      const t = e.target as Node;
      if (el?.contains(t) || a?.contains(t)) return;
      onCloseRef.current();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        e.preventDefault();
        onCloseRef.current();
      }
    };
    const ro = new ResizeObserver(reposition);
    if (el) ro.observe(el);
    window.addEventListener('resize', reposition);
    window.addEventListener('scroll', reposition, true);
    document.addEventListener('pointerdown', onPointerDown, true);
    document.addEventListener('keydown', onKey, true);
    return () => {
      ro.disconnect();
      window.removeEventListener('resize', reposition);
      window.removeEventListener('scroll', reposition, true);
      document.removeEventListener('pointerdown', onPointerDown, true);
      document.removeEventListener('keydown', onKey, true);
      if (restoreFocus && el?.contains(document.activeElement)) a?.focus();
    };
  }, [open, anchor, placement, restoreFocus]);

  return (
    <div ref={ref} popover="manual" className={cx(styles.popover, className)} style={style} role={role} {...aria}>
      {open ? children : null}
    </div>
  );
}

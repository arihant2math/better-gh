import { useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import styles from './Overlay.module.css';
import { placeElement, type Placement } from './Popover';

const DELAY = 450;
/** Once one tooltip was shown, neighbours show instantly (like native toolbars). */
let warmUntil = 0;

export function Tooltip({
  label,
  shortcut,
  placement = 'bottom',
  children,
}: {
  label: ReactNode;
  shortcut?: string;
  placement?: Placement;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLSpanElement>(null);
  const tip = useRef<HTMLDivElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const id = useId();

  const show = () => {
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setOpen(true), Date.now() < warmUntil ? 0 : DELAY);
  };
  const hide = () => {
    clearTimeout(timer.current);
    if (open) warmUntil = Date.now() + 400;
    setOpen(false);
  };

  useLayoutEffect(() => {
    const el = tip.current;
    if (!el || !open) return;
    el.showPopover();
    const target = anchor.current?.firstElementChild ?? anchor.current;
    if (target) placeElement(el, target.getBoundingClientRect(), placement, 6);
    return () => {
      if (el.matches(':popover-open')) el.hidePopover();
    };
  }, [open, placement]);

  useEffect(() => () => clearTimeout(timer.current), []);

  return (
    <span
      ref={anchor}
      className={styles.anchor}
      onPointerEnter={show}
      onPointerLeave={hide}
      onPointerDown={hide}
      onFocus={(e) => {
        if (e.target.matches(':focus-visible')) show();
      }}
      onBlur={hide}
      aria-describedby={open ? id : undefined}
    >
      {children}
      {open && (
        <div ref={tip} id={id} role="tooltip" popover="manual" className={styles.tooltip}>
          {label}
          {shortcut && <span className={styles.tooltipKbd}>{shortcut}</span>}
        </div>
      )}
    </span>
  );
}

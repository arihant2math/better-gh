/**
 * Hand-rolled SVG/HTML charts for the admin dashboard (no chart library).
 * Conventions (dataviz guidance): thin marks, 2px lines, 4px rounded
 * data-ends, 2px surface gaps between touching segments, text in text
 * tokens (never the series color), a legend for >= 2 series, a hover
 * tooltip on every mark, and categorical slots `--chart-1..3` in fixed order.
 */
import { useId, useState, type ReactNode } from 'react';
import { Link } from '../../router';
import { cx } from '../../ui/Button';
import styles from './charts.module.css';

// ------------------------------------------------------------------ stat tile

export function StatTile({ label, value, sub, trend, href }: { label: string; value: ReactNode; sub?: ReactNode; trend?: ReactNode; href?: string }) {
  const body = (
    <>
      <div className={styles.tileLabel}>{label}</div>
      <div className={styles.tileValue}>{value}</div>
      {sub && <div className={styles.tileSub}>{sub}</div>}
      {trend && <div className={styles.tileTrend}>{trend}</div>}
    </>
  );
  return href ? (
    <Link className={cx(styles.tile, styles.tileLink)} to={href}>
      {body}
    </Link>
  ) : (
    <div className={styles.tile}>{body}</div>
  );
}

// ------------------------------------------------------------------ sparkline

/**
 * Single-series trend line (de-emphasis ink; the latest point in the accent
 * with a surface ring). Hover shows the value at the nearest sample.
 */
export function Sparkline({
  values,
  width = 160,
  height = 36,
  format = String,
  label,
  min: minProp,
}: {
  values: { t: number; v: number }[];
  width?: number;
  height?: number;
  format?: (v: number) => string;
  label: string;
  /** Lower bound of the y-domain (default 0). */
  min?: number;
}) {
  const [hover, setHover] = useState<number | null>(null);
  const pad = 5;
  if (values.length === 0) return <div className={styles.sparkEmpty} style={{ width, height }} aria-label={`${label}: no samples yet`} />;
  const min = minProp ?? 0;
  const max = Math.max(min + 1e-9, ...values.map((p) => p.v));
  const x = (i: number) => (values.length === 1 ? width - pad : pad + (i * (width - 2 * pad)) / (values.length - 1));
  const y = (v: number) => height - pad - ((v - min) / (max - min)) * (height - 2 * pad);
  const d = values.map((p, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(p.v).toFixed(1)}`).join('');
  const last = values.length - 1;
  const h = hover ?? last;
  const current = values[h]!;
  return (
    <div className={styles.spark} style={{ width }}>
      <svg
        width={width}
        height={height}
        role="img"
        aria-label={`${label}: ${values.length} samples, latest ${format(values[last]!.v)}`}
        onPointerMove={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          const rel = ((e.clientX - r.left) / r.width) * width;
          const i = Math.round(((rel - pad) / (width - 2 * pad)) * last);
          setHover(Math.max(0, Math.min(last, i)));
        }}
        onPointerLeave={() => setHover(null)}
      >
        <line x1={0} x2={width} y1={height - pad} y2={height - pad} className={styles.baseline} />
        <path d={d} className={styles.sparkLine} />
        {hover !== null && <line x1={x(h)} x2={x(h)} y1={0} y2={height} className={styles.crosshair} />}
        <circle cx={x(h)} cy={y(current.v)} r={4} className={styles.sparkDot} />
      </svg>
      {hover !== null && (
        <div className={styles.tooltip} style={{ left: Math.min(Math.max(x(h), 40), width - 40) }} role="tooltip">
          <strong>{format(current.v)}</strong>
          <span>{new Date(current.t).toLocaleTimeString()}</span>
        </div>
      )}
    </div>
  );
}

// ------------------------------------------------------------------ meter

export type Severity = 'ok' | 'warning' | 'critical';

/** Ratio against a limit; fill colour carries severity, track is the same hue lighter. */
export function Meter({ value, max, label, severity = 'ok', detail }: { value: number; max: number; label: string; severity?: Severity; detail?: ReactNode }) {
  const pct = max > 0 ? Math.max(0, Math.min(1, value / max)) : 0;
  return (
    <div className={styles.meterWrap}>
      <div className={styles.meterHead}>
        <span>{label}</span>
        <span className={styles.meterValue}>{Math.round(pct * 100)}%</span>
      </div>
      <div
        className={styles.meter}
        data-severity={severity}
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={max}
        aria-valuenow={value}
        title={`${Math.round(pct * 1000) / 10}%`}
      >
        <div className={styles.meterFill} style={{ width: `${pct * 100}%` }} />
      </div>
      {detail && <div className={styles.meterDetail}>{detail}</div>}
    </div>
  );
}

// ------------------------------------------------------------------ stacked bar

export interface Segment {
  label: string;
  value: number;
  /** Categorical slot 1-3, or `muted` for the remainder. */
  slot: 1 | 2 | 3 | 'muted';
}

/** Part-to-whole: one horizontal stacked bar + legend with values. */
export function StackedBar({ segments, format = (n: number) => n.toLocaleString(), label }: { segments: Segment[]; format?: (n: number) => string; label: string }) {
  const [hover, setHover] = useState<number | null>(null);
  const total = segments.reduce((s, x) => s + Math.max(0, x.value), 0);
  const id = useId();
  return (
    <figure className={styles.stack} aria-labelledby={id}>
      <figcaption id={id} className={styles.srOnly}>
        {label}: {segments.map((s) => `${s.label} ${format(s.value)}`).join(', ')}
      </figcaption>
      <div className={styles.stackBar} aria-hidden onPointerLeave={() => setHover(null)}>
        {total === 0 ? (
          <div className={styles.stackEmpty} />
        ) : (
          segments
            .filter((s) => s.value > 0)
            .map((s) => {
              const i = segments.indexOf(s);
              return (
                <div
                  key={s.label}
                  className={cx(styles.stackSeg, hover !== null && hover !== i && styles.dim)}
                  data-slot={s.slot}
                  style={{ flexGrow: s.value }}
                  onPointerEnter={() => setHover(i)}
                />
              );
            })
        )}
        {hover !== null && segments[hover] && (
          <div className={styles.tooltip} role="tooltip" style={{ left: '50%' }}>
            <strong>{format(segments[hover].value)}</strong>
            <span>
              {segments[hover].label} · {total ? Math.round((segments[hover].value / total) * 100) : 0}%
            </span>
          </div>
        )}
      </div>
      <ul className={styles.legend}>
        {segments.map((s, i) => (
          <li key={s.label} onPointerEnter={() => setHover(i)} onPointerLeave={() => setHover(null)}>
            <span className={styles.swatch} data-slot={s.slot} />
            <span className={styles.legendLabel}>{s.label}</span>
            <span className={styles.legendValue}>{format(s.value)}</span>
          </li>
        ))}
      </ul>
    </figure>
  );
}

// ------------------------------------------------------------------ bar list

/** Magnitude comparison: labelled horizontal bars in one hue, value at the tip. */
export function BarList({
  items,
  format = (n: number) => n.toLocaleString(),
  label,
  max: maxProp,
}: {
  items: { label: ReactNode; key: string; value: number; title?: string }[];
  format?: (n: number) => string;
  label: string;
  max?: number;
}) {
  const max = maxProp ?? Math.max(1, ...items.map((i) => i.value));
  return (
    <ul className={styles.barList} aria-label={label}>
      {items.map((it) => (
        <li key={it.key} className={styles.barRow} title={it.title ?? `${it.key}: ${format(it.value)}`}>
          <span className={styles.barLabel}>{it.label}</span>
          <span className={styles.barTrack}>
            <span className={styles.bar} style={{ width: `${Math.max(it.value > 0 ? 1.5 : 0, (it.value / max) * 100)}%` }} />
          </span>
          <span className={styles.barValue}>{format(it.value)}</span>
        </li>
      ))}
    </ul>
  );
}

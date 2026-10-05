/**
 * Lightweight SVG charts of the Insights pages (P31), following the
 * dataviz conventions of `components/admin/charts.tsx`: 2px lines, thin
 * bars with 2px gaps, text in text tokens (never the series colour),
 * categorical slots `--chart-1..3` in fixed order, a legend for >= 2
 * series, gridlines in `--chart-grid`, and a crosshair tooltip on hover.
 */
import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { compact, niceMax } from './data';
import styles from './Insights.module.css';

export interface Series {
  label: string;
  slot: 1 | 2 | 3;
  values: number[];
}

function useWidth<T extends HTMLElement>(): [React.RefObject<T | null>, number] {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(el.clientWidth);
    if (typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, width];
}

/**
 * Time series as areas (`kind="area"`, values may be negative) or grouped
 * columns (`kind="bar"`). `times` are labels for the x axis (one per value).
 */
export function TimeChart({
  times,
  series,
  kind: kindProp = 'area',
  height = 180,
  label,
  formatTime,
  format = (n: number) => n.toLocaleString(),
}: {
  times: number[];
  series: Series[];
  kind?: 'area' | 'bar';
  height?: number;
  label: string;
  formatTime: (t: number) => string;
  format?: (n: number) => string;
}) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [hover, setHover] = useState<number | null>(null);
  const id = useId();
  const n = times.length;
  // An area needs at least two points: short series render as columns.
  const kind = n < 3 ? 'bar' : kindProp;
  const left = 40;
  const right = 8;
  const top = 8;
  const bottom = 22;
  const all = series.flatMap((s) => s.values);
  const maxV = niceMax(Math.max(0, ...all));
  const minV = Math.min(0, ...all) < 0 ? -niceMax(-Math.min(0, ...all)) : 0;
  const plotW = Math.max(1, width - left - right);
  const plotH = height - top - bottom;
  const y = (v: number) => top + ((maxV - v) / (maxV - minV || 1)) * plotH;
  const step = n > 0 ? plotW / n : plotW;
  const xc = (i: number) => left + step * (i + 0.5);
  const ticks = minV < 0 ? [maxV, 0, minV] : [maxV, maxV / 2, 0];
  const labelEvery = Math.max(1, Math.ceil(n / Math.max(1, Math.floor(plotW / 80))));
  const summary = series.map((s) => `${s.label} total ${format(s.values.reduce((a, b) => a + b, 0))}`).join(', ');
  return (
    <figure className={styles.chart} aria-labelledby={id}>
      <figcaption id={id} className={styles.srOnly}>
        {label}: {n} points. {summary}
      </figcaption>
      <div ref={ref} className={styles.chartArea} style={{ height }}>
        {width > 0 && n > 0 && (
          <svg
            width={width}
            height={height}
            aria-hidden
            onPointerMove={(e) => {
              const r = e.currentTarget.getBoundingClientRect();
              const i = Math.floor((e.clientX - r.left - left) / step);
              setHover(i >= 0 && i < n ? i : null);
            }}
            onPointerLeave={() => setHover(null)}
          >
            {ticks.map((t) => (
              <g key={t}>
                <line x1={left} x2={width - right} y1={y(t)} y2={y(t)} className={t === 0 ? styles.axis : styles.grid} />
                <text x={left - 6} y={y(t)} dy="0.32em" textAnchor="end" className={styles.tick}>
                  {compact(t)}
                </text>
              </g>
            ))}
            {times.map((t, i) =>
              i % labelEvery === 0 ? (
                <text key={t} x={xc(i)} y={height - 6} textAnchor="middle" className={styles.tick}>
                  {formatTime(t)}
                </text>
              ) : null,
            )}
            {kind === 'bar'
              ? series.map((s, si) => {
                  const bw = Math.max(1, (step - 2) / series.length - (series.length > 1 ? 1 : 0));
                  return s.values.map((v, i) =>
                    v === 0 ? null : (
                      <rect
                        key={`${si}:${i}`}
                        x={left + step * i + 1 + si * (bw + 1)}
                        y={Math.min(y(v), y(0))}
                        width={bw}
                        height={Math.max(1, Math.abs(y(v) - y(0)))}
                        rx={Math.min(2, bw / 2)}
                        className={styles.bar}
                        data-slot={s.slot}
                        data-dim={hover !== null && hover !== i ? '' : undefined}
                      />
                    ),
                  );
                })
              : series.map((s) => {
                  const pts = s.values.map((v, i) => `${xc(i).toFixed(1)},${y(v).toFixed(1)}`);
                  const line = `M${pts.join('L')}`;
                  const area = `${line}L${xc(n - 1).toFixed(1)},${y(0).toFixed(1)}L${xc(0).toFixed(1)},${y(0).toFixed(1)}Z`;
                  return (
                    <g key={s.label}>
                      <path d={area} className={styles.area} data-slot={s.slot} />
                      <path d={line} className={styles.line} data-slot={s.slot} />
                    </g>
                  );
                })}
            {hover !== null && <line x1={xc(hover)} x2={xc(hover)} y1={top} y2={top + plotH} className={styles.crosshair} />}
          </svg>
        )}
        {hover !== null && width > 0 && (
          <div className={styles.tooltip} style={{ left: Math.min(Math.max(xc(hover), 70), width - 70) }} role="tooltip">
            <span>{formatTime(times[hover]!)}</span>
            {series.map((s) => (
              <span key={s.label} className={styles.tipRow}>
                <span className={styles.swatch} data-slot={s.slot} />
                {s.label}
                <strong>{format(s.values[hover] ?? 0)}</strong>
              </span>
            ))}
          </div>
        )}
      </div>
      {series.length > 1 && <Legend items={series.map((s) => ({ label: s.label, slot: s.slot }))} />}
    </figure>
  );
}

export function Legend({ items }: { items: { label: ReactNode; slot: 1 | 2 | 3 }[] }) {
  return (
    <ul className={styles.legend}>
      {items.map((it, i) => (
        <li key={i}>
          <span className={styles.swatch} data-slot={it.slot} />
          {it.label}
        </li>
      ))}
    </ul>
  );
}

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

/** Commits by weekday and hour: circle area ∝ count. */
export function PunchCard({ data }: { data: [number, number, number][] }) {
  const [ref, width] = useWidth<HTMLDivElement>();
  const [hover, setHover] = useState<[number, number, number] | null>(null);
  const max = Math.max(1, ...data.map((d) => d[2]));
  const left = 36;
  const cell = Math.max(10, Math.min(28, (width - left) / 24));
  const height = cell * 7 + 20;
  return (
    <figure className={styles.chart}>
      <figcaption className={styles.srOnly}>Commits by day of week and hour; busiest {max} commits.</figcaption>
      <div ref={ref} className={styles.chartArea} style={{ height }}>
        {width > 0 && (
          <svg width={width} height={height} aria-hidden onPointerLeave={() => setHover(null)}>
            {DAYS.map((d, i) => (
              <text key={d} x={left - 8} y={i * cell + cell / 2} dy="0.32em" textAnchor="end" className={styles.tick}>
                {d}
              </text>
            ))}
            {[0, 6, 12, 18].map((h) => (
              <text key={h} x={left + h * cell + cell / 2} y={height - 4} textAnchor="middle" className={styles.tick}>
                {h === 0 ? '12a' : h === 12 ? '12p' : h < 12 ? `${h}a` : `${h - 12}p`}
              </text>
            ))}
            {data.map(([d, h, c]) => (
              <circle
                key={`${d}:${h}`}
                cx={left + h * cell + cell / 2}
                cy={d * cell + cell / 2}
                r={c ? Math.max(1.5, (Math.sqrt(c / max) * cell) / 2 - 1) : 1}
                className={c ? styles.dot : styles.dotEmpty}
                onPointerEnter={() => setHover([d, h, c])}
              />
            ))}
          </svg>
        )}
        {hover && (
          <div className={styles.tooltip} style={{ left: Math.min(Math.max(left + hover[1] * cell, 70), width - 70), top: Math.max(0, hover[0] * cell - 40) }} role="tooltip">
            <strong>
              {hover[2]} commit{hover[2] === 1 ? '' : 's'}
            </strong>
            <span>
              {DAYS[hover[0]]} {String(hover[1]).padStart(2, '0')}:00
            </span>
          </div>
        )}
      </div>
    </figure>
  );
}

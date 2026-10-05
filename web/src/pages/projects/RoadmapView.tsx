import { observer } from 'mobx-react-lite';
import type { CSSProperties } from 'react';
import type { ProjectField, ProjectView } from '../../sync/models';
import { cx } from '../../ui/Button';
import { EmptyState } from '../../ui/EmptyState';
import { CalendarIcon } from '../../ui/icons';
import { TitleContent } from './FieldCell';
import { addDays, cellOf, iterationById, optionHex, todayYmd, type ItemRow } from './fields';
import styles from './Projects.module.css';

const DAY = 86_400_000;
const ms = (ymd: string) => Date.parse(`${ymd}T00:00:00Z`);

interface Span {
  row: ItemRow;
  start: string;
  end: string;
}

export function roadmapField(view: ProjectView, fields: ProjectField[]): ProjectField | undefined {
  return fields.find((f) => f.id === view.dateFieldId) ?? fields.find((f) => f.dataType === 'iteration') ?? fields.find((f) => f.dataType === 'date');
}

/** Minimal roadmap: one bar per item on a time axis (iteration span or a date). */
export const RoadmapView = observer(function RoadmapView({
  view,
  fields,
  rows,
  activeId,
  onOpen,
}: {
  view: ProjectView;
  fields: ProjectField[];
  rows: ItemRow[];
  activeId: number | null;
  onOpen: (id: number) => void;
}) {
  const field = roadmapField(view, fields);
  if (!field) {
    return (
      <EmptyState icon={CalendarIcon} title="No date or iteration field">
        Add a date or iteration field to plan items on a roadmap.
      </EmptyState>
    );
  }
  const status = fields.find((f) => f.dataType === 'status');
  const spans: Span[] = [];
  const undated: ItemRow[] = [];
  for (const row of rows) {
    const v = row.item.values[String(field.id)];
    if (field.dataType === 'iteration') {
      const it = iterationById(field, v);
      if (it) spans.push({ row, start: it.startDate, end: addDays(it.startDate, it.duration) });
      else undated.push(row);
    } else if (typeof v === 'string' && v) spans.push({ row, start: v, end: addDays(v, 1) });
    else undated.push(row);
  }
  spans.sort((a, b) => (a.start < b.start ? -1 : a.start > b.start ? 1 : 0));

  const today = todayYmd();
  const marks: { label: string; start: string; end: string }[] =
    field.dataType === 'iteration'
      ? (field.iterations?.iterations ?? []).map((it) => ({ label: it.title, start: it.startDate, end: addDays(it.startDate, it.duration) }))
      : [];
  const allStarts = [...spans.map((s) => s.start), ...marks.map((m) => m.start), today];
  const allEnds = [...spans.map((s) => s.end), ...marks.map((m) => m.end), addDays(today, 1)];
  const from = ms(addDays(allStarts.sort()[0]!, -3));
  const to = ms(addDays(allEnds.sort().at(-1)!, 3));
  const total = Math.max(DAY, to - from);
  const pct = (ymd: string) => ((ms(ymd) - from) / total) * 100;
  if (marks.length === 0) {
    // Week ticks for date fields.
    for (let t = from; t < to; t += 7 * DAY) {
      const d = new Date(t).toISOString().slice(0, 10);
      marks.push({ label: new Date(t).toLocaleDateString(undefined, { month: 'short', day: 'numeric', timeZone: 'UTC' }), start: d, end: addDays(d, 7) });
    }
  }

  return (
    <div className={styles.roadmap}>
      <div className={styles.rmHead}>
        <div className={styles.rmLabel}>{field.name}</div>
        <div className={styles.rmTrack}>
          {marks.map((m) => (
            <div key={m.start + m.label} className={styles.rmMark} style={{ left: `${pct(m.start)}%`, width: `${pct(m.end) - pct(m.start)}%` }}>
              {m.label}
            </div>
          ))}
          <div className={styles.rmToday} style={{ left: `${pct(today)}%` }} title="Today" />
        </div>
      </div>
      <div className={styles.rmBody}>
        {spans.map((s) => {
          const color = status ? optionHex(status.options?.find((o) => o.id === s.row.item.values[String(status.id)])?.color) : undefined;
          return (
            <div key={s.row.item.id} className={cx(styles.rmRow, s.row.item.id === activeId && styles.trActive)}>
              <div className={styles.rmLabel}>
                <TitleContent row={s.row} onOpen={() => onOpen(s.row.item.id)} />
              </div>
              <div className={styles.rmTrack}>
                <div className={styles.rmToday} style={{ left: `${pct(today)}%` }} />
                <button
                  type="button"
                  className={styles.rmBar}
                  style={{ left: `${pct(s.start)}%`, width: `max(10px, ${pct(s.end) - pct(s.start)}%)`, '--c': `#${color ?? '8b949e'}` } as CSSProperties}
                  title={`${s.row.title} · ${cellOf(s.row, field).text}`}
                  onClick={() => onOpen(s.row.item.id)}
                >
                  <span>{s.row.title}</span>
                </button>
              </div>
            </div>
          );
        })}
        {undated.length > 0 && (
          <>
            <div className={styles.rmSection}>
              No {field.name} · {undated.length}
            </div>
            {undated.map((r) => (
              <div key={r.item.id} className={styles.rmRow}>
                <div className={styles.rmLabel}>
                  <TitleContent row={r} onOpen={() => onOpen(r.item.id)} />
                </div>
                <div className={styles.rmTrack} />
              </div>
            ))}
          </>
        )}
      </div>
    </div>
  );
});

import { useMemo } from 'react';
import { useResource } from '../../api/cache';
import { fetchRaw } from '../../api/code';
import type { BlobView } from '../../api/types';
import { Skeleton } from '../../ui/EmptyState';
import styles from './Code.module.css';
import { parseCsv } from './csv';

const MAX_ROWS = 1000;

/** Rendered CSV/TSV (lazy chunk). */
export default function CsvTable({ blob }: { blob: BlobView }) {
  const { data, error } = useResource<string>(`raw:${blob.sha}`, () => fetchRaw(blob.raw_url), { immutable: true });
  const rows = useMemo(() => (data === undefined ? null : parseCsv(data, blob.path.toLowerCase().endsWith('.tsv') ? '\t' : ',')), [data, blob.path]);
  if (error) return <div className={styles.notice}>Could not load this file.</div>;
  if (!rows) return <div className={styles.fileLoading}><Skeleton width="60%" /></div>;
  if (!rows.length) return <div className={styles.notice}>This file is empty.</div>;
  const [head, ...body] = rows;
  return (
    <div className={styles.csvWrap}>
      <table className={styles.csv}>
        <thead>
          <tr>
            <th aria-label="Row" />
            {head!.map((h, i) => (
              <th key={i}>{h}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {body.slice(0, MAX_ROWS).map((r, i) => (
            <tr key={i}>
              <td className={styles.csvNum}>{i + 1}</td>
              {r.map((c, j) => (
                <td key={j}>{c}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {body.length > MAX_ROWS && <div className={styles.notice}>Showing the first {MAX_ROWS} of {body.length} rows.</div>}
    </div>
  );
}

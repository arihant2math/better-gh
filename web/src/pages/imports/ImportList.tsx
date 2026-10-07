import { IMPORT_STEPS, isActive, type MetadataImport } from '../../api/metadataImports';
import { StatusPill } from '../../components/admin/kit';
import { Link } from '../../router';
import { RelativeTime } from '../../ui/RelativeTime';
import { STATUS_PILL } from './ImportDetail';
import s from './imports.module.css';

/** Imports, newest first; each links to `detailPath(id)`. */
export function ImportList({ rows, detailPath }: { rows: MetadataImport[]; detailPath: (id: number) => string }) {
  return (
    <ul className={s.list} aria-label="Imports">
      {rows.map((r) => {
        const [pill, label] = STATUS_PILL[r.status];
        const target = r.repository?.full_name ?? `${r.owner}/${r.repo_name}`;
        return (
          <li key={r.id} className={s.item}>
            <div className={s.itemMain}>
              <div className={s.itemTitle}>
                <Link to={detailPath(r.id)}>
                  {r.source_repo} → {target}
                </Link>
                <StatusPill status={pill}>{label}</StatusPill>
              </div>
              <div className={s.muted}>
                {new URL(r.api_url).host}
                {isActive(r.status) && ` · ${IMPORT_STEPS[r.step] ?? r.step}`}
                {r.stats.issues !== undefined && ` · ${r.stats.issues} issues`}
                {r.error && ` · ${r.error}`}
              </div>
            </div>
            <span className={s.muted}>
              <RelativeTime date={r.created_at} />
            </span>
          </li>
        );
      })}
    </ul>
  );
}

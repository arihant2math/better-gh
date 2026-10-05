import { observer } from 'mobx-react-lite';
import { useMemo } from 'react';
import type { RunGraph as Graph, WorkflowJob } from '../../api/actions';
import { Link } from '../../router';
import { cx } from '../../ui/Button';
import { layoutGraph, MAX_ROWS, type LayoutInput } from './graphLayout';
import { groupJobs } from './RunShell';
import { Duration, StatusIcon, visualStatus } from './shared';
import styles from './Run.module.css';

interface NodeData {
  key: string;
  name: string;
  jobs: WorkflowJob[];
  matrix: boolean;
}

/** Status of a group: failure wins, then running, then queued, else the shared conclusion. */
function groupStatus(jobs: WorkflowJob[]): { status: string; conclusion: string | null } {
  if (!jobs.length) return { status: 'queued', conclusion: null };
  const vs = jobs.map((j) => visualStatus(j.status, j.conclusion));
  if (vs.includes('failure')) return { status: 'completed', conclusion: 'failure' };
  if (vs.includes('in_progress')) return { status: 'in_progress', conclusion: null };
  if (vs.includes('queued') || vs.includes('waiting')) return { status: 'queued', conclusion: null };
  if (vs.includes('cancelled')) return { status: 'completed', conclusion: 'cancelled' };
  if (vs.every((v) => v === 'skipped')) return { status: 'completed', conclusion: 'skipped' };
  return { status: 'completed', conclusion: 'success' };
}

/**
 * The run's job DAG: HTML nodes over an SVG layer of `needs` edges. Jobs not
 * materialized yet (waiting on their needs) render as pending placeholders.
 */
export const RunGraph = observer(function RunGraph({ graph, jobs, runBase }: { graph: Graph | undefined; jobs: WorkflowJob[]; runBase: string }) {
  const nodes: NodeData[] = useMemo(() => {
    const groups = groupJobs(jobs, graph);
    if (!graph) return groups.map((g) => ({ ...g, matrix: g.jobs.length > 1 }));
    const byKey = new Map(groups.map((g) => [g.key, g]));
    const out: NodeData[] = graph.jobs.map((g) => ({ key: g.key, name: g.name, jobs: byKey.get(g.key)?.jobs ?? [], matrix: g.matrix || (byKey.get(g.key)?.jobs.length ?? 0) > 1 }));
    // Jobs the graph doesn't know (shouldn't happen) still show up.
    for (const g of groups) if (!graph.jobs.some((x) => x.key === g.key)) out.push({ ...g, matrix: g.jobs.length > 1 });
    return out;
  }, [graph, jobs]);

  const needs = useMemo(() => new Map(graph?.jobs.map((g) => [g.key, g.needs]) ?? []), [graph]);
  const layout = useMemo(() => {
    const input: LayoutInput[] = nodes.map((n) => ({ key: n.key, needs: needs.get(n.key) ?? [], rows: Math.max(1, n.jobs.length), group: n.matrix }));
    return layoutGraph(input);
  }, [nodes, needs]);

  if (!nodes.length) return null;
  const byKey = new Map(nodes.map((n) => [n.key, n]));
  const doneKeys = new Set(nodes.filter((n) => n.jobs.length && n.jobs.every((j) => j.status === 'completed')).map((n) => n.key));

  return (
    <div className={styles.graphScroll}>
      <div className={styles.graph} style={{ width: layout.width, height: layout.height }}>
        <svg className={styles.edges} width={layout.width} height={layout.height} aria-hidden>
          {layout.edges.map((e) => (
            <path key={`${e.from}>${e.to}`} d={e.d} className={cx(styles.edge, doneKeys.has(e.from) && styles.edgeDone)} />
          ))}
          {layout.edges.map((e) => {
            const m = /^M([\d.]+),([\d.]+).* ([\d.]+),([\d.]+)$/.exec(e.d);
            return m ? (
              <g key={`dots-${e.from}>${e.to}`} className={styles.edgeDot}>
                <circle cx={Number(m[1])} cy={Number(m[2])} r={3} />
                <circle cx={Number(m[3])} cy={Number(m[4])} r={3} />
              </g>
            ) : null;
          })}
        </svg>
        {layout.nodes.map((ln) => {
          const n = byKey.get(ln.key)!;
          const style = { left: ln.x, top: ln.y, width: ln.w, height: ln.h };
          if (!n.matrix) {
            const j = n.jobs[0];
            return j ? (
              <Link key={ln.key} to={`${runBase}/job/${j.id}`} className={cx(styles.node, styles.nodeLink)} style={style}>
                <JobLine job={j} />
              </Link>
            ) : (
              <div key={ln.key} className={cx(styles.node, styles.nodePending)} style={style}>
                <StatusIcon status="pending" conclusion={null} />
                <span className={styles.nodeName}>{n.name}</span>
              </div>
            );
          }
          const st = groupStatus(n.jobs);
          return (
            <div key={ln.key} className={cx(styles.node, styles.nodeGroup, !n.jobs.length && styles.nodePending)} style={style}>
              <div className={styles.groupHead}>
                <StatusIcon status={st.status} conclusion={st.conclusion} size={14} />
                <span className={styles.nodeName}>{n.name}</span>
                <span className={styles.groupCount}>{n.jobs.length ? `${n.jobs.length} job${n.jobs.length === 1 ? '' : 's'}` : 'Matrix'}</span>
              </div>
              {n.jobs.slice(0, MAX_ROWS).map((j) => (
                <Link key={j.id} to={`${runBase}/job/${j.id}`} className={cx(styles.groupRow, styles.nodeLink)}>
                  <JobLine job={j} />
                </Link>
              ))}
              {n.jobs.length > MAX_ROWS && <div className={styles.groupMore}>and {n.jobs.length - MAX_ROWS} more…</div>}
            </div>
          );
        })}
      </div>
    </div>
  );
});

const JobLine = observer(function JobLine({ job }: { job: WorkflowJob }) {
  const running = job.status === 'in_progress';
  return (
    <>
      <StatusIcon status={job.status} conclusion={job.conclusion} />
      <span className={styles.nodeName} title={job.name}>
        {job.name}
      </span>
      {(job.status === 'completed' || running) && job.conclusion !== 'skipped' && <Duration start={job.started_at} end={job.completed_at} running={running} />}
    </>
  );
});

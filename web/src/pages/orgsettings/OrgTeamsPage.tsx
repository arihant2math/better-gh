import { useMemo, useState } from 'react';
import { refresh, useResource } from '../../api/cache';
import { DataTable, type Column } from '../../components/admin/DataTable';
import styles from '../../components/admin/admin.module.css';
import { formatCount, plural } from '../../components/admin/format';
import { PageHeader, SearchInput, errorMessage } from '../../components/admin/kit';
import { setQuery, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { PeopleIcon, PlusIcon } from '../../ui/icons';
import { allTeamsKey, getTeam, listAllTeams, teamKey } from './api';
import { TeamDialog, privacyPill, teamPath, teamTree, useAllTeams, useOrgAccess, type TreeRow } from './common';
import local from './OrgSettings.module.css';

/** Lazy member / repository counts from team-full (rendered rows only). */
function TeamCount({ org, slug, field }: { org: string; slug: string; field: 'members_count' | 'repos_count' }) {
  const res = useResource(teamKey(org, slug), () => getTeam(org, slug));
  if (res.data) return <>{formatCount(res.data[field])}</>;
  if (res.error) return <span className={styles.subtle}>—</span>;
  return <Skeleton width={24} />;
}

export default function OrgTeamsPage() {
  const { org = '' } = useParams<{ org: string }>();
  const query = useQuery();
  const q = query.get('q') ?? '';
  const access = useOrgAccess(org);
  const teams = useAllTeams(org);
  const [creating, setCreating] = useState(false);

  const rows = useMemo(() => {
    const tree = teamTree(teams.data ?? []);
    const needle = q.trim().toLowerCase();
    if (!needle) return tree;
    return tree.filter((r) => r.team.name.toLowerCase().includes(needle) || r.team.slug.includes(needle) || (r.team.description ?? '').toLowerCase().includes(needle));
  }, [teams.data, q]);

  const columns: Column<TreeRow>[] = [
    {
      id: 'team',
      header: 'Team',
      width: 'minmax(240px, 3fr)',
      render: (r) => (
        <span className={local.treeCell} style={{ paddingLeft: q ? 0 : r.depth * 20 }}>
          {!q && r.depth > 0 && <span className={local.treeElbow} aria-hidden />}
          <PeopleIcon size={16} className={styles.subtle} />
          <span className={styles.cellMain}>
            <strong>{q ? r.path : r.team.name}</strong>
            <span className={styles.subtle}>{r.team.description || ' '}</span>
          </span>
        </span>
      ),
    },
    { id: 'privacy', header: 'Visibility', width: '104px', render: (r) => privacyPill(r.team.privacy) },
    { id: 'members', header: 'Members', width: '80px', align: 'end', render: (r) => <TeamCount org={org} slug={r.team.slug} field="members_count" /> },
    { id: 'repos', header: 'Repos', width: '72px', align: 'end', hideBelow: 720, render: (r) => <TeamCount org={org} slug={r.team.slug} field="repos_count" /> },
  ];

  useShortcuts('Organization teams', {
    n: { handler: () => setCreating(true), description: 'New team', group: 'Organization' },
  });

  return (
    <div className={styles.fill}>
      <PageHeader
        title="Teams"
        description="Groups of members with cascading repository access and mentions. Child teams inherit their parent’s access."
        actions={
          <Button variant="primary" leadingIcon={PlusIcon} kbd="N" onClick={() => setCreating(true)}>
            New team
          </Button>
        }
      />
      <div className={styles.toolbar}>
        <SearchInput label="Find a team" placeholder="Find a team…" value={q} onChange={(v) => setQuery({ q: v || null })} />
        <span className={styles.toolbarSpacer} />
        <span className={styles.meta} aria-live="polite">
          {teams.data ? (q ? `${formatCount(rows.length)} of ${formatCount(teams.data.length)}` : `${plural(teams.data.length, 'team')}`) : ''}
        </span>
      </div>
      <DataTable
        aria-label="Teams"
        rows={rows}
        columns={columns}
        getKey={(r) => r.team.id}
        href={(r) => teamPath(org, r.team.slug)}
        loading={teams.loading}
        empty={
          teams.error ? (
            <EmptyState icon={PeopleIcon} title="Could not load teams" action={<Button onClick={() => void refresh(allTeamsKey(org), () => listAllTeams(org))}>Try again</Button>}>
              {errorMessage(teams.error)}
            </EmptyState>
          ) : (
            <EmptyState icon={PeopleIcon} title={q ? 'No teams match' : 'No teams yet'}>
              {q ? 'Try a different search.' : access.isOwner ? 'Create a team to manage repository access for groups of members.' : 'Teams you can see appear here.'}
            </EmptyState>
          )
        }
      />
      <TeamDialog org={org} open={creating} onClose={() => setCreating(false)} teams={teams.data ?? []} />
    </div>
  );
}


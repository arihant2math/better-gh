import { refresh, useResource } from '../../api/cache';
import { listOrgImports } from '../../api/metadataImports';
import { ErrorState, PageHeader, Panel } from '../../components/admin/kit';
import { Link, navigate, useParams } from '../../router';
import { Skeleton } from '../../ui/EmptyState';
import { ImportForm } from '../imports/ImportForm';
import { ImportList } from '../imports/ImportList';
import s from '../imports/imports.module.css';
import { OwnerRequired, useOrgAccess } from './common';
import { orgSettingsPath } from './OrgSettingsLayout';

/** `/organizations/:org/settings/import`: import GitHub / GitLab repositories into the organization. */
export default function OrgImportPage() {
  const { org = '' } = useParams<{ org: string }>();
  const access = useOrgAccess(org);
  const key = `org:${org}:metadata-imports`;
  const list = useResource(access.isOwner ? key : null, () => listOrgImports(org));
  if (!access.loading && !access.isOwner) return <OwnerRequired org={org} what="import repositories" />;
  const reload = () => void refresh(key, () => listOrgImports(org)).catch(() => undefined);
  return (
    <>
      <PageHeader
        title="Import a repository"
        description="Bring a repository from GitHub.com, GitHub Enterprise Server or GitLab into this organization with its issues, pull requests and reviews, labels, milestones, releases, wiki and teams. Unmatched users become mannequins you can reclaim."
        actions={
          <Link to={orgSettingsPath(org, 'mannequins')} className={s.headerLink}>
            Mannequins
          </Link>
        }
      />
      <Panel title="New import">
        <ImportForm owner={org} onCreated={(imp) => navigate(`${orgSettingsPath(org, 'import')}/${imp.id}`)} />
      </Panel>
      {list.error ? (
        <ErrorState error={list.error} onRetry={reload} />
      ) : !list.data ? (
        <Skeleton height={80} />
      ) : (
        list.data.length > 0 && (
          <Panel padded={false} title="Previous imports">
            <ImportList rows={list.data} detailPath={(id) => `${orgSettingsPath(org, 'import')}/${id}`} />
          </Panel>
        )
      )}
    </>
  );
}

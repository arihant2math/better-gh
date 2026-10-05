import { refresh, useResource } from '../../api/cache';
import { listOrgImports } from '../../api/metadataImports';
import { ErrorState, PageHeader, Panel } from '../../components/admin/kit';
import { navigate, useParams } from '../../router';
import { Skeleton } from '../../ui/EmptyState';
import { ImportForm } from '../imports/ImportForm';
import { ImportList } from '../imports/ImportList';
import { OwnerRequired, useOrgAccess } from './common';
import { orgSettingsPath } from './OrgSettingsLayout';

/** `/organizations/:org/settings/import`: import GitHub repositories into the organization. */
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
        title="Import from GitHub"
        description="Bring a repository from GitHub.com or GitHub Enterprise Server into this organization with its issues, labels, milestones, releases and teams. Unmatched users become mannequins."
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

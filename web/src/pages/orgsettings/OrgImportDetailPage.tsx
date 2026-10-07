import { PageHeader } from '../../components/admin/kit';
import { Link, useParams } from '../../router';
import { ImportDetail } from '../imports/ImportDetail';
import { orgSettingsPath } from './OrgSettingsLayout';

/** `/organizations/:org/settings/import/:id` */
export default function OrgImportDetailPage() {
  const { org = '', id = '' } = useParams<{ org: string; id: string }>();
  return (
    <>
      <PageHeader title={`Import #${id}`} description={<Link to={orgSettingsPath(org, 'import')}>All imports</Link>} />
      <ImportDetail id={Number(id)} mannequinsPath={orgSettingsPath(org, 'mannequins')} />
    </>
  );
}

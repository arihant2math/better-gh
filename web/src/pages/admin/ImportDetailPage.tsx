import { PageHeader } from '../../components/admin/kit';
import { Link, useParams } from '../../router';
import { ImportDetail } from '../imports/ImportDetail';

/** `/site-admin/imports/:id` */
export default function ImportDetailPage() {
  const { id = '' } = useParams<{ id: string }>();
  return (
    <>
      <PageHeader title={`Import #${id}`} description={<Link to="/site-admin/imports">All imports</Link>} />
      <ImportDetail id={Number(id)} />
    </>
  );
}

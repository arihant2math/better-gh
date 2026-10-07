/**
 * Repository Security tab (P65 secret scanning; P66 adds code scanning and
 * advisories): one lazy chunk for every `/:owner/:repo/security/*` route,
 * which keeps the route table (initial bundle) to a single loader.
 */
import { NotFound } from '../../app/NotFound';
import { useParams } from '../../router';
import SecretScanningAlert from './SecretScanningAlert';
import SecretScanningList from './SecretScanningList';
import SecurityOverview from './SecurityOverview';
import UnblockSecretPage from './UnblockSecretPage';

export default function SecurityPage() {
  const { number, placeholder, view } = useParams<{ number?: string; placeholder?: string; view?: string }>();
  if (placeholder !== undefined) return <UnblockSecretPage />;
  if (number !== undefined) return <SecretScanningAlert />;
  if (view === 'secret-scanning') return <SecretScanningList />;
  if (view !== undefined) return <NotFound />;
  return <SecurityOverview />;
}

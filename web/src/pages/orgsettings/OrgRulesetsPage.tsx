import { observer } from 'mobx-react-lite';
import styles from '../../components/admin/admin.module.css';
import { useLocation, useParams } from '../../router';
import { orgByLogin } from '../../sync/selectors';
import { Skeleton } from '../../ui/EmptyState';
import RulesetsSection from '../rulesets/RulesetsSection';
import { OwnerRequired, useOrgAccess } from './common';
import { orgSettingsPath } from './OrgSettingsLayout';

/** `/organizations/:org/settings/rules[/*]`: organization rulesets and rule insights (owners only). */
export default observer(function OrgRulesetsPage() {
  const { org = '' } = useParams<{ org: string }>();
  const { pathname } = useLocation();
  const access = useOrgAccess(org);
  const local = orgByLogin(org);
  const rest = pathname.split('/').slice(5).filter(Boolean).map(decodeURIComponent);
  return (
    <div className={styles.page}>
      {access.loading ? (
        <Skeleton height={160} />
      ) : !access.isOwner ? (
        <OwnerRequired org={org} what="manage rulesets" />
      ) : (
        <RulesetsSection host={{ scope: { kind: 'org', org }, base: orgSettingsPath(org, 'rules'), orgId: local?.id ?? null }} rest={rest} />
      )}
    </div>
  );
});

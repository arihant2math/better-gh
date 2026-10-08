import { observer } from 'mobx-react-lite';
import { store } from '@/sync';
import RulesetsSection from '../../rulesets/RulesetsSection';
import type { SectionProps } from '../shared';

/** `/:owner/:repo/settings/rules[/*]`: repository rulesets and rule insights. */
export default observer(function RulesSettings({ repo, rest, base }: SectionProps) {
  const isOrg = !!store().get('org', repo.ownerId);
  return (
    <RulesetsSection
      host={{
        scope: { kind: 'repo', owner: repo.owner, repo: repo.name },
        base: `${base}/rules`,
        orgId: isOrg ? repo.ownerId : null,
        repo,
        readOnly: repo.archived,
      }}
      rest={rest}
    />
  );
});

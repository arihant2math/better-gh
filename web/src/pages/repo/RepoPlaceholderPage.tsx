import { NotFound } from '../../app/NotFound';
import { useParams } from '../../router';
import { EmptyState } from '../../ui/EmptyState';
import { GraphIcon, ShieldIcon, type Icon } from '../../ui/icons';
import { PLACEHOLDER_TABS } from './nav';

const TABS: Record<string, { title: string; icon: Icon; text: string }> = {
  security: { title: 'Security', icon: ShieldIcon, text: 'Security advisories, policies and alerts.' },
  pulse: { title: 'Insights', icon: GraphIcon, text: 'Contributors, traffic, commit activity and code frequency.' },
};

/** `/:owner/:repo/:tab/*`: placeholders for announced tabs (P31 Insights, P66 Security), else a real 404. */
export default function RepoPlaceholderPage() {
  const { tab = '', '*': rest = '' } = useParams<{ tab: string; '*'?: string }>();
  const t = TABS[tab];
  if (!t || !PLACEHOLDER_TABS.has(tab) || rest) return <NotFound />;
  return (
    <EmptyState icon={t.icon} title={`${t.title} — coming soon`}>
      {t.text}
    </EmptyState>
  );
}

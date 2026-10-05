import { useParams } from '../../router';
import { EmptyState } from '../../ui/EmptyState';
import { BookIcon, GearIcon, GraphIcon, PlayIcon, ShieldIcon, TableIcon, type Icon } from '../../ui/icons';

const TABS: Record<string, { title: string; icon: Icon; text: string }> = {
  actions: { title: 'Actions', icon: PlayIcon, text: 'Workflow runs, logs and artifacts will appear here.' },
  projects: { title: 'Projects', icon: TableIcon, text: 'Plan and track work with boards and tables.' },
  wiki: { title: 'Wiki', icon: BookIcon, text: 'Long-form documentation for this repository.' },
  security: { title: 'Security', icon: ShieldIcon, text: 'Security advisories, policies and alerts.' },
  pulse: { title: 'Insights', icon: GraphIcon, text: 'Contributors, traffic, commit activity and code frequency.' },
  settings: { title: 'Settings', icon: GearIcon, text: 'General settings, collaborators, branches, webhooks and deploy keys.' },
};

/** Placeholder for repo tabs that feature agents will build. */
export default function RepoPlaceholderPage() {
  const { tab = '' } = useParams<{ tab: string }>();
  const t = TABS[tab] ?? { title: tab, icon: BookIcon, text: 'This section does not exist yet.' };
  return (
    <EmptyState icon={t.icon} title={`${t.title} — coming soon`}>
      {t.text}
    </EmptyState>
  );
}

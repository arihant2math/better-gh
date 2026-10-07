import { useState } from 'react';
import { useResource } from '../../../api/cache';
import { api, v3 } from '../../../api/client';
import {
  addOrgGroupRunner,
  createOrgGroup,
  deleteOrgGroup,
  listOrgGroupRepos,
  listOrgGroupRunners,
  listOrgGroups,
  listOrgRunners,
  removeOrgGroupRunner,
  setOrgGroupRepos,
  updateOrgGroup,
  type RunnerGroup,
} from '../../../api/runners';
import { attempt, useConfirm } from '../../../components/admin/kit';
import { Link, navigate, useParams } from '../../../router';
import type { MinimalRepository } from '../../../api/types';
import { Tag } from '../../../ui/Badge';
import { Button, IconButton } from '../../../ui/Button';
import { EmptyState } from '../../../ui/EmptyState';
import { ArrowLeftIcon, LockIcon, OrganizationIcon, PencilIcon, PlusIcon, RepoIcon, ServerIcon, TrashIcon, WorkflowIcon } from '../../../ui/icons';
import { toast } from '../../../ui/Toast';
import { ActionsNav, orgNavItems, orgSettingsBase } from './nav';
import { AddRunnerRow, GroupDialog, LabelChips, RunnerStatus, osArch, runnerStyles as k, visibilityText, type GroupValues } from './runnerKit';
import { ErrorState, ListSkeleton, Section, errorMessage } from './shared';
import styles from './Settings.module.css';

/** `/organizations/:org/settings/actions/runner-groups[/:id]` */
export default function OrgRunnerGroupsPage() {
  const { org = '', id } = useParams<{ org: string; id?: string }>();
  const groupId = id && /^\d+$/.test(id) ? Number(id) : null;
  const [version, setVersion] = useState(0);
  const bump = () => setVersion((v) => v + 1);
  const groups = useResource(`org-runner-groups:${org.toLowerCase()}:${version}`, () => listOrgGroups(org));
  const runners = useResource(`org-runners:${org.toLowerCase()}:${version}`, () => listOrgRunners(org));
  const [editing, setEditing] = useState<RunnerGroup | 'new' | null>(null);
  const repos = useResource(editing ? `org-runner-groups:${org.toLowerCase()}:repos` : null, () => api.get<MinimalRepository[]>(`${v3('orgs', org, 'repos')}?per_page=100`));
  const editingGroup = editing && editing !== 'new' ? editing : null;
  const selected = useResource(editingGroup?.visibility === 'selected' ? `org-runner-group:${editingGroup.id}:repos:${version}` : null, () => listOrgGroupRepos(org, editingGroup!.id));
  const confirm = useConfirm();
  const base = `${orgSettingsBase(org)}/actions/runner-groups`;
  const group = groupId != null ? groups.data?.find((g) => g.id === groupId) : undefined;

  const save = async (v: GroupValues) => {
    const body = { name: v.name, visibility: v.visibility, allows_public_repositories: v.allows_public_repositories, restricted_to_workflows: v.restricted_to_workflows, selected_workflows: v.selected_workflows };
    if (editingGroup) {
      await updateOrgGroup(org, editingGroup.id, editingGroup.default ? { ...body, name: undefined } : body);
      if (v.visibility === 'selected') await setOrgGroupRepos(org, editingGroup.id, v.selected);
      toast({ kind: 'success', title: `Saved ${v.name}` });
    } else {
      const created = await createOrgGroup(org, { ...body, selected_repository_ids: v.visibility === 'selected' ? v.selected : undefined });
      toast({ kind: 'success', title: `Created runner group ${created.name}` });
      navigate(`${base}/${created.id}`);
    }
    bump();
  };

  const remove = (g: RunnerGroup) =>
    confirm({
      title: `Delete runner group ${g.name}?`,
      body: 'Its runners move to the Default group, and repositories lose access granted through this group.',
      confirmLabel: 'Delete group',
      danger: true,
      onConfirm: async () => {
        await deleteOrgGroup(org, g.id);
        toast({ kind: 'success', title: `Deleted ${g.name}` });
        if (groupId === g.id) navigate(base);
        bump();
      },
    });

  const counts = new Map<number, number>();
  for (const r of runners.data ?? []) if (r.runner_group_id != null) counts.set(r.runner_group_id, (counts.get(r.runner_group_id) ?? 0) + 1);
  const newButton = (
    <Button size="sm" variant="primary" leadingIcon={PlusIcon} onClick={() => setEditing('new')}>
      New runner group
    </Button>
  );

  let content;
  if (groups.error) content = <ErrorState error={groups.error} what="runner groups" onRetry={bump} />;
  else if (!groups.data) content = <ListSkeleton rows={2} />;
  else if (groupId != null && !group)
    content = (
      <EmptyState icon={WorkflowIcon} title="Runner group not found" action={<Link to={base}>All runner groups</Link>}>
        It may have been deleted.
      </EmptyState>
    );
  else if (group)
    content = (
      <GroupDetail
        org={org}
        group={group}
        version={version}
        onEdit={() => setEditing(group)}
        onDelete={() => remove(group)}
        onChanged={bump}
        orgRunners={runners.data}
        groupName={(gid) => groups.data?.find((g) => g.id === gid)?.name}
        back={base}
      />
    );
  else
    content = (
      <Section title="Runner groups" description="Runner groups control which repositories can use the organization's self-hosted runners." action={newButton}>
        <div className={k.cardList}>
          {groups.data.map((g) => (
            <section key={g.id} className={k.groupCard} aria-label={`Runner group ${g.name}`}>
              <div className={k.groupHeader}>
                <h3 className={k.groupTitle}>
                  <Link to={`${base}/${g.id}`}>{g.name}</Link> {g.default && <Tag>Default</Tag>}
                </h3>
                <IconButton icon={PencilIcon} size="sm" label={`Edit ${g.name}`} onClick={() => setEditing(g)} />
                {!g.default && <IconButton icon={TrashIcon} size="sm" label={`Delete ${g.name}`} onClick={() => remove(g)} />}
              </div>
              <GroupMeta group={g} runnerCount={runners.data ? (counts.get(g.id) ?? 0) : undefined} />
            </section>
          ))}
        </div>
      </Section>
    );

  return (
    <div className={styles.page}>
      <ActionsNav items={orgNavItems(org)} current="runner-groups" back={{ to: `/${org}`, label: org }} />
      <div className={styles.content}>
        <header className={styles.header}>
          <div className={styles.orgCrumb}>
            <OrganizationIcon size={16} />
            {org} · Organization settings
          </div>
          <h1 className={styles.title}>{group ? group.name : 'Runner groups'}</h1>
          <p className={styles.desc}>Organize self-hosted runners into groups and choose which repositories and workflows may use each group.</p>
        </header>
        <div className={styles.sections}>{content}</div>
      </div>
      <GroupDialog
        open={!!editing}
        onClose={() => setEditing(null)}
        kind="org"
        group={editingGroup}
        targets={repos.data?.map((r) => ({ id: r.id, label: r.name, private: r.private }))}
        targetsError={repos.error}
        initialSelected={editingGroup?.visibility === 'selected' ? (selected.data?.map((r) => r.id) ?? null) : []}
        onSubmit={save}
      />
      {confirm.dialog}
    </div>
  );
}

function GroupMeta({ group: g, runnerCount, repos }: { group: RunnerGroup; runnerCount?: number; repos?: MinimalRepository[] }) {
  return (
    <div className={k.groupMeta}>
      <span>
        <RepoIcon size={14} />
        {visibilityText(g, 'org')}
        {repos && `: ${repos.map((r) => r.name).join(', ') || 'none'}`}
      </span>
      {runnerCount !== undefined && (
        <span>
          <ServerIcon size={14} />
          {runnerCount === 1 ? '1 runner' : `${runnerCount} runners`}
        </span>
      )}
      <span>{g.allows_public_repositories ? 'Public repositories allowed' : 'Public repositories not allowed'}</span>
      <span title={g.selected_workflows.join('\n')}>
        {g.restricted_to_workflows ? `Restricted to ${g.selected_workflows.length} workflow${g.selected_workflows.length === 1 ? '' : 's'}` : 'Any workflow'}
      </span>
    </div>
  );
}

function GroupDetail({
  org,
  group: g,
  version,
  onEdit,
  onDelete,
  onChanged,
  orgRunners,
  groupName,
  back,
}: {
  org: string;
  group: RunnerGroup;
  version: number;
  onEdit: () => void;
  onDelete: () => void;
  onChanged: () => void;
  orgRunners: { id: number; name: string; runner_group_id?: number | null }[] | undefined;
  groupName: (id: number | null | undefined) => string | undefined;
  back: string;
}) {
  const members = useResource(`org-runner-group:${g.id}:runners:${version}`, () => listOrgGroupRunners(org, g.id));
  const repos = useResource(g.visibility === 'selected' ? `org-runner-group:${g.id}:repos:${version}` : null, () => listOrgGroupRepos(org, g.id));
  const inGroup = new Set((members.data ?? []).map((r) => r.id));
  const candidates = (orgRunners ?? []).filter((r) => !inGroup.has(r.id)).map((r) => ({ ...r, groupName: groupName(r.runner_group_id) }));

  return (
    <>
      <Link to={back} className={styles.sectionLink} style={{ display: 'inline-flex', alignItems: 'center', gap: 4 }}>
        <ArrowLeftIcon size={14} /> All runner groups
      </Link>
      <section className={k.groupCard} aria-label="Group settings">
        <div className={k.groupHeader}>
          <h2 className={k.groupTitle}>
            Settings {g.default && <Tag>Default</Tag>}
          </h2>
          <Button size="sm" leadingIcon={PencilIcon} onClick={onEdit}>
            Edit
          </Button>
          {!g.default && (
            <Button size="sm" variant="danger" leadingIcon={TrashIcon} onClick={onDelete}>
              Delete
            </Button>
          )}
        </div>
        <GroupMeta group={g} repos={repos.data} />
        {g.restricted_to_workflows && g.selected_workflows.length > 0 && (
          <div className={k.empty}>
            {g.selected_workflows.map((w) => (
              <div key={w} className={styles.inlineCode} style={{ display: 'block', marginBottom: 2 }}>
                {w}
              </div>
            ))}
          </div>
        )}
      </section>

      <Section title="Runners" description={g.default ? 'New organization runners join the Default group. Move runners into another group to limit where they run.' : 'Removing a runner moves it back to the Default group.'}>
        <section className={k.groupCard} aria-label={`Runners in ${g.name}`}>
          {members.error ? (
            <div className={k.empty} style={{ borderTop: 0 }}>
              Couldn’t load runners: {errorMessage(members.error)}
            </div>
          ) : !members.data ? (
            <div className={k.empty} style={{ borderTop: 0 }}>
              Loading runners…
            </div>
          ) : members.data.length === 0 ? (
            <div className={k.empty} style={{ borderTop: 0 }}>
              No runners in this group yet.
            </div>
          ) : (
            members.data.map((r, i) => (
              <div key={r.id} className={k.memberRow} style={i === 0 ? { borderTop: 0 } : undefined}>
                <div className={k.memberMain}>
                  <span className={k.name}>{r.name}</span>
                  <RunnerStatus runner={r} />
                  <span className={styles.meta}>{osArch(r)}</span>
                  <LabelChips labels={r.labels} />
                </div>
                {!g.default && (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => void attempt(`Couldn’t remove ${r.name}`, () => removeOrgGroupRunner(org, g.id, r.id), `Moved ${r.name} to the Default group`).then((ok) => ok && onChanged())}
                  >
                    Remove
                  </Button>
                )}
              </div>
            ))
          )}
          {members.data && orgRunners && (
            <AddRunnerRow
              candidates={candidates}
              groupName={g.name}
              onAdd={async (id) => {
                const r = orgRunners.find((x) => x.id === id);
                if (await attempt('Couldn’t move the runner', () => addOrgGroupRunner(org, g.id, id), `Moved ${r?.name ?? 'runner'} to ${g.name}`)) onChanged();
              }}
            />
          )}
        </section>
      </Section>
      {g.visibility === 'private' && (
        <p className={styles.note}>
          <LockIcon size={12} /> Only private repositories of {org} can use this group.
        </p>
      )}
    </>
  );
}

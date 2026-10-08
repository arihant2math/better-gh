import { observer } from 'mobx-react-lite';
import { useEffect, useId, useState, type ReactNode } from 'react';
import {
  deleteRepo,
  getFullRepo,
  repoExists,
  setTopics,
  transferRepo,
  updateRepo,
  type FullRepository,
  type RepoPatch,
} from '@/api/repoSettings';
import { isPendingTransfer, requestTransfer } from '@/api/lifecycle';
import { session } from '@/app/session';
import { site, visibilityPolicy, type RepoVisibility } from '@/app/site';
import { Banner, ButtonRow, Checkbox, ConfirmDialog, FormStack, PageHeader, Section, Toggle, useDebounced } from '@/components/settings/kit';
import { navigate } from '@/router';
import { store } from '@/sync';
import type { Repo } from '@/sync/models';
import { Button } from '@/ui/Button';
import { Skeleton } from '@/ui/EmptyState';
import { CheckIcon, GitBranchIcon, AlertIcon } from '@/ui/icons';
import { Field, Input, Select } from '@/ui/Input';
import { toast } from '@/ui/Toast';
import { MERGE_MESSAGE_OPTIONS, SQUASH_MESSAGE_OPTIONS, messageOptionId } from '../model';
import styles from '../RepoSettings.module.css';
import { PendingTransferBanner, usePendingTransfer } from './PendingTransfer';
import { ChipInput, DefaultBranchDialog, LoadError, repoKey, useLocalResource, type SectionProps } from '../shared';
import { MAX_TOPICS, homepageError, normalizeTopic, repoNameError, topicError } from '../validation';

type Full = ReturnType<typeof useLocalResource<FullRepository>>;

/** PATCH with an optimistic local copy of the non-synced fields (synced ones go through the store overlay). */
function usePatch(repo: Repo, full: Full) {
  return (patch: RepoPatch, label?: string) => {
    const prev = full.data;
    full.update((cur) => ({ ...cur, ...patch }));
    const { done } = updateRepo(repo, patch, label);
    return done.then(
      (server) => {
        if (server && typeof server === 'object') full.update((cur) => ({ ...cur, ...server }));
        return server;
      },
      (e: unknown) => {
        if (prev) {
          const back: Partial<FullRepository> = {};
          for (const k of Object.keys(patch) as (keyof FullRepository)[]) (back as Record<string, unknown>)[k] = prev[k];
          full.update((cur) => ({ ...cur, ...back }));
        }
        throw e;
      },
    );
  };
}

export default observer(function GeneralSettings({ repo }: SectionProps) {
  const full = useLocalResource(repoKey(repo, 'full'), () => getFullRepo(repo.owner, repo.name));
  const patch = usePatch(repo, full);
  const ro = repo.archived;
  return (
    <>
      <PageHeader title="General" />
      {full.error ? <LoadError error={full.error} /> : null}
      <NameSection repo={repo} disabled={ro} />
      <AboutSection repo={repo} full={full.data} disabled={ro} patch={patch} />
      <DefaultBranchSection repo={repo} disabled={ro} />
      <FeaturesSection repo={repo} full={full.data} disabled={ro} patch={patch} />
      <PullRequestsSection full={full.data} disabled={ro} patch={patch} />
      <DangerZone repo={repo} />
    </>
  );
});

// ------------------------------------------------------------------ name

const NameSection = observer(function NameSection({ repo, disabled }: { repo: Repo; disabled: boolean }) {
  const id = useId();
  const [name, setName] = useState(repo.name);
  const [touched, setTouched] = useState(false);
  const [availability, setAvailability] = useState<{ name: string; taken: boolean } | null>(null);
  const trimmed = name.trim();
  const debounced = useDebounced(trimmed, 300);
  const changed = trimmed !== repo.name;
  const formatError = changed ? repoNameError(trimmed) : null;

  useEffect(() => setName(repo.name), [repo.name]);
  useEffect(() => {
    if (!debounced || debounced === repo.name || repoNameError(debounced)) return;
    let cancelled = false;
    repoExists(repo.owner, debounced).then(
      (r) => !cancelled && setAvailability({ name: debounced, taken: r.exists && r.id !== repo.id }),
      () => undefined,
    );
    return () => {
      cancelled = true;
    };
  }, [debounced, repo.owner, repo.name, repo.id]);

  const known = availability?.name === trimmed ? availability : null;
  const error = (touched || known) && formatError ? formatError : known?.taken ? `The repository ${trimmed} already exists on this account.` : null;
  const canRename = changed && !formatError && !!known && !known.taken && !disabled;

  const rename = () => {
    setTouched(true);
    if (!canRename) return;
    const oldName = repo.name;
    const { done } = updateRepo(repo, { name: trimmed }, `Rename ${oldName} to ${trimmed}`);
    // Optimistic: the store row is renamed now, so follow it to the new URL.
    navigate(`/${repo.owner}/${trimmed}/settings`, { replace: true, keepScroll: true });
    done.then(
      () => toast({ kind: 'success', title: `Repository renamed to ${trimmed}` }),
      () => navigate(`/${repo.owner}/${oldName}/settings`, { replace: true, keepScroll: true }),
    );
  };

  return (
    <Section>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          rename();
        }}
      >
        <FormStack>
          <div className={styles.inline}>
            <Field
              label="Repository name"
              htmlFor={id}
              error={error}
              hint={
                changed && known && !known.taken && !formatError ? (
                  <span className={styles.ok}>
                    <CheckIcon size={12} /> {trimmed} is available.
                  </span>
                ) : undefined
              }
            >
              <Input
                id={id}
                value={name}
                disabled={disabled}
                invalid={!!error}
                autoComplete="off"
                spellCheck={false}
                onChange={(e) => setName(e.target.value)}
                onBlur={() => setTouched(true)}
              />
            </Field>
            <Button type="submit" disabled={!canRename}>
              Rename
            </Button>
          </div>
          {changed && !error && (
            <Banner tone="warning" icon={AlertIcon}>
              Renaming changes the URL of this repository. Requests to <code>{repo.owner}/{repo.name}</code> (web, API and git) will be redirected to the
              new name, but you should update your local clones: <code>git remote set-url origin …/{repo.owner}/{trimmed}.git</code>
            </Banner>
          )}
        </FormStack>
      </form>
    </Section>
  );
});

// ------------------------------------------------------------------ about

const AboutSection = observer(function AboutSection({
  repo,
  full,
  disabled,
  patch,
}: {
  repo: Repo;
  full: FullRepository | undefined;
  disabled: boolean;
  patch: ReturnType<typeof usePatch>;
}) {
  const ids = { desc: useId(), web: useId() };
  const [desc, setDesc] = useState<string | null>(null);
  const [web, setWeb] = useState<string | null>(null);
  const [topics, setTopicsState] = useState<string[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const descV = desc ?? repo.description ?? '';
  const webV = web ?? full?.homepage ?? '';
  const topicsV = topics ?? repo.topics;
  const webError = homepageError(webV);
  const descError = descV.length > 350 ? 'Description is too long (maximum is 350 characters).' : null;
  const dirty =
    (desc !== null && desc !== (repo.description ?? '')) ||
    (web !== null && web !== (full?.homepage ?? '')) ||
    (topics !== null && topics.join(',') !== repo.topics.join(','));

  const save = async () => {
    if (!dirty || webError || descError || busy) return;
    setBusy(true);
    setError(null);
    try {
      const p: RepoPatch = {};
      if (desc !== null && desc !== (repo.description ?? '')) p.description = desc.trim() || null;
      if (web !== null && web !== (full?.homepage ?? '')) {
        const w = web.trim();
        p.homepage = w && !/^[a-z][a-z0-9+.-]*:/i.test(w) ? `https://${w}` : w || null;
      }
      const jobs: Promise<unknown>[] = [];
      if (Object.keys(p).length) jobs.push(patch(p, 'Update description'));
      if (topics !== null && topics.join(',') !== repo.topics.join(',')) jobs.push(setTopics(repo, topics));
      await Promise.all(jobs);
      setDesc(null);
      setWeb(null);
      setTopicsState(null);
      toast({ kind: 'success', title: 'Repository details saved' });
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not save.');
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section title="About" description="Shown on the repository's home page and in search results.">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <FormStack>
          <Field label="Description" htmlFor={ids.desc} error={descError}>
            <Input
              id={ids.desc}
              value={descV}
              disabled={disabled}
              invalid={!!descError}
              placeholder="Short description of this repository"
              onChange={(e) => setDesc(e.target.value)}
            />
          </Field>
          <Field label="Website" htmlFor={ids.web} error={web !== null ? webError : null}>
            {full ? (
              <Input
                id={ids.web}
                value={webV}
                disabled={disabled}
                invalid={web !== null && !!webError}
                placeholder="https://example.com"
                inputMode="url"
                onChange={(e) => setWeb(e.target.value)}
              />
            ) : (
              <Skeleton height={32} />
            )}
          </Field>
          <ChipInput
            label="Topics"
            values={topicsV}
            onChange={setTopicsState}
            normalize={normalizeTopic}
            validate={(t) => topicError(t)}
            max={MAX_TOPICS}
            disabled={disabled}
            placeholder="Add topics (press Enter or comma)"
            hint={`Lowercase letters, numbers and hyphens. Up to ${MAX_TOPICS} topics.`}
          />
          {error && <Banner tone="danger">{error}</Banner>}
          <ButtonRow>
            <Button type="submit" variant="primary" disabled={!dirty || !!webError || !!descError || disabled} loading={busy}>
              Save changes
            </Button>
            {dirty && (
              <Button
                variant="ghost"
                onClick={() => {
                  setDesc(null);
                  setWeb(null);
                  setTopicsState(null);
                  setError(null);
                }}
              >
                Discard
              </Button>
            )}
          </ButtonRow>
        </FormStack>
      </form>
    </Section>
  );
});

// ------------------------------------------------------------------ default branch

const DefaultBranchSection = observer(function DefaultBranchSection({ repo, disabled }: { repo: Repo; disabled: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <Section title="Default branch" description="The default branch is the base branch for pull requests and code commits.">
      <div className={styles.box}>
        <div className={styles.boxRow}>
          <GitBranchIcon size={16} />
          <span className={styles.boxText}>
            <span className={styles.mono}>{repo.defaultBranch}</span>
          </span>
          <Button size="sm" disabled={disabled} onClick={() => setOpen(true)}>
            Switch to another branch
          </Button>
        </div>
      </div>
      <DefaultBranchDialog repo={repo} open={open} onClose={() => setOpen(false)} />
    </Section>
  );
});

// ------------------------------------------------------------------ features

function auto(p: Promise<unknown>) {
  p.catch(() => undefined); // rollback + toast are automatic
}

const FeaturesSection = observer(function FeaturesSection({
  repo,
  full,
  disabled,
  patch,
}: {
  repo: Repo;
  full: FullRepository | undefined;
  disabled: boolean;
  patch: ReturnType<typeof usePatch>;
}) {
  return (
    <Section title="Features">
      <FormStack>
        <Toggle
          label="Issues"
          description="Track ideas, feedback, tasks, and bugs."
          checked={repo.hasIssues}
          disabled={disabled}
          onChange={(v) => auto(patch({ has_issues: v }, v ? 'Enable issues' : 'Disable issues'))}
        />
        <Toggle
          label="Projects"
          description="Plan and track work with project boards."
          checked={repo.hasProjects}
          disabled={disabled}
          onChange={(v) => auto(patch({ has_projects: v }, v ? 'Enable projects' : 'Disable projects'))}
        />
        <Toggle
          label="Wikis"
          description="Host documentation for your repository."
          checked={repo.hasWiki}
          disabled={disabled}
          onChange={(v) => auto(patch({ has_wiki: v }, v ? 'Enable wiki' : 'Disable wiki'))}
        />
        <Toggle
          label="Discussions"
          description="A place for your community to have conversations, ask questions and post answers."
          checked={!!full?.has_discussions}
          disabled={disabled || !full}
          onChange={(v) => auto(patch({ has_discussions: v }, v ? 'Enable discussions' : 'Disable discussions'))}
        />
        <Checkbox
          label="Template repository"
          description="Template repositories let users generate new repositories with the same directory structure and files."
          checked={!!full?.is_template}
          disabled={disabled || !full}
          onChange={(v) => auto(patch({ is_template: v }, v ? 'Make template' : 'Remove template'))}
        />
        <Checkbox
          label="Allow forking"
          description="Members can fork this repository."
          checked={!!full?.allow_forking}
          disabled={disabled || !full}
          onChange={(v) => auto(patch({ allow_forking: v }, v ? 'Allow forking' : 'Disallow forking'))}
        />
        <Checkbox
          label="Require contributors to sign off on web-based commits"
          description="Commits made through the web interface must include a Signed-off-by trailer."
          checked={!!full?.web_commit_signoff_required}
          disabled={disabled || !full}
          onChange={(v) => auto(patch({ web_commit_signoff_required: v }, 'Update commit sign-off'))}
        />
      </FormStack>
    </Section>
  );
});

// ------------------------------------------------------------------ pull requests

const PullRequestsSection = observer(function PullRequestsSection({
  full,
  disabled,
  patch,
}: {
  full: FullRepository | undefined;
  disabled: boolean;
  patch: ReturnType<typeof usePatch>;
}) {
  const [error, setError] = useState<string | null>(null);
  const mergeId = useId();
  const squashId = useId();
  if (!full) {
    return (
      <Section title="Pull Requests">
        <Skeleton height={120} />
      </Section>
    );
  }
  const methods = { allow_merge_commit: full.allow_merge_commit, allow_squash_merge: full.allow_squash_merge, allow_rebase_merge: full.allow_rebase_merge };
  const setMethod = (k: keyof typeof methods, v: boolean) => {
    const next = { ...methods, [k]: v };
    if (!next.allow_merge_commit && !next.allow_squash_merge && !next.allow_rebase_merge) {
      setError('You must select at least one merge method.');
      return;
    }
    setError(null);
    auto(patch({ [k]: v }, 'Update merge options'));
  };
  const off = disabled;
  return (
    <Section title="Pull Requests" description="When merging pull requests, you can allow any combination of merge commits, squashing, or rebasing. At least one option must be enabled.">
      <FormStack>
        <div className={styles.group} role="group" aria-label="Merge methods">
          <Checkbox
            label="Allow merge commits"
            description="Add all commits from the head branch to the base branch with a merge commit."
            checked={full.allow_merge_commit}
            disabled={off}
            onChange={(v) => setMethod('allow_merge_commit', v)}
          />
          {full.allow_merge_commit && (
            <div className={styles.indent}>
              <MessageSelect
                id={mergeId}
                label="Default commit message"
                value={messageOptionId(MERGE_MESSAGE_OPTIONS, full.merge_commit_title, full.merge_commit_message)}
                options={MERGE_MESSAGE_OPTIONS}
                disabled={off}
                onChange={(o) => auto(patch({ merge_commit_title: o.title, merge_commit_message: o.message }, 'Update merge commit message'))}
              />
            </div>
          )}
          <Checkbox
            label="Allow squash merging"
            description="Combine all commits from the head branch into a single commit in the base branch."
            checked={full.allow_squash_merge}
            disabled={off}
            onChange={(v) => setMethod('allow_squash_merge', v)}
          />
          {full.allow_squash_merge && (
            <div className={styles.indent}>
              <MessageSelect
                id={squashId}
                label="Default commit message"
                value={messageOptionId(SQUASH_MESSAGE_OPTIONS, full.squash_merge_commit_title, full.squash_merge_commit_message)}
                options={SQUASH_MESSAGE_OPTIONS}
                disabled={off}
                onChange={(o) => auto(patch({ squash_merge_commit_title: o.title, squash_merge_commit_message: o.message }, 'Update squash commit message'))}
              />
            </div>
          )}
          <Checkbox
            label="Allow rebase merging"
            description="Add all commits from the head branch onto the base branch individually."
            checked={full.allow_rebase_merge}
            disabled={off}
            onChange={(v) => setMethod('allow_rebase_merge', v)}
          />
          {error && (
            <Banner tone="danger" icon={AlertIcon}>
              {error}
            </Banner>
          )}
        </div>
        <Checkbox
          label="Always suggest updating pull request branches"
          description="Whenever there are new changes available in the base branch, present an “update branch” option in the pull request."
          checked={full.allow_update_branch}
          disabled={off}
          onChange={(v) => auto(patch({ allow_update_branch: v }, 'Update pull request settings'))}
        />
        <Checkbox
          label="Allow auto-merge"
          description="Waits for merge requirements to be met and then merges automatically."
          checked={full.allow_auto_merge}
          disabled={off}
          onChange={(v) => auto(patch({ allow_auto_merge: v }, 'Update pull request settings'))}
        />
        <Checkbox
          label="Automatically delete head branches"
          description="Deleted branches will still be able to be restored."
          checked={full.delete_branch_on_merge}
          disabled={off}
          onChange={(v) => auto(patch({ delete_branch_on_merge: v }, 'Update pull request settings'))}
        />
      </FormStack>
    </Section>
  );
});

function MessageSelect<T, M>({
  id,
  label,
  value,
  options,
  disabled,
  onChange,
}: {
  id: string;
  label: string;
  value: string;
  options: { id: string; label: string; title: T; message: M }[];
  disabled?: boolean;
  onChange: (o: { title: T; message: M }) => void;
}) {
  return (
    <Field label={label} htmlFor={id}>
      <Select id={id} value={value} disabled={disabled} onChange={(e) => onChange(options.find((o) => o.id === e.target.value)!)}>
        {options.map((o) => (
          <option key={o.id} value={o.id}>
            {o.label}
          </option>
        ))}
      </Select>
    </Field>
  );
}

// ------------------------------------------------------------------ danger zone

function DangerRow({ title, children, action }: { title: string; children: ReactNode; action: ReactNode }) {
  return (
    <div className={styles.boxRow}>
      <div className={styles.boxText}>
        <span className={styles.dangerTitle}>{title}</span>
        <span className={styles.small}>{children}</span>
      </div>
      {action}
    </div>
  );
}

type DangerDialog = 'visibility' | 'archive' | 'transfer' | 'delete' | null;

const currentVisibility = (repo: Repo): RepoVisibility => repo.visibility ?? (repo.private ? 'private' : 'public');

const VISIBILITY_WARNING: Record<RepoVisibility, string> = {
  public: 'The code will be visible to everyone who can see this site. Anyone can fork your repository.',
  internal: 'Everyone signed in to this site will be able to see and fork this repository.',
  private: 'Only people with access will see this repository. Stars and watchers from people without access are hidden.',
};

/** Change visibility to any other visibility the site policy allows (internal: organizations only). */
const VisibilityDialog = observer(function VisibilityDialog({ repo, open, onClose }: { repo: Repo; open: boolean; onClose: () => void }) {
  const full = `${repo.owner}/${repo.name}`;
  const current = currentVisibility(repo);
  const isOrg = !!store().get('org', repo.ownerId);
  const targets = visibilityPolicy(site.info, isOrg).allowed.filter((v) => v !== current);
  const [picked, setPicked] = useState<RepoVisibility | null>(null);
  const target = picked && targets.includes(picked) ? picked : targets[0];
  const id = useId();
  if (!target) {
    return (
      <ConfirmDialog open={open} onClose={onClose} title={`Change the visibility of ${full}`} confirmLabel="Close" danger={false} onConfirm={() => undefined}>
        <Banner tone="info" icon={AlertIcon}>
          The site policy allows no other visibility for this repository.
        </Banner>
      </ConfirmDialog>
    );
  }
  return (
    <ConfirmDialog
      open={open}
      onClose={onClose}
      title={`Make ${full} ${target}`}
      confirmLabel={`I understand, make this repository ${target}`}
      confirmText={full}
      onConfirm={() => {
        auto(updateRepo(repo, { visibility: target }, `Make ${full} ${target}`).done.then(() => toast({ kind: 'success', title: `${full} is now ${target}` })));
      }}
    >
      {targets.length > 1 && (
        <Field label="New visibility" htmlFor={id}>
          <Select id={id} value={target} onChange={(e) => setPicked(e.target.value as RepoVisibility)}>
            {targets.map((v) => (
              <option key={v} value={v}>
                {v[0]!.toUpperCase() + v.slice(1)}
              </option>
            ))}
          </Select>
        </Field>
      )}
      <Banner tone="warning" icon={AlertIcon}>
        {VISIBILITY_WARNING[target]}
      </Banner>
    </ConfirmDialog>
  );
});

const DangerZone = observer(function DangerZone({ repo }: { repo: Repo }) {
  const [open, setOpen] = useState<DangerDialog>(null);
  const full = `${repo.owner}/${repo.name}`;
  const close = () => setOpen(null);
  const pending = usePendingTransfer(repo.owner, repo.name);
  return (
    <Section title="Danger Zone" danger>
      {pending.transfer && <PendingTransferBanner owner={repo.owner} transfer={pending.transfer} onCancelled={pending.clear} />}
      <div className={styles.box}>
        <DangerRow
          title="Change repository visibility"
          action={
            <Button variant="danger" size="sm" disabled={repo.archived} onClick={() => setOpen('visibility')}>
              Change visibility
            </Button>
          }
        >
          This repository is currently {currentVisibility(repo)}.
        </DangerRow>
        <DangerRow
          title="Transfer ownership"
          action={
            <Button variant="danger" size="sm" disabled={repo.archived || !!pending.transfer} onClick={() => setOpen('transfer')}>
              Transfer
            </Button>
          }
        >
          {pending.transfer
            ? `A transfer to ${pending.transfer.to.login} is waiting to be accepted. Cancel it to transfer elsewhere.`
            : 'Transfer this repository to another user or to an organization where you can create repositories. Another user has 1 day to accept.'}
        </DangerRow>
        <DangerRow
          title={repo.archived ? 'Unarchive this repository' : 'Archive this repository'}
          action={
            <Button variant="danger" size="sm" onClick={() => setOpen('archive')}>
              {repo.archived ? 'Unarchive this repository' : 'Archive this repository'}
            </Button>
          }
        >
          {repo.archived ? 'Make this repository writable again.' : 'Mark this repository as archived and read-only.'}
        </DangerRow>
        <DangerRow
          title="Delete this repository"
          action={
            <Button variant="danger" size="sm" onClick={() => setOpen('delete')}>
              Delete this repository
            </Button>
          }
        >
          Once you delete a repository, there is no going back. Please be certain.
        </DangerRow>
      </div>

      <VisibilityDialog repo={repo} open={open === 'visibility'} onClose={close} />

      <ConfirmDialog
        open={open === 'archive'}
        onClose={close}
        title={repo.archived ? `Unarchive ${full}` : `Archive ${full}`}
        confirmLabel={repo.archived ? 'I understand, unarchive this repository' : 'I understand the consequences, archive this repository'}
        confirmText={full}
        onConfirm={() => {
          const archived = !repo.archived;
          auto(
            updateRepo(repo, { archived }, archived ? `Archive ${full}` : `Unarchive ${full}`).done.then(() =>
              toast({ kind: 'success', title: archived ? `${full} was archived` : `${full} was unarchived` }),
            ),
          );
        }}
      >
        <Banner tone="warning" icon={AlertIcon}>
          {repo.archived
            ? 'This will make the repository writable again: issues, pull requests, labels, milestones, wikis and settings can be changed.'
            : 'This repository will become read-only. Issues, pull requests, labels, milestones, wikis and settings can no longer be changed, and no one can push to it.'}
        </Banner>
      </ConfirmDialog>

      <TransferDialog repo={repo} open={open === 'transfer'} onClose={close} onRequested={pending.reload} />

      <ConfirmDialog
        open={open === 'delete'}
        onClose={close}
        title={`Delete ${full}`}
        confirmLabel="Delete this repository"
        confirmText={full}
        onConfirm={async () => {
          await deleteRepo(repo);
          navigate('/');
          toast({ kind: 'success', title: `Your repository "${full}" was successfully deleted.` });
        }}
      >
        <Banner tone="danger" icon={AlertIcon}>
          This will permanently delete the <strong>{full}</strong> repository, wiki, issues, comments, webhooks and settings, and remove all collaborator
          associations. This cannot be undone.
        </Banner>
      </ConfirmDialog>
    </Section>
  );
});

const TransferDialog = observer(function TransferDialog({ repo, open, onClose, onRequested }: { repo: Repo; open: boolean; onClose: () => void; onRequested: () => void }) {
  const ownerId = useId();
  const nameId = useId();
  const listId = useId();
  const [newOwner, setNewOwner] = useState('');
  const [newName, setNewName] = useState('');
  useEffect(() => {
    if (open) {
      setNewOwner('');
      setNewName('');
    }
  }, [open]);
  const viewer = session.user;
  const s = store();
  const targets = [
    ...(viewer && viewer.login !== repo.owner ? [viewer.login] : []),
    ...s
      .all('membership')
      .filter((m) => m.userId === viewer?.id)
      .map((m) => s.get('org', m.orgId)?.login)
      .filter((l): l is string => !!l && l !== repo.owner),
  ];
  const nameErr = newName.trim() ? repoNameError(newName) : null;
  const full = `${repo.owner}/${repo.name}`;
  return (
    <ConfirmDialog
      open={open}
      onClose={onClose}
      title={`Transfer ${full}`}
      confirmLabel="I understand, transfer this repository"
      confirmText={full}
      onConfirm={async () => {
        const o = newOwner.trim();
        if (!o) throw new Error('Enter the new owner.');
        if (o.toLowerCase() === repo.owner.toLowerCase() && (!newName.trim() || newName.trim() === repo.name)) throw new Error('The repository is already owned by this account.');
        if (nameErr) throw new Error(nameErr);
        const name = newName.trim() || repo.name;
        // To an organization or to yourself the move is immediate (optimistic below). To another
        // user the server answers 202 with the repository unchanged: a request they have 1 day to accept.
        const toOrg = !!s.byKey('org', 'login', o.toLowerCase());
        const toSelf = !!viewer && o.toLowerCase() === viewer.login.toLowerCase();
        if (!toOrg && !toSelf) {
          const res = await requestTransfer(repo.owner, repo.name, o, newName.trim() || undefined);
          if (isPendingTransfer(full, res)) {
            onRequested();
            toast({ kind: 'success', title: `Transfer requested — ${o} has 1 day to accept`, description: `${full} stays where it is until ${o} accepts.` });
          } else {
            navigate(`/${res.full_name}/settings`, { replace: true });
            toast({ kind: 'success', title: `Repository transferred to ${res.full_name}` });
          }
          return;
        }
        const oldPath = `/${repo.owner}/${repo.name}/settings`;
        const done = transferRepo(repo, o, newName.trim() || undefined);
        navigate(`/${o}/${name}/settings`, { replace: true });
        done.then(
          () => toast({ kind: 'success', title: `Repository transferred to ${o}/${name}` }),
          () => navigate(oldPath, { replace: true }),
        );
      }}
    >
      <Banner tone="warning" icon={AlertIcon}>
        Transferring moves the repository with its issues, pull requests, wiki, stars and watchers. Requests to the old URL are redirected. Team access is removed
        unless the new owner is the same organization. A transfer to another user only happens once they accept it (within 1 day).
      </Banner>
      <Field label="New owner" htmlFor={ownerId} hint="A user or an organization where you can create repositories.">
        <Input id={ownerId} value={newOwner} list={listId} autoComplete="off" spellCheck={false} onChange={(e) => setNewOwner(e.target.value)} />
        <datalist id={listId}>
          {targets.map((t) => (
            <option key={t} value={t} />
          ))}
        </datalist>
      </Field>
      <Field label="New repository name (optional)" htmlFor={nameId} error={nameErr}>
        <Input id={nameId} value={newName} placeholder={repo.name} autoComplete="off" spellCheck={false} invalid={!!nameErr} onChange={(e) => setNewName(e.target.value)} />
      </Field>
    </ConfirmDialog>
  );
});

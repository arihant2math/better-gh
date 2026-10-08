import { useEffect, useId, useState, type FormEvent } from 'react';
import {
  createBranchPolicy,
  deleteBranchPolicy,
  getEnvironment,
  listBranchPolicies,
  updateEnvironment,
  type BranchPolicy,
  type Environment,
  type EnvironmentReviewer,
} from '@/api/actions';
import { api, v3 } from '@/api/client';
import { Link } from '@/router';
import { Avatar } from '@/ui/Badge';
import { Button, IconButton } from '@/ui/Button';
import { GitBranchIcon, PeopleIcon, PlusIcon, TagIcon, TrashIcon, XIcon } from '@/ui/icons';
import { Field, Input, Select } from '@/ui/Input';
import { toast } from '@/ui/Toast';
import { ConfigList } from './ConfigItems';
import { ErrorState, ListSkeleton, Section, reload, toastError, useRes, type Res } from './shared';
import styles from './Settings.module.css';

const MAX_REVIEWERS = 6;
const MAX_WAIT = 43_200;

type Policy = 'all' | 'protected' | 'custom';

const envRes = (owner: string, repo: string, env: string): Res<Environment> => ({
  key: `actions:env:${owner}/${repo}:${env}`.toLowerCase(),
  load: () => getEnvironment(owner, repo, env),
});

const policiesRes = (owner: string, repo: string, env: string): Res<BranchPolicy[]> => ({
  key: `actions:env-policies:${owner}/${repo}:${env}`.toLowerCase(),
  load: () => listBranchPolicies(owner, repo, env).then((r) => r.branch_policies),
});

function policyOf(e: Environment): Policy {
  const p = e.deployment_branch_policy;
  if (!p) return 'all';
  return p.protected_branches ? 'protected' : 'custom';
}

/** Resolve `login` or `org/team-slug` to a reviewer. */
async function resolveReviewer(text: string): Promise<EnvironmentReviewer> {
  const t = text.trim().replace(/^@/, '');
  if (t.includes('/')) {
    const [org, slug] = t.split('/', 2) as [string, string];
    const team = await api.get<{ id: number; name: string; slug: string; html_url: string }>(v3('orgs', org, 'teams', slug));
    return { type: 'Team', reviewer: { id: team.id, name: team.name, slug: `${org}/${team.slug}`, html_url: team.html_url } };
  }
  const user = await api.get<{ id: number; login: string; avatar_url: string }>(v3('users', t));
  return { type: 'User', reviewer: { id: user.id, login: user.login, avatar_url: user.avatar_url } };
}

/** `/settings/environments/:env/edit`: protection rules, branch policies, secrets and variables of one environment. */
export default function EnvironmentConfig({ owner, repo, env }: { owner: string; repo: string; env: string }) {
  const res = envRes(owner, repo, env);
  const { data, error } = useRes(res);
  const back = `/${owner}/${repo}/settings/environments`;
  return (
    <>
      <Link to={back} className={styles.sectionLink}>
        ← All environments
      </Link>
      {error ? (
        <ErrorState error={error} what="the environment" onRetry={() => void reload(res)} />
      ) : !data ? (
        <ListSkeleton rows={3} />
      ) : (
        <>
          <ProtectionRules key={data.updated_at} owner={owner} repo={repo} env={data} onSaved={() => void reload(res)} />
          {policyOf(data) === 'custom' && <BranchPolicies owner={owner} repo={repo} env={data.name} />}
          <ConfigList scope={{ kind: 'env', owner, repo, env: data.name }} kind="secrets" title="Environment secrets" description="Released to a job only after its protection rules pass." />
          <ConfigList scope={{ kind: 'env', owner, repo, env: data.name }} kind="variables" title="Environment variables" />
        </>
      )}
    </>
  );
}

function ProtectionRules({ owner, repo, env, onSaved }: { owner: string; repo: string; env: Environment; onSaved: () => void }) {
  const id = useId();
  const rules = env.protection_rules ?? [];
  const reviewerRule = rules.find((r) => r.type === 'required_reviewers');
  const timerRule = rules.find((r) => r.type === 'wait_timer');
  const [reviewers, setReviewers] = useState<EnvironmentReviewer[]>(reviewerRule?.type === 'required_reviewers' ? reviewerRule.reviewers : []);
  const [selfReview, setSelfReview] = useState(reviewerRule?.type === 'required_reviewers' ? reviewerRule.prevent_self_review : false);
  const [timerOn, setTimerOn] = useState(!!timerRule);
  const [wait, setWait] = useState(String(timerRule?.type === 'wait_timer' ? timerRule.wait_timer : 5));
  const [bypass, setBypass] = useState(env.can_admins_bypass ?? true);
  const [policy, setPolicy] = useState<Policy>(policyOf(env));
  const [adding, setAdding] = useState('');
  const [addErr, setAddErr] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const minutes = Number(wait);
  const waitErr = timerOn && (!Number.isInteger(minutes) || minutes < 1 || minutes > MAX_WAIT) ? `Enter a whole number of minutes between 1 and ${MAX_WAIT}.` : null;

  const add = async (e: FormEvent) => {
    e.preventDefault();
    if (!adding.trim()) return;
    if (reviewers.length >= MAX_REVIEWERS) {
      setAddErr(`An environment can have at most ${MAX_REVIEWERS} reviewers.`);
      return;
    }
    try {
      const r = await resolveReviewer(adding);
      if (!reviewers.some((x) => x.type === r.type && x.reviewer.id === r.reviewer.id)) setReviewers([...reviewers, r]);
      setAdding('');
      setAddErr(null);
    } catch {
      setAddErr(`No user or team named ${adding.trim()}.`);
    }
  };

  const save = async () => {
    if (waitErr || saving) return;
    setSaving(true);
    try {
      await updateEnvironment(owner, repo, env.name, {
        reviewers: reviewers.map((r) => ({ type: r.type, id: r.reviewer.id })),
        prevent_self_review: selfReview,
        wait_timer: timerOn ? minutes : 0,
        can_admins_bypass: bypass,
        deployment_branch_policy: policy === 'all' ? null : { protected_branches: policy === 'protected', custom_branch_policies: policy === 'custom' },
      });
      toast({ kind: 'success', title: `Saved protection rules of ${env.name}` });
      onSaved();
    } catch (e) {
      toastError("Couldn't save the protection rules", e);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Section
      title={`Environment: ${env.name}`}
      description="Jobs that reference this environment wait for these rules before they start and receive its secrets."
      action={
        <Button size="sm" variant="primary" loading={saving} disabled={!!waitErr} onClick={() => void save()}>
          Save protection rules
        </Button>
      }
    >
      <div className={styles.envRules}>
        <div className={styles.envRule}>
          <h3 className={styles.envRuleTitle}>
            <PeopleIcon size={16} /> Required reviewers
          </h3>
          <p className={styles.sectionDesc}>Up to {MAX_REVIEWERS} people or teams; one approval lets the job run.</p>
          {reviewers.length > 0 && (
            <ul className={styles.reviewerList} aria-label="Required reviewers">
              {reviewers.map((r) => {
                const label = r.type === 'Team' ? (r.reviewer.slug ?? r.reviewer.name ?? 'team') : (r.reviewer.login ?? 'user');
                return (
                  <li key={`${r.type}:${r.reviewer.id}`} className={styles.reviewerItem}>
                    {r.type === 'User' ? <Avatar user={{ login: label, avatarUrl: r.reviewer.avatar_url ?? '' }} size={20} /> : <PeopleIcon size={16} />}
                    <span>{label}</span>
                    <IconButton icon={XIcon} size="sm" label={`Remove ${label}`} onClick={() => setReviewers(reviewers.filter((x) => x !== r))} />
                  </li>
                );
              })}
            </ul>
          )}
          <form className={styles.inlineForm} onSubmit={(e) => void add(e)}>
            <Field label="Add reviewer" htmlFor={`${id}-rev`} error={addErr} hint="A username, or org/team-slug for a team.">
              <Input id={`${id}-rev`} value={adding} autoComplete="off" invalid={!!addErr} onChange={(e) => setAdding(e.target.value)} />
            </Field>
            <Button type="submit" size="sm" leadingIcon={PlusIcon} disabled={!adding.trim()}>
              Add
            </Button>
          </form>
          <label className={styles.check}>
            <input type="checkbox" checked={selfReview} onChange={(e) => setSelfReview(e.target.checked)} />
            Prevent self-review (the user who triggered the run can't approve it)
          </label>
        </div>
        <div className={styles.envRule}>
          <label className={styles.check}>
            <input type="checkbox" checked={timerOn} onChange={(e) => setTimerOn(e.target.checked)} />
            <strong>Wait timer</strong>
          </label>
          {timerOn && (
            <Field label="Minutes to wait before the job starts" htmlFor={`${id}-wait`} error={waitErr}>
              <Input id={`${id}-wait`} type="number" min={1} max={MAX_WAIT} value={wait} invalid={!!waitErr} onChange={(e) => setWait(e.target.value)} />
            </Field>
          )}
        </div>
        <div className={styles.envRule}>
          <label className={styles.check}>
            <input type="checkbox" checked={bypass} onChange={(e) => setBypass(e.target.checked)} />
            <strong>Allow administrators to bypass</strong> (repository admins may approve without being reviewers)
          </label>
        </div>
        <div className={styles.envRule}>
          <Field label="Deployment branches and tags" htmlFor={`${id}-policy`}>
            <Select id={`${id}-policy`} value={policy} onChange={(e) => setPolicy(e.target.value as Policy)}>
              <option value="all">No restriction</option>
              <option value="protected">Protected branches only</option>
              <option value="custom">Selected branches and tags</option>
            </Select>
          </Field>
        </div>
      </div>
    </Section>
  );
}

function BranchPolicies({ owner, repo, env }: { owner: string; repo: string; env: string }) {
  const id = useId();
  const res = policiesRes(owner, repo, env);
  const { data, error } = useRes(res);
  const [name, setName] = useState('');
  const [kind, setKind] = useState<'branch' | 'tag'>('branch');
  const [busy, setBusy] = useState(false);
  useEffect(() => setName(''), [env]);
  const add = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim() || busy) return;
    setBusy(true);
    try {
      await createBranchPolicy(owner, repo, env, name.trim(), kind);
      setName('');
      await reload(res);
    } catch (x) {
      toastError("Couldn't add the rule", x);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Section title="Deployment branch and tag rules" description="Only refs matching a rule may deploy to this environment. * matches within a path segment, ** across segments.">
      {error ? (
        <ErrorState error={error} what="branch rules" onRetry={() => void reload(res)} />
      ) : !data ? (
        <ListSkeleton rows={1} />
      ) : data.length === 0 ? (
        <div className={styles.emptyInline}>No rules yet: nothing can deploy until you add one.</div>
      ) : (
        <div className={styles.list} role="list">
          {data.map((p) => (
            <div key={p.id} role="listitem" className={styles.row}>
              {p.type === 'tag' ? <TagIcon size={16} className={styles.rowIcon} /> : <GitBranchIcon size={16} className={styles.rowIcon} />}
              <span className={styles.envName}>{p.name}</span>
              <span className={styles.spacer} />
              <span className={styles.meta}>{p.type}</span>
              <span className={styles.rowActions}>
                <IconButton
                  icon={TrashIcon}
                  size="sm"
                  label={`Delete rule ${p.name}`}
                  onClick={() => void deleteBranchPolicy(owner, repo, env, p.id).then(() => reload(res), (x) => toastError("Couldn't delete the rule", x))}
                />
              </span>
            </div>
          ))}
        </div>
      )}
      <form className={styles.inlineForm} onSubmit={(e) => void add(e)}>
        <Field label="Name pattern" htmlFor={`${id}-name`}>
          <Input id={`${id}-name`} value={name} placeholder="release/*" autoComplete="off" onChange={(e) => setName(e.target.value)} />
        </Field>
        <Select aria-label="Ref type" value={kind} onChange={(e) => setKind(e.target.value as 'branch' | 'tag')}>
          <option value="branch">Branch</option>
          <option value="tag">Tag</option>
        </Select>
        <Button type="submit" size="sm" leadingIcon={PlusIcon} loading={busy} disabled={!name.trim()}>
          Add rule
        </Button>
      </form>
    </Section>
  );
}

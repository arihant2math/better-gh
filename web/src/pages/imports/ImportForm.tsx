import { useId, useState, type FormEvent } from 'react';
import { createMetadataImport, apiUrlFor, parseUserMap, sourceRepoFrom, type MetadataImport, type MetadataImportInput } from '../../api/metadataImports';
import { Checkbox, apiFieldErrors } from '../../components/settings/kit';
import { Button } from '../../ui/Button';
import { Field, Input, Select, Textarea } from '../../ui/Input';
import s from './imports.module.css';

type Steps = Required<Pick<MetadataImportInput, 'git' | 'settings' | 'labels' | 'milestones' | 'issues' | 'releases' | 'teams' | 'include_lfs'>>;

const STEP_LABELS: [keyof Steps, string, string?][] = [
  ['git', 'Git history', 'Branches and tags'],
  ['settings', 'Repository settings', 'Description, homepage, topics, features'],
  ['labels', 'Labels'],
  ['milestones', 'Milestones'],
  ['issues', 'Issues', 'With comments, reactions and key events; original numbers'],
  ['releases', 'Releases', 'With downloaded assets'],
  ['teams', 'Teams', 'Organization teams and their repository permissions'],
  ['include_lfs', 'Git LFS objects'],
];

/**
 * Start a GitHub / GHES metadata import. `owner` fixes the target owner
 * (organization settings); otherwise it is asked for (site admin).
 */
export function ImportForm({ owner: fixedOwner, onCreated }: { owner?: string; onCreated: (imp: MetadataImport) => void }) {
  const id = useId();
  const [ghes, setGhes] = useState(false);
  const [host, setHost] = useState('');
  const [source, setSource] = useState('');
  const [token, setToken] = useState('');
  const [owner, setOwner] = useState(fixedOwner ?? '');
  const [name, setName] = useState('');
  const [visibility, setVisibility] = useState<'' | 'public' | 'private' | 'internal'>('');
  const [userMap, setUserMap] = useState('');
  const [steps, setSteps] = useState<Steps>({ git: true, settings: true, labels: true, milestones: true, issues: true, releases: true, teams: false, include_lfs: false });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [fields, setFields] = useState<Record<string, string>>({});

  const repo = sourceRepoFrom(source);
  const map = parseUserMap(userMap);
  const local: Record<string, string> = {};
  if (source && !/^[^/\s]+\/[^/\s]+$/.test(repo)) local.source_repo = 'Use owner/name or the repository URL';
  if (ghes && !host.trim()) local.api_url = 'Enter the GitHub Enterprise Server host';
  if (!fixedOwner && !owner.trim()) local.owner = '';
  if (map.error) local.user_map = map.error;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (Object.keys(local).length || !repo) return;
    setBusy(true);
    setError(null);
    setFields({});
    try {
      const imp = await createMetadataImport({
        api_url: ghes ? apiUrlFor(host) : 'https://api.github.com',
        source_repo: repo,
        token: token.trim() || undefined,
        owner: (fixedOwner ?? owner).trim(),
        name: name.trim() || undefined,
        visibility: visibility || undefined,
        user_map: map.map,
        ...steps,
      });
      setToken('');
      onCreated(imp);
    } catch (err) {
      const { message, fields } = apiFieldErrors(err);
      setFields(fields as Record<string, string>);
      setError(Object.keys(fields).length ? null : message);
    } finally {
      setBusy(false);
    }
  };

  const err = (f: string) => fields[f] ?? (local[f] || null);

  return (
    <form className={s.form} onSubmit={(e) => void submit(e)} aria-label="New import">
      <fieldset className={s.fieldset}>
        <legend className={s.legend}>Source</legend>
        <div className={s.row}>
          <Field label="Platform" htmlFor={`${id}-platform`}>
            <Select id={`${id}-platform`} value={ghes ? 'ghes' : 'github'} onChange={(e) => setGhes(e.target.value === 'ghes')}>
              <option value="github">GitHub.com</option>
              <option value="ghes">GitHub Enterprise Server</option>
            </Select>
          </Field>
          {ghes && (
            <Field label="Server host" htmlFor={`${id}-host`} error={err('api_url')} hint="e.g. github.example.com (the API is /api/v3)">
              <Input id={`${id}-host`} value={host} onChange={(e) => setHost(e.target.value)} placeholder="github.example.com" invalid={!!err('api_url')} />
            </Field>
          )}
        </div>
        <Field label="Source repository" htmlFor={`${id}-source`} error={err('source_repo')} hint="owner/name or the repository URL">
          <Input id={`${id}-source`} value={source} onChange={(e) => setSource(e.target.value)} placeholder="octo-org/hello-world" required invalid={!!err('source_repo')} />
        </Field>
        <Field label="Access token" htmlFor={`${id}-token`} error={err('token')} hint="Read access to the repository (classic token with repo scope for private ones). Stored encrypted, never shown again.">
          <Input id={`${id}-token`} type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value)} invalid={!!err('token')} />
        </Field>
      </fieldset>

      <fieldset className={s.fieldset}>
        <legend className={s.legend}>Destination</legend>
        <div className={s.row}>
          {fixedOwner ? (
            <Field label="Owner">
              <Input value={fixedOwner} readOnly aria-readonly />
            </Field>
          ) : (
            <Field label="Owner" htmlFor={`${id}-owner`} error={err('owner')} hint="Organization or user login">
              <Input id={`${id}-owner`} value={owner} onChange={(e) => setOwner(e.target.value)} required invalid={!!fields.owner} />
            </Field>
          )}
          <Field label="Repository name" htmlFor={`${id}-name`} error={err('name')} hint={repo.includes('/') ? `Default: ${repo.split('/')[1]}` : undefined}>
            <Input id={`${id}-name`} value={name} onChange={(e) => setName(e.target.value)} invalid={!!fields.name} />
          </Field>
        </div>
        <Field label="Visibility" htmlFor={`${id}-vis`}>
          <Select id={`${id}-vis`} value={visibility} onChange={(e) => setVisibility(e.target.value as typeof visibility)}>
            <option value="">Same as the source</option>
            <option value="public">Public</option>
            <option value="private">Private</option>
            <option value="internal">Internal</option>
          </Select>
        </Field>
      </fieldset>

      <fieldset className={s.fieldset}>
        <legend className={s.legend}>What to import</legend>
        <div className={s.checks}>
          {STEP_LABELS.map(([key, label, description]) => (
            <Checkbox key={key} label={label} description={description} checked={steps[key]} onChange={(v) => setSteps((st) => ({ ...st, [key]: v }))} disabled={key === 'include_lfs' && !steps.git} />
          ))}
        </div>
      </fieldset>

      <Field
        label="User mapping (optional)"
        htmlFor={`${id}-map`}
        error={err('user_map')}
        hint="One “source-login,local-login” per line. Users are matched by verified email first; anyone unmatched becomes a mannequin that can be reclaimed later."
      >
        <Textarea id={`${id}-map`} className={s.mapInput} value={userMap} onChange={(e) => setUserMap(e.target.value)} placeholder={'octocat,alice\nhubot,bob'} />
      </Field>

      {error && (
        <div className={s.formError} role="alert">
          {error}
        </div>
      )}
      <div className={s.actions}>
        <Button type="submit" variant="primary" disabled={busy || !repo || Object.keys(local).length > 0}>
          {busy ? 'Checking the source…' : 'Start import'}
        </Button>
      </div>
    </form>
  );
}

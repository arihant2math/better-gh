import { observer } from 'mobx-react-lite';
import { useId, useRef, useState } from 'react';
import { invalidate } from '../../api/cache';
import { createImport } from '../../api/imports';
import { session } from '../../app/session';
import { apiFieldErrors, Banner, ButtonRow, Checkbox, FormStack, PageHeader, RadioCards, Section, type FieldErrors } from '../../components/settings/kit';
import { Link, navigate } from '../../router';
import { formatKeys } from '../../shortcuts/manager';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { hasSync, sync } from '../../sync';
import { Avatar } from '../../ui/Badge';
import { Button } from '../../ui/Button';
import { AlertIcon, CheckIcon, ChevronDownIcon, GlobeIcon, LockIcon, OrganizationIcon } from '../../ui/icons';
import { Field, Input, Select } from '../../ui/Input';
import { Menu } from '../../ui/Menu';
import { normalizeRepoName, repoNameError } from './names';
import styles from './New.module.css';
import { useOwners } from './owners';

type Visibility = 'public' | 'private' | 'internal';

const INTERVALS = [
  { value: 60, label: 'Every hour' },
  { value: 240, label: 'Every 4 hours' },
  { value: 480, label: 'Every 8 hours' },
  { value: 1440, label: 'Every day' },
];

/** Client-side check of the source URL (the server re-validates, incl. SSRF rules). */
export function sourceUrlError(raw: string): string | null {
  const s = raw.trim();
  if (!s) return 'Your old repository’s clone URL is required';
  let u: URL;
  try {
    u = new URL(s);
  } catch {
    return 'Enter a valid http:// or https:// clone URL';
  }
  if (u.protocol !== 'http:' && u.protocol !== 'https:') return 'Only http:// and https:// clone URLs are supported';
  if (!u.hostname) return 'Enter a valid http:// or https:// clone URL';
  return null;
}

/** Repository name suggested by a clone URL (`…/name.git` → `name`). */
export function nameFromUrl(raw: string): string {
  try {
    const path = new URL(raw.trim()).pathname.replace(/\/+$/, '');
    return path.split('/').pop()?.replace(/\.git$/i, '') ?? '';
  } catch {
    return '';
  }
}

/** `/new/import`: import a repository from another git host (optionally as a pull mirror). */
export default observer(function ImportRepoPage() {
  const owners = useOwners();
  const me = session.user;
  const [ownerLogin, setOwnerLogin] = useState(me?.login ?? '');
  const owner = owners.find((o) => o.login === ownerLogin) ?? owners[0];
  const [sourceUrl, setSourceUrl] = useState('');
  const [username, setUsername] = useState('');
  const [secret, setSecret] = useState('');
  const [rawName, setRawName] = useState('');
  const [chosenVisibility, setVisibility] = useState<Visibility>('private');
  const [mirror, setMirror] = useState(false);
  const [interval, setIntervalMinutes] = useState(480);
  const [lfs, setLfs] = useState(false);
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [formError, setFormError] = useState<string | null>(null);
  const ownerBtn = useRef<HTMLButtonElement>(null);
  const urlRef = useRef<HTMLInputElement>(null);
  const [ownerMenu, setOwnerMenu] = useState(false);
  const ids = { url: useId(), user: useId(), secret: useId(), name: useId(), interval: useId() };

  const visibility: Visibility = chosenVisibility === 'internal' && !owner?.isOrg ? 'private' : chosenVisibility;
  const suggested = nameFromUrl(sourceUrl);
  const name = normalizeRepoName(rawName || suggested);
  const urlErr = sourceUrlError(sourceUrl);
  const nameErr = name ? repoNameError(name) : 'Repository name is required';
  const urlError = errors.source_url ?? (touched ? urlErr : null);
  const nameError = errors.name ?? (touched || rawName ? nameErr : null);

  const submit = async () => {
    setTouched(true);
    if (busy || !owner) return;
    if (urlErr) {
      urlRef.current?.focus();
      return;
    }
    if (nameErr) return;
    setBusy(true);
    setErrors({});
    setFormError(null);
    try {
      const imp = await createImport({
        source_url: sourceUrl.trim(),
        username: username.trim() || undefined,
        password_or_token: secret || undefined,
        owner: owner.login,
        name,
        visibility,
        mirror,
        include_lfs: lfs,
        mirror_interval_minutes: mirror ? interval : undefined,
      });
      setSecret('');
      invalidate('profile:repos:');
      invalidate('profile:org-repos:');
      if (hasSync()) await Promise.race([sync().ensureScope(`repo:${imp.repository.id}`).catch(() => false), new Promise((r) => setTimeout(r, 1500))]);
      navigate(`/${imp.repository.owner}/${imp.repository.name}/import`);
    } catch (e) {
      const { message, fields } = apiFieldErrors(e);
      setErrors(fields);
      if (!fields.source_url && !fields.name) setFormError(message);
      setBusy(false);
    }
  };

  useShortcuts('Import repository', {
    'mod+enter': { handler: () => void submit(), description: 'Begin import', group: 'Forms', allowInInput: true },
  });

  if (!me) return null;
  const visibilityOptions = [
    { value: 'public' as const, label: 'Public', icon: GlobeIcon, description: 'Anyone on the internet can see this repository. You choose who can commit.' },
    ...(owner?.isOrg ? [{ value: 'internal' as const, label: 'Internal', icon: OrganizationIcon, description: `Members of ${owner.login} can see this repository.` }] : []),
    { value: 'private' as const, label: 'Private', icon: LockIcon, description: 'You choose who can see and commit to this repository.' },
  ];

  return (
    <div className={styles.page}>
      <PageHeader
        title="Import your project"
        description={
          <>
            Import all the files, branches and tags of a repository from another git host, or keep it in sync as a read-only mirror. Want to start from scratch?{' '}
            <Link to="/new">Create a new repository</Link>.
          </>
        }
      />
      <form
        className={styles.form}
        noValidate
        aria-label="Import repository"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Section title="Your old repository’s clone URL">
          <FormStack wide>
            <Field label="Clone URL" htmlFor={ids.url} error={urlError} hint="An http:// or https:// URL, e.g. https://github.com/owner/project.git">
              <Input
                ref={urlRef}
                id={ids.url}
                type="url"
                value={sourceUrl}
                autoFocus
                spellCheck={false}
                autoComplete="off"
                placeholder="https://github.com/owner/project.git"
                invalid={!!urlError}
                onChange={(e) => {
                  setSourceUrl(e.target.value);
                  if (errors.source_url) setErrors((x) => ({ ...x, source_url: undefined }));
                }}
                onBlur={() => sourceUrl && setTouched(true)}
              />
            </Field>
            <div className={styles.credentials}>
              <Field label="Username (optional)" htmlFor={ids.user}>
                <Input id={ids.user} value={username} autoComplete="off" spellCheck={false} onChange={(e) => setUsername(e.target.value)} />
              </Field>
              <Field label="Password or access token (optional)" htmlFor={ids.secret} hint="Stored encrypted, used only to fetch from this URL.">
                <Input id={ids.secret} type="password" value={secret} autoComplete="new-password" onChange={(e) => setSecret(e.target.value)} />
              </Field>
            </div>
          </FormStack>
        </Section>

        <Section title="Your new repository details">
          <FormStack wide>
            <div className={styles.ownerName}>
              <div className={styles.ownerCol}>
                <span className={styles.label} id={`${ids.name}-owner`}>
                  Owner <span aria-hidden>*</span>
                </span>
                <button
                  ref={ownerBtn}
                  type="button"
                  className={styles.ownerButton}
                  aria-haspopup="menu"
                  aria-expanded={ownerMenu}
                  aria-labelledby={`${ids.name}-owner ${ids.name}-owner-value`}
                  onClick={() => setOwnerMenu((v) => !v)}
                >
                  <Avatar user={owner} size={20} square={owner?.isOrg} />
                  <span id={`${ids.name}-owner-value`}>{owner?.login}</span>
                  <ChevronDownIcon size={16} />
                </button>
                <Menu
                  open={ownerMenu}
                  onClose={() => setOwnerMenu(false)}
                  anchor={ownerBtn}
                  aria-label="Choose an owner"
                  items={owners.map((o) => ({
                    id: o.login,
                    text: o.login,
                    label: o.login,
                    description: o.isOrg ? 'Organization' : 'Your personal account',
                    leading: <Avatar user={o} size={20} square={o.isOrg} />,
                    trailing: o.login === owner?.login ? <CheckIcon size={16} /> : undefined,
                    onSelect: () => {
                      setOwnerLogin(o.login);
                      setErrors({});
                    },
                  }))}
                />
              </div>
              <span className={styles.slash} aria-hidden>
                /
              </span>
              <div className={styles.nameCol}>
                <label className={styles.label} htmlFor={ids.name}>
                  Repository name <span aria-hidden>*</span>
                </label>
                <Input
                  id={ids.name}
                  value={rawName}
                  placeholder={suggested || undefined}
                  autoComplete="off"
                  spellCheck={false}
                  maxLength={100}
                  invalid={!!nameError}
                  aria-describedby={`${ids.name}-status`}
                  onChange={(e) => {
                    setRawName(e.target.value);
                    if (errors.name) setErrors((x) => ({ ...x, name: undefined }));
                  }}
                />
              </div>
            </div>
            <div id={`${ids.name}-status`} className={styles.nameStatus} aria-live="polite">
              {nameError ? (
                <span className={styles.bad}>
                  <AlertIcon size={14} /> {nameError}
                </span>
              ) : !rawName && suggested ? (
                <span className={styles.hint}>
                  The repository will be named <strong>{name}</strong>.
                </span>
              ) : null}
            </div>
            <RadioCards aria-label="Visibility" value={visibility} onChange={setVisibility} options={visibilityOptions} />
          </FormStack>
        </Section>

        <Section title="Options">
          <FormStack wide>
            <Checkbox
              checked={mirror}
              onChange={setMirror}
              label="Mirror the repository"
              description="Keep this repository in sync with the source on a schedule. Mirrors are read-only: pushes are refused until you convert it to a regular repository."
            />
            {mirror && (
              <Field label="Sync interval" htmlFor={ids.interval}>
                <Select id={ids.interval} value={String(interval)} onChange={(e) => setIntervalMinutes(Number(e.target.value))}>
                  {INTERVALS.map((i) => (
                    <option key={i.value} value={i.value}>
                      {i.label}
                    </option>
                  ))}
                </Select>
              </Field>
            )}
            <Checkbox checked={lfs} onChange={setLfs} label="Include Git LFS objects" description="Download the large files tracked with Git LFS as well." />
          </FormStack>
        </Section>

        <div className={styles.footer}>
          {formError && (
            <Banner tone="danger" icon={AlertIcon}>
              {formError}
            </Banner>
          )}
          <ButtonRow end>
            <Button onClick={() => navigate('/new')}>Cancel</Button>
            <Button type="submit" variant="primary" loading={busy} disabled={busy || !owner} kbd={formatKeys('mod+enter').join('')}>
              Begin import
            </Button>
          </ButtonRow>
        </div>
      </form>
    </div>
  );
});

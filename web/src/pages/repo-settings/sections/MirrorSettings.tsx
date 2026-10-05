import { useId, useState } from 'react';
import { convertMirror, getMirror, syncMirror, updateMirror, type Mirror, type MirrorUpdate } from '../../../api/imports';
import { apiFieldErrors, Banner, ButtonRow, Checkbox, ConfirmDialog, FormStack, PageHeader, Pill, Section, type FieldErrors } from '../../../components/settings/kit';
import { navigate } from '../../../router';
import { Button } from '../../../ui/Button';
import { AlertIcon, SyncIcon } from '../../../ui/icons';
import { Field, Input, Select } from '../../../ui/Input';
import { RelativeTime } from '../../../ui/RelativeTime';
import { toast } from '../../../ui/Toast';
import styles from '../RepoSettings.module.css';
import { ListSkeleton, LoadError, repoKey, useLocalResource, type SectionProps } from '../shared';

const INTERVALS = [10, 30, 60, 240, 480, 1440, 10080];

function intervalLabel(m: number): string {
  if (m % 1440 === 0) return m === 1440 ? 'Every day' : m === 10080 ? 'Every week' : `Every ${m / 1440} days`;
  if (m % 60 === 0) return m === 60 ? 'Every hour' : `Every ${m / 60} hours`;
  return `Every ${m} minutes`;
}

/** `/settings/mirror`: pull mirror configuration (only for mirrors). */
export default function MirrorSettings({ repo, base }: SectionProps) {
  const mirror = useLocalResource(repoKey(repo, 'mirror'), () => getMirror(repo.owner, repo.name));
  const [converting, setConverting] = useState(false);
  const [syncing, setSyncing] = useState(false);

  if (!repo.mirrorUrl) {
    return (
      <>
        <PageHeader title="Mirror" />
        <Banner>This repository is not a mirror.</Banner>
      </>
    );
  }
  const m = mirror.data;

  const syncNow = async () => {
    setSyncing(true);
    try {
      const next = await syncMirror(repo.owner, repo.name);
      mirror.update(() => next);
      toast({ kind: 'success', title: 'Sync started', description: 'The mirror is fetching from its source.' });
    } catch (e) {
      toast({ kind: 'error', title: apiFieldErrors(e).message });
    } finally {
      setSyncing(false);
    }
  };

  return (
    <>
      <PageHeader
        title="Mirror"
        description="This repository is a read-only pull mirror: it fetches every branch and tag from its source on a schedule, and refuses pushes."
        actions={
          <Button size="sm" leadingIcon={SyncIcon} loading={syncing} onClick={() => void syncNow()}>
            Sync now
          </Button>
        }
      />
      {mirror.error ? <LoadError error={mirror.error} /> : null}
      {!m ? (
        mirror.error ? null : <ListSkeleton rows={3} />
      ) : (
        <>
          <Section title="Status">
            <dl className={styles.mirrorStatus}>
              <dt>Last sync</dt>
              <dd>
                {m.last_sync_at ? <RelativeTime date={m.last_sync_at} /> : 'Never'}{' '}
                {m.last_status === 'success' ? <Pill tone="success">Succeeded</Pill> : m.last_status === 'failed' ? <Pill tone="danger">Failed</Pill> : <Pill>Pending</Pill>}
                {m.syncing && <Pill tone="accent">Sync queued</Pill>}
              </dd>
              <dt>Next sync</dt>
              <dd>{m.enabled && m.next_sync_at ? <RelativeTime date={m.next_sync_at} /> : 'Paused'}</dd>
            </dl>
            {m.last_status === 'failed' && m.last_error && (
              <Banner tone="danger" icon={AlertIcon}>
                <strong>Last sync failed{m.consecutive_failures > 1 ? ` (${m.consecutive_failures} times in a row)` : ''}:</strong>{' '}
                <span className={styles.mono}>{m.last_error}</span>
              </Banner>
            )}
          </Section>
          <MirrorForm repo={repo} mirror={m} onSaved={(next) => mirror.update(() => next)} />
        </>
      )}
      <Section title="Danger Zone" danger>
        <div className={styles.box}>
          <div className={styles.boxRow}>
            <div className={styles.boxText}>
              <span className={styles.dangerTitle}>Convert to a regular repository</span>
              <span className={styles.small}>Stop mirroring. The repository keeps its contents and becomes writable; it no longer follows its source.</span>
            </div>
            <Button variant="danger" size="sm" onClick={() => setConverting(true)}>
              Convert repository
            </Button>
          </div>
        </div>
      </Section>
      <ConfirmDialog
        open={converting}
        onClose={() => setConverting(false)}
        title="Convert to a regular repository?"
        confirmLabel="Stop mirroring"
        confirmText={`${repo.owner}/${repo.name}`}
        onConfirm={async () => {
          await convertMirror(repo.owner, repo.name);
          toast({ kind: 'success', title: `${repo.owner}/${repo.name} is no longer a mirror` });
          navigate(base);
        }}
      >
        <p className={styles.muted}>Scheduled syncs stop and pushes are accepted. This can’t be undone; you would need to import the repository again.</p>
      </ConfirmDialog>
    </>
  );
}

function MirrorForm({ repo, mirror, onSaved }: { repo: SectionProps['repo']; mirror: Mirror; onSaved: (m: Mirror) => void }) {
  const ids = { url: useId(), user: useId(), secret: useId(), interval: useId() };
  const [url, setUrl] = useState(mirror.url);
  const [interval, setIntervalMinutes] = useState(mirror.interval_minutes);
  const [enabled, setEnabled] = useState(mirror.enabled);
  const [lfs, setLfs] = useState(mirror.include_lfs);
  const [username, setUsername] = useState('');
  const [secret, setSecret] = useState('');
  const [clear, setClear] = useState(false);
  const [busy, setBusy] = useState(false);
  const [errors, setErrors] = useState<FieldErrors>({});
  const [formError, setFormError] = useState<string | null>(null);
  const options = INTERVALS.includes(interval) ? INTERVALS : [...INTERVALS, interval].sort((a, b) => a - b);

  const patch: MirrorUpdate = {};
  if (url.trim() !== mirror.url) patch.url = url.trim();
  if (interval !== mirror.interval_minutes) patch.interval_minutes = interval;
  if (enabled !== mirror.enabled) patch.enabled = enabled;
  if (lfs !== mirror.include_lfs) patch.include_lfs = lfs;
  if (secret) {
    patch.password_or_token = secret;
    if (username.trim()) patch.username = username.trim();
  } else if (clear) patch.clear_credentials = true;
  const dirty = Object.keys(patch).length > 0;

  const save = async () => {
    if (!dirty || busy) return;
    setBusy(true);
    setErrors({});
    setFormError(null);
    try {
      const next = await updateMirror(repo.owner, repo.name, patch);
      onSaved(next);
      setSecret('');
      setUsername('');
      setClear(false);
      toast({ kind: 'success', title: 'Mirror settings saved' });
    } catch (e) {
      const { message, fields } = apiFieldErrors(e);
      setErrors(fields);
      if (!fields.url && !fields.interval_minutes) setFormError(message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Section title="Settings">
      <form
        aria-label="Mirror settings"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <FormStack>
          <Field label="Source URL" htmlFor={ids.url} error={errors.url}>
            <Input id={ids.url} type="url" value={url} spellCheck={false} autoComplete="off" invalid={!!errors.url} onChange={(e) => setUrl(e.target.value)} />
          </Field>
          <Field label="Sync interval" htmlFor={ids.interval} error={errors.interval_minutes}>
            <Select id={ids.interval} value={String(interval)} onChange={(e) => setIntervalMinutes(Number(e.target.value))}>
              {options.map((v) => (
                <option key={v} value={v}>
                  {intervalLabel(v)}
                </option>
              ))}
            </Select>
          </Field>
          <Checkbox checked={enabled} onChange={setEnabled} label="Sync automatically" description="Uncheck to pause scheduled syncs (Sync now still works)." />
          <Checkbox checked={lfs} onChange={setLfs} label="Include Git LFS objects" />
          <Field
            label="Credentials"
            htmlFor={ids.secret}
            hint={mirror.has_credentials ? 'Credentials are stored (encrypted). Enter new ones to replace them.' : 'No credentials stored; the source is fetched anonymously.'}
          >
            <div className={styles.row}>
              <Input id={ids.user} aria-label="Username" placeholder="Username" value={username} autoComplete="off" onChange={(e) => setUsername(e.target.value)} />
              <Input id={ids.secret} type="password" placeholder="Password or token" value={secret} autoComplete="new-password" onChange={(e) => setSecret(e.target.value)} />
            </div>
          </Field>
          {mirror.has_credentials && !secret && <Checkbox checked={clear} onChange={setClear} label="Remove stored credentials" />}
          {formError && <Banner tone="danger">{formError}</Banner>}
          <ButtonRow>
            <Button type="submit" variant="primary" loading={busy} disabled={!dirty}>
              Save mirror settings
            </Button>
          </ButtonRow>
        </FormStack>
      </form>
    </Section>
  );
}

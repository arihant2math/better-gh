import { useState } from 'react';
import { mutate, refresh, useResource } from '../../../api/cache';
import {
  alertsPrefix,
  getScanHistory,
  getSettings,
  ssKeys,
  startScan,
  status,
  updateSecurityAndAnalysis,
  type Scan,
  type ScanHistory,
  type SecretScanningSettings,
  type SecurityAndAnalysisPatch,
} from '../../../api/secretScanning';
import { Banner, PageHeader, Section, Toggle, errorMessage } from '../../../components/settings/kit';
import { formatDateTime } from '../../../components/admin/format';
import { invalidateLists } from '../../../components/admin/usePagedList';
import { Link } from '../../../router';
import { Button } from '../../../ui/Button';
import { SyncIcon } from '../../../ui/icons';
import { toast } from '../../../ui/Toast';
import { CustomPatternsPanel } from '../../security/CustomPatternsPanel';
import sec from '../../security/Security.module.css';
import styles from '../RepoSettings.module.css';
import { ListSkeleton, LoadError, type SectionProps } from '../shared';

type Field = 'secret_scanning' | 'push_protection' | 'non_provider_patterns';

const PATCH_KEY: Record<Field, keyof SecurityAndAnalysisPatch> = {
  secret_scanning: 'secret_scanning',
  push_protection: 'secret_scanning_push_protection',
  non_provider_patterns: 'secret_scanning_non_provider_patterns',
};

const ENFORCED = 'Enforced by the site administrator';

/** `/settings/security_analysis`: secret scanning, push protection, custom patterns and scans. */
export default function CodeSecuritySettings({ repo }: SectionProps) {
  const { owner, name } = repo;
  const key = ssKeys.settings(owner, name);
  const settings = useResource(key, () => getSettings(owner, name));
  const [saving, setSaving] = useState<Field | null>(null);
  const s = settings.data;

  const set = async (field: Field, on: boolean) => {
    if (!s || saving) return;
    const prev = s;
    const next: SecretScanningSettings = { ...s, [field]: on };
    if (field === 'secret_scanning' && !on) Object.assign(next, { push_protection: s.enforced_by_site.push_protection, non_provider_patterns: false });
    mutate<SecretScanningSettings>(key, () => next); // optimistic
    setSaving(field);
    try {
      await updateSecurityAndAnalysis(owner, name, { [PATCH_KEY[field]]: status(on) });
      void refresh(key, () => getSettings(owner, name)).catch(() => undefined);
      invalidateLists(alertsPrefix(owner, name));
      if (field === 'secret_scanning' && on) void refresh(ssKeys.scanHistory(owner, name), () => getScanHistory(owner, name)).catch(() => undefined);
      toast({ kind: 'success', title: `${LABELS[field]} ${on ? 'enabled' : 'disabled'}` });
    } catch (e) {
      mutate<SecretScanningSettings>(key, () => prev);
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      setSaving(null);
    }
  };

  return (
    <>
      <PageHeader title="Code security" description="Find secrets pushed to this repository and stop new ones before they land." />
      {settings.error ? (
        <LoadError error={settings.error} />
      ) : !s ? (
        <ListSkeleton rows={3} />
      ) : !s.available ? (
        <Banner>Secret scanning is not available on this instance.</Banner>
      ) : (
        <>
          <Section title="Secret Protection">
            <Toggle
              label="Secret scanning"
              description={
                s.enforced_by_site.secret_scanning
                  ? ENFORCED
                  : 'Receive alerts for secrets such as API keys and tokens found in this repository’s history and new pushes.'
              }
              checked={s.secret_scanning}
              disabled={s.enforced_by_site.secret_scanning || !!saving}
              onChange={(v) => void set('secret_scanning', v)}
            />
            <Toggle
              label="Push protection"
              description={
                s.enforced_by_site.push_protection
                  ? ENFORCED
                  : s.secret_scanning
                    ? 'Block pushes that contain supported secrets. Contributors can bypass the block with a reason.'
                    : 'Requires secret scanning.'
              }
              checked={s.push_protection}
              disabled={!s.secret_scanning || s.enforced_by_site.push_protection || !!saving}
              onChange={(v) => void set('push_protection', v)}
            />
            <Toggle
              label="Non-provider patterns"
              description={s.secret_scanning ? 'Also scan for generic secrets such as private keys and HTTP basic auth headers.' : 'Requires secret scanning.'}
              checked={s.non_provider_patterns}
              disabled={!s.secret_scanning || !!saving}
              onChange={(v) => void set('non_provider_patterns', v)}
            />
            {s.secret_scanning && (
              <p className={styles.muted}>
                <Link to={`/${owner}/${name}/security/secret-scanning`}>View secret scanning alerts</Link>
              </p>
            )}
          </Section>
          <ScanSection owner={owner} name={name} enabled={s.secret_scanning} />
          <CustomPatternsPanel scope={{ kind: 'repo', owner, repo: name }} />
        </>
      )}
    </>
  );
}

const LABELS: Record<Field, string> = { secret_scanning: 'Secret scanning', push_protection: 'Push protection', non_provider_patterns: 'Non-provider patterns' };

const SCAN_KINDS: { key: keyof ScanHistory; label: string }[] = [
  { key: 'incremental_scans', label: 'Incremental' },
  { key: 'backfill_scans', label: 'History (backfill)' },
  { key: 'pattern_update_scans', label: 'Pattern update' },
  { key: 'custom_pattern_backfill_scans', label: 'Custom pattern' },
];

function ScanSection({ owner, name, enabled }: { owner: string; name: string; enabled: boolean }) {
  const key = ssKeys.scanHistory(owner, name);
  const history = useResource(enabled ? key : null, () => getScanHistory(owner, name));
  const [busy, setBusy] = useState(false);
  const reload = () => void refresh(key, () => getScanHistory(owner, name)).catch(() => undefined);

  const scan = async () => {
    setBusy(true);
    try {
      await startScan(owner, name);
      toast({ kind: 'success', title: 'Scan queued', description: 'The whole git history of the repository is being scanned.' });
      reload();
      setTimeout(reload, 2500);
    } catch (e) {
      toast({ kind: 'error', title: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  const rows: (Scan & { kind: string })[] = history.data
    ? SCAN_KINDS.flatMap((k) => (history.data![k.key] ?? []).map((s) => ({ ...s, kind: k.label })))
        .sort((a, b) => (b.started_at ?? '').localeCompare(a.started_at ?? ''))
        .slice(0, 10)
    : [];

  return (
    <Section
      title="Scans"
      description="New pushes are scanned as they arrive. Scan the full history again after enabling patterns or importing old commits."
      actions={
        <Button size="sm" leadingIcon={SyncIcon} loading={busy} disabled={!enabled} onClick={() => void scan()}>
          Scan repository history now
        </Button>
      }
    >
      {!enabled ? (
        <p className={styles.muted}>Enable secret scanning to scan this repository.</p>
      ) : history.error ? (
        <LoadError error={history.error} />
      ) : !history.data ? (
        <ListSkeleton rows={2} />
      ) : rows.length === 0 ? (
        <p className={styles.muted}>No scans yet.</p>
      ) : (
        <div className={sec.box}>
          <table className={sec.scanTable} aria-label="Recent scans">
            <thead>
              <tr>
                <th>Scan</th>
                <th>Status</th>
                <th>Started</th>
                <th>Completed</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={i}>
                  <td>{r.kind}</td>
                  <td>{r.status === 'completed' ? 'Completed' : r.status === 'pending' ? 'In progress' : r.status}</td>
                  <td>{r.started_at ? formatDateTime(r.started_at) : '—'}</td>
                  <td>{r.completed_at ? formatDateTime(r.completed_at) : '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Section>
  );
}

/**
 * "All repositories" / "Only select repositories" with a repository picker.
 * `publicOption` adds a leading "Public repositories (read-only)" choice
 * (fine-grained personal access tokens); `mode` is then also `'public'`.
 */
import { useMemo, useState } from 'react';
import { listAccountRepos, type SimpleUser } from '../../api/apps';
import { useResource } from '../../api/cache';
import { session } from '../../app/session';
import { RadioCards } from '../../components/settings/kit';
import { IconButton } from '../../ui/Button';
import { LockIcon, RepoIcon, SearchIcon, XIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import styles from './apps.module.css';

export interface PickedRepo {
  id: number;
  full_name: string;
  private: boolean;
}

export interface RepoSelection {
  mode: 'all' | 'selected';
  repos: PickedRepo[];
}

export function RepoAccessPicker<S extends { mode: string; repos: PickedRepo[] } = RepoSelection>({
  account,
  value,
  onChange,
  publicOption,
}: {
  account: SimpleUser;
  value: S;
  onChange: (v: S) => void;
  publicOption?: { label: string; description: string };
}) {
  const me = session.user?.login ?? '';
  const res = useResource(value.mode === 'selected' ? `apps:repos:${account.login}` : null, () => listAccountRepos(account, me));
  const [q, setQ] = useState('');
  const picked = new Set(value.repos.map((r) => r.id));
  const matches = useMemo(() => {
    const needle = q.trim().toLowerCase();
    return (res.data ?? []).filter((r) => !picked.has(r.id) && (!needle || r.full_name.toLowerCase().includes(needle))).slice(0, 8);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [res.data, q, value.repos]);
  return (
    <div className={styles.picker}>
      <RadioCards
        aria-label="Repository access"
        value={value.mode}
        onChange={(mode) => onChange({ ...value, mode })}
        options={[
          ...(publicOption ? [{ value: 'public', ...publicOption }] : []),
          { value: 'all', label: 'All repositories', description: `Applies to all current and future repositories owned by ${account.login}.` },
          { value: 'selected', label: 'Only select repositories', description: 'Select at least one repository.' },
        ]}
      />
      {value.mode === 'selected' && (
        <>
          <Input
            leadingIcon={SearchIcon}
            placeholder={res.loading ? 'Loading repositories…' : 'Search for a repository'}
            aria-label="Search repositories"
            value={q}
            onChange={(e) => setQ(e.target.value)}
          />
          {q && (
            <ul className={styles.repoResults} role="listbox" aria-label="Matching repositories">
              {matches.length === 0 && <li className={styles.hint}>No matching repositories.</li>}
              {matches.map((r) => (
                <li key={r.id}>
                  <button
                    type="button"
                    role="option"
                    aria-selected={false}
                    onClick={() => {
                      onChange({ ...value, repos: [...value.repos, { id: r.id, full_name: r.full_name, private: r.private }] });
                      setQ('');
                    }}
                  >
                    {r.private ? <LockIcon size={14} /> : <RepoIcon size={14} />} {r.full_name}
                  </button>
                </li>
              ))}
            </ul>
          )}
          <ul className={styles.repoPicked} aria-label="Selected repositories">
            {value.repos.map((r) => (
              <li key={r.id}>
                {r.private ? <LockIcon size={14} /> : <RepoIcon size={14} />}
                <span>{r.full_name}</span>
                <IconButton
                  icon={XIcon}
                  size="sm"
                  label={`Remove ${r.full_name}`}
                  onClick={() => onChange({ ...value, repos: value.repos.filter((x) => x.id !== r.id) })}
                />
              </li>
            ))}
            {value.repos.length === 0 && <li className={styles.hint}>No repositories selected.</li>}
          </ul>
        </>
      )}
    </div>
  );
}

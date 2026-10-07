import { useRef, useState } from 'react';
import { prefetch, useResource } from '../../api/cache';
import { codeKeys, getFullRepo, type RestFullRepo } from '../../api/code';
import { navigate } from '../../router';
import type { Repo } from '../../sync/models';
import { Button, IconButton, cx } from '../../ui/Button';
import { ChevronDownIcon, CodeIcon, CopyIcon, FileZipIcon, PlusIcon, TerminalIcon, UploadIcon } from '../../ui/icons';
import { Menu } from '../../ui/Menu';
import { Popover } from '../../ui/Popover';
import { toast } from '../../ui/Toast';
import styles from './Code.module.css';
import { codeUrl, copyText, type CodeTarget } from './util';

type CloneTab = 'https' | 'ssh' | 'cli';
const TAB_KEY = 'bgh:clone-tab';

export function useFullRepo(owner: string, repo: string) {
  return useResource<RestFullRepo>(codeKeys.repo(owner, repo), () => getFullRepo(owner, repo), { ttlMs: 60_000 });
}

/** Archive URL for a ref (`refs/heads/x`, `refs/tags/x` or a SHA). */
export function archiveUrl(t: CodeTarget, ext: 'zip' | 'tar.gz'): string {
  const ref = t.kind === 'branch' ? `refs/heads/${t.ref}` : t.kind === 'tag' ? `refs/tags/${t.ref}` : t.ref;
  return `/${t.owner}/${t.repo}/archive/${ref}.${ext}`;
}

/** Green "Code" dropdown: HTTPS / SSH / CLI clone URLs and Download ZIP. */
export function CloneMenu({ repo, t }: { repo: Repo; t: CodeTarget }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const [tab, setTab] = useState<CloneTab>(() => {
    try {
      return (localStorage.getItem(TAB_KEY) as CloneTab | null) ?? 'https';
    } catch {
      return 'https';
    }
  });
  const full = useResource<RestFullRepo>(open ? codeKeys.repo(repo.owner, repo.name) : null, () => getFullRepo(repo.owner, repo.name));
  const https = full.data?.clone_url ?? `${window.location.origin}/${repo.owner}/${repo.name}.git`;
  const ssh = full.data?.ssh_url ?? `git@${window.location.hostname}:${repo.owner}/${repo.name}.git`;
  const value = tab === 'https' ? https : tab === 'ssh' ? ssh : `gh repo clone ${repo.owner}/${repo.name}`;
  const pick = (k: CloneTab) => {
    setTab(k);
    try {
      localStorage.setItem(TAB_KEY, k);
    } catch {
      /* ignore */
    }
  };
  return (
    <>
      <Button
        ref={anchor}
        size="sm"
        variant="success"
        leadingIcon={CodeIcon}
        trailingIcon={ChevronDownIcon}
        onClick={() => setOpen((o) => !o)}
        onMouseEnter={() => prefetch(codeKeys.repo(repo.owner, repo.name), () => getFullRepo(repo.owner, repo.name))}
        aria-expanded={open}
      >
        Code
      </Button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={anchor} placement="bottom-end" role="dialog" aria-label="Clone">
        <div className={styles.clone}>
          <div className={styles.cloneTitle}>
            <TerminalIcon size={16} /> Clone
          </div>
          <div className={styles.cloneTabs} role="tablist">
            {(['https', 'ssh', 'cli'] as const).map((k) => (
              <button key={k} type="button" role="tab" aria-selected={tab === k} className={cx(styles.cloneTab, tab === k && styles.cloneTabOn)} onClick={() => pick(k)}>
                {k === 'https' ? 'HTTPS' : k === 'ssh' ? 'SSH' : 'GitHub CLI'}
              </button>
            ))}
          </div>
          <div className={styles.cloneUrl}>
            <input readOnly value={value} aria-label="Clone URL" onFocus={(e) => e.currentTarget.select()} />
            <IconButton icon={CopyIcon} label="Copy URL" size="sm" onClick={() => void copyText(value).then(() => toast({ title: 'Copied to clipboard' }))} />
          </div>
          <p className={styles.cloneHint}>
            {tab === 'https'
              ? 'Clone using the web URL (use a personal access token as the password).'
              : tab === 'ssh'
                ? 'Use a password-protected SSH key.'
                : 'Work fast with the official CLI pointed at this server (GH_HOST).'}
          </p>
          <a className={styles.cloneZip} href={archiveUrl(t, 'zip')} download>
            <FileZipIcon size={16} /> Download ZIP
          </a>
        </div>
      </Popover>
    </>
  );
}

/** "Add file ▾": create a new file / upload files into the current directory. */
export function AddFileMenu({ t }: { t: CodeTarget }) {
  const anchor = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button ref={anchor} size="sm" leadingIcon={PlusIcon} trailingIcon={ChevronDownIcon} onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        Add file
      </Button>
      <Menu
        open={open}
        onClose={() => setOpen(false)}
        anchor={anchor}
        placement="bottom-end"
        aria-label="Add file"
        items={[
          { id: 'new', label: 'Create new file', icon: PlusIcon, onSelect: () => navigate(codeUrl(t, 'new', t.ref, t.path)) },
          { id: 'upload', label: 'Upload files', icon: UploadIcon, onSelect: () => navigate(codeUrl(t, 'upload', t.ref, t.path)) },
        ]}
      />
    </>
  );
}

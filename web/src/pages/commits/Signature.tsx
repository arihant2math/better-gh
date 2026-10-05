/** "Verified" / "Unverified" commit signature badges (P25). */
import { useEffect, useReducer, useRef, useState } from 'react';
import { load, peek } from '../../api/cache';
import { api } from '../../api/client';
import { Link } from '../../router';
import { Avatar } from '../../ui/Badge';
import { cx } from '../../ui/Button';
import { Popover } from '../../ui/Popover';
import { describeSignature } from './signature';
import styles from './Signature.module.css';

/** One signed commit's verification (`GET /_bgh/repos/{o}/{r}/commit-signatures`). */
export interface CommitSignature {
  verified: boolean;
  /** GitHub `verification.reason` (`valid`, `unknown_key`, `bad_email`, …). */
  reason: string;
  key_type: 'gpg' | 'ssh' | 'x509' | 'unknown';
  /** OpenPGP key id or SSH fingerprint named by the signature. */
  key_id: string | null;
  signer: { login: string; avatar_url: string } | null;
  /** Signed by the server's web-flow key (web edits, merges). */
  web_flow: boolean;
}

/** Signed commits only; unsigned SHAs are absent. */
export interface CommitSignatures {
  signatures: Record<string, CommitSignature>;
}

/** Lives here (not `api/code.ts`) to keep it out of the initial bundle. */
export function getCommitSignatures(owner: string, repo: string, shas: string[]): Promise<CommitSignatures> {
  const q = new URLSearchParams();
  for (const s of shas) q.append('sha', s);
  return api.get<CommitSignatures>(`/_bgh/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/commit-signatures?${q}`);
}

const signaturesKey = (o: string, r: string, shas: string[]) => `sig:${o}/${r}:${shas.join(',')}`;

/** The endpoint's batch limit. */
const BATCH = 100;

/**
 * Signature info for `lists` of SHAs (e.g. one list per loaded page), one
 * request per list (split at 100; signed commits only; not immutable: a key
 * upload can verify them). Re-renders when a batch arrives.
 */
export function useSignatures(owner: string, repo: string, lists: string[][]): Record<string, CommitSignature> {
  const [, bump] = useReducer((x: number) => x + 1, 0);
  const batches: string[][] = [];
  for (const shas of lists) for (let i = 0; i < shas.length; i += BATCH) batches.push(shas.slice(i, i + BATCH));
  const keys = batches.map((b) => signaturesKey(owner, repo, b));
  const joined = keys.join('|');
  useEffect(() => {
    let alive = true;
    batches.forEach((b, i) => {
      load(keys[i]!, () => getCommitSignatures(owner, repo, b)).then(
        () => alive && bump(),
        () => undefined,
      );
    });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- keyed by the joined cache keys
  }, [joined]);
  const merged: Record<string, CommitSignature> = {};
  for (const k of keys) Object.assign(merged, peek<CommitSignatures>(k)?.signatures);
  return merged;
}

/** Badge with a details popover; nothing for unsigned commits. */
export function SignatureBadge({ signature, size = 'sm' }: { signature: CommitSignature | undefined; size?: 'sm' | 'md' }) {
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  if (!signature) return null;
  const t = describeSignature(signature);
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={cx(styles.badge, signature.verified ? styles.verified : styles.unverified, size === 'md' && styles.md)}
        aria-expanded={open}
        aria-label={`${t.label} signature: details`}
        onClick={(e) => {
          e.preventDefault();
          e.stopPropagation();
          setOpen((o) => !o);
        }}
      >
        {t.label}
      </button>
      <Popover open={open} onClose={() => setOpen(false)} anchor={anchor} placement="bottom-end" className={styles.pop} role="dialog" aria-label={`${t.label} signature`}>
        <div className={styles.popBody} onClick={(e) => e.stopPropagation()}>
          <div className={cx(styles.title, signature.verified ? styles.titleOk : styles.titleBad)}>{t.label} signature</div>
          <p className={styles.detail}>{t.detail}</p>
          {signature.signer && (
            <div className={styles.signer}>
              <Avatar user={{ login: signature.signer.login, avatarUrl: signature.signer.avatar_url }} size={20} />
              <Link to={`/${signature.signer.login}`}>{signature.signer.login}</Link>
            </div>
          )}
          {t.key && <div className={styles.key}>{t.key}</div>}
          {signature.web_flow && (
            <a className={styles.help} href="/web-flow.gpg" target="_blank" rel="noreferrer">
              Download the web-flow public key
            </a>
          )}
        </div>
      </Popover>
    </>
  );
}

import { useEffect, useId, useMemo, useState, type FormEvent, type ReactNode } from 'react';
import { session } from '../../app/session';
import { formatKeys } from '../../shortcuts/manager';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { AlertIcon, GitBranchIcon, GitCommitIcon, GitPullRequestIcon } from '../../ui/icons';
import { Field, Input, Textarea } from '../../ui/Input';
import { useRefs } from './RefPicker';
import styles from './CommitDialog.module.css';

export interface CommitRequest {
  /** Summary line (falls back to the default message). */
  message: string;
  /** Optional extended description. */
  description: string;
  /** `message` + blank line + `description` (what goes into the commit). */
  fullMessage: string;
  /** The branch being edited (base of a new branch). */
  branch: string;
  /** Name of the branch to create first, or `null` to commit directly to `branch`. */
  newBranch: string | null;
}

export type CommitErrorKind = 'conflict' | 'protected' | 'exists' | 'invalid' | 'forbidden' | 'other';

/** Error thrown from `onCommit`; `protected` switches the dialog to "new branch". */
export class CommitError extends Error {
  constructor(
    message: string,
    readonly kind: CommitErrorKind,
  ) {
    super(message);
  }
}

export interface CommitDialogProps {
  open: boolean;
  onClose: () => void;
  owner: string;
  repo: string;
  /** Branch being edited. */
  branch: string;
  /** Prefilled summary, e.g. "Update README.md". */
  defaultMessage: string;
  /** False when direct commits are impossible (protected branch, tag or commit). */
  canCommitDirectly?: boolean;
  /** Why direct commits are unavailable (shown under the disabled option). */
  directBlockedReason?: string;
  title?: string;
  submitLabel?: string;
  /** Extra content above the form (e.g. upload progress). */
  children?: ReactNode;
  onCommit: (req: CommitRequest) => Promise<void>;
}

/** Git ref name rules (subset of `git check-ref-format`). */
export function branchNameProblem(name: string, existing: Iterable<string> = []): string | null {
  if (!name) return 'Enter a branch name';
  if (/[\s~^:?*[\\]|\.\.|@\{|\/\/|^[/.-]|[/.]$|\.lock$|\/\./.test(name) || name === '@') return 'Not a valid branch name';
  for (const b of existing) if (b === name) return `A branch named ${name} already exists`;
  return null;
}

/** First free `{login}-patch-{n}`. */
export function suggestBranchName(login: string, existing: Iterable<string>): string {
  const taken = new Set(existing);
  const base = `${(login || 'user').replace(/[^\w.-]/g, '-')}-patch-`;
  let n = 1;
  while (taken.has(`${base}${n}`)) n++;
  return `${base}${n}`;
}

/**
 * Commit dialog shared by edit / new / delete / upload: summary +
 * description, "commit directly" vs "new branch + pull request", ⌘↵ submits.
 */
export function CommitDialog(props: CommitDialogProps) {
  // Remount per opening so the form starts fresh.
  return props.open ? <CommitForm {...props} /> : null;
}

function CommitForm({
  open,
  onClose,
  owner,
  repo,
  branch,
  defaultMessage,
  canCommitDirectly = true,
  directBlockedReason,
  title = 'Commit changes',
  submitLabel = 'Commit changes',
  children,
  onCommit,
}: CommitDialogProps) {
  const id = useId();
  const refs = useRefs(owner, repo);
  const branchNames = useMemo(() => refs.data?.branches.map((b) => b.name) ?? [], [refs.data]);
  const [message, setMessage] = useState('');
  const [description, setDescription] = useState('');
  const [blocked, setBlocked] = useState<string | null>(canCommitDirectly ? null : (directBlockedReason ?? 'You can’t commit directly to this ref.'));
  const [mode, setMode] = useState<'direct' | 'new'>(canCommitDirectly ? 'direct' : 'new');
  const [newBranch, setNewBranch] = useState<string | null>(null);
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const suggested = suggestBranchName(session.user?.login ?? '', branchNames);
  const branchName = newBranch ?? suggested;
  const branchError = mode === 'new' && (touched || newBranch !== null) ? branchNameProblem(branchName.trim(), branchNames) : null;

  useEffect(() => {
    if (!canCommitDirectly) {
      setMode('new');
      setBlocked(directBlockedReason ?? 'You can’t commit directly to this ref.');
    }
  }, [canCommitDirectly, directBlockedReason]);

  const submit = async (e?: FormEvent) => {
    e?.preventDefault();
    if (busy) return;
    const summary = message.trim() || defaultMessage;
    const nb = mode === 'new' ? branchName.trim() : null;
    if (nb !== null) {
      const problem = branchNameProblem(nb, branchNames);
      if (problem) {
        setTouched(true);
        return;
      }
    }
    const desc = description.trim();
    setBusy(true);
    setError(null);
    try {
      await onCommit({ message: summary, description: desc, fullMessage: desc ? `${summary}\n\n${desc}` : summary, branch, newBranch: nb });
    } catch (err) {
      if (err instanceof CommitError && err.kind === 'protected') {
        setBlocked(err.message);
        setMode('new');
        setError(`${err.message} Commit to a new branch and open a pull request instead.`);
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      setBusy(false);
    }
  };

  useShortcuts(
    'Commit dialog',
    {
      'mod+enter': { handler: () => void submit(), description: 'Commit changes', group: 'Editor' },
      'mod+s': () => void submit(),
    },
    open,
  );

  const disabled = busy || (mode === 'new' && !!branchNameProblem(branchName.trim(), branchNames));

  return (
    <Dialog
      open={open}
      onClose={() => !busy && onClose()}
      title={title}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button
            variant={mode === 'new' ? 'primary' : 'success'}
            type="submit"
            form={`${id}-form`}
            loading={busy}
            disabled={disabled}
            leadingIcon={mode === 'new' ? GitPullRequestIcon : GitCommitIcon}
            kbd={formatKeys('mod+enter').join(' ')}
          >
            {mode === 'new' ? 'Propose changes' : submitLabel}
          </Button>
        </>
      }
    >
      <form id={`${id}-form`} className={styles.form} onSubmit={(e) => void submit(e)}>
        {children}
        {error && (
          <div className={styles.error} role="alert">
            <AlertIcon size={16} />
            <span>{error}</span>
          </div>
        )}
        <Field label="Commit message" htmlFor={`${id}-msg`}>
          <Input id={`${id}-msg`} value={message} placeholder={defaultMessage} onChange={(e) => setMessage(e.target.value)} autoFocus maxLength={200} />
        </Field>
        <Field label="Extended description" htmlFor={`${id}-desc`}>
          <Textarea
            id={`${id}-desc`}
            className={styles.desc}
            value={description}
            placeholder="Add an optional extended description…"
            onChange={(e) => setDescription(e.target.value)}
            rows={4}
          />
        </Field>
        <fieldset className={styles.choices}>
          <legend className={styles.legend}>Where should this commit go?</legend>
          <label className={styles.choice} data-disabled={blocked ? '' : undefined}>
            <input type="radio" name={`${id}-mode`} checked={mode === 'direct'} disabled={!!blocked} onChange={() => setMode('direct')} />
            <GitCommitIcon size={16} className={styles.choiceIcon} />
            <span>
              Commit directly to the <code className={styles.branch}>{branch}</code> branch
              {blocked && <span className={styles.choiceHint}>{blocked}</span>}
            </span>
          </label>
          <label className={styles.choice}>
            <input type="radio" name={`${id}-mode`} checked={mode === 'new'} onChange={() => setMode('new')} />
            <GitPullRequestIcon size={16} className={styles.choiceIcon} />
            <span>
              <strong>Create a new branch</strong> for this commit and start a pull request
            </span>
          </label>
          {mode === 'new' && (
            <div className={styles.newBranch}>
              <Input
                aria-label="New branch name"
                leadingIcon={GitBranchIcon}
                value={branchName}
                invalid={!!branchError}
                onChange={(e) => setNewBranch(e.target.value)}
                onBlur={() => setTouched(true)}
                size="sm"
                spellCheck={false}
              />
              {branchError && <div className={styles.fieldError}>{branchError}</div>}
            </div>
          )}
        </fieldset>
      </form>
    </Dialog>
  );
}

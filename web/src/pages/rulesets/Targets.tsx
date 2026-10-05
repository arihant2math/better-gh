import { useId, useMemo, useRef, useState } from 'react';
import type { Repo } from '../../sync/models';
import { Button, IconButton } from '../../ui/Button';
import { PlusIcon, RepoIcon, TriangleDownIcon, XIcon } from '../../ui/icons';
import { Field, Input, Select } from '../../ui/Input';
import { Menu, type MenuEntry } from '../../ui/Menu';
import { Checkbox } from '../../components/settings/kit';
import { ChipInput } from '../repo-settings/shared';
import { useOrgRepos, useRefNames } from './data';
import { selectsRef, selectsRepo } from './match';
import { patternLabel, storedPattern, toInput, type RepoTargeting, type RulesetForm } from './model';
import styles from './Rulesets.module.css';

type Set = (patch: Partial<RulesetForm>) => void;

const PREVIEW_MAX = 50;

/** Include / exclude ref patterns with a live preview of the matching branches or tags. */
export function RefTargets({ form, set, repo, error }: { form: RulesetForm; set: Set; repo?: Repo; error?: string }) {
  const target = form.target === 'tag' ? 'tag' : 'branch';
  const noun = target === 'tag' ? 'tags' : 'branches';
  const [menu, setMenu] = useState(false);
  const [adding, setAdding] = useState<'include' | 'exclude' | null>(null);
  const [text, setText] = useState('');
  const btn = useRef<HTMLButtonElement>(null);
  const inputId = useId();
  const refs = useRefNames(repo, form.target);
  const prefix = target === 'tag' ? 'refs/tags/' : 'refs/heads/';
  const def = repo?.defaultBranch ?? '';
  const names = useMemo(() => refs.data ?? [], [refs.data]);

  const add = (kind: 'include' | 'exclude', raw: string) => {
    const p = storedPattern(raw, target);
    if (!p) return;
    const k = kind === 'include' ? 'refInclude' : 'refExclude';
    if (!form[k].includes(p)) set({ [k]: [...form[k], p] });
  };
  const remove = (kind: 'include' | 'exclude', p: string) => {
    const k = kind === 'include' ? 'refInclude' : 'refExclude';
    set({ [k]: form[k].filter((x) => x !== p) });
  };

  const typedMatches = useMemo(() => {
    const p = storedPattern(text, target);
    if (!p) return [];
    return names.filter((n) => selectsRef({ include: [p], exclude: [] }, target, prefix + n, def));
  }, [text, target, names, prefix, def]);

  const matching = useMemo(
    () => names.filter((n) => selectsRef({ include: form.refInclude, exclude: form.refExclude }, target, prefix + n, def)),
    [names, form.refInclude, form.refExclude, target, prefix, def],
  );

  const items: MenuEntry[] = [
    ...(target === 'branch' ? [{ id: 'default', label: 'Include default branch', onSelect: () => add('include', '~DEFAULT_BRANCH') }] : []),
    { id: 'all', label: `Include all ${noun}`, onSelect: () => add('include', '~ALL') },
    { id: 'inc', label: 'Include by pattern', onSelect: () => setAdding('include') },
    { id: 'exc', label: 'Exclude by pattern', onSelect: () => setAdding('exclude') },
  ];

  const rows = [...form.refInclude.map((p) => ['include', p] as const), ...form.refExclude.map((p) => ['exclude', p] as const)];

  return (
    <div className={styles.rules}>
      <div className={styles.box}>
        <div className={styles.boxHead}>
          <span>Target {noun}</span>
          <span className={styles.spacer} />
          <Button ref={btn} size="sm" leadingIcon={PlusIcon} trailingIcon={TriangleDownIcon} onClick={() => setMenu(true)} aria-haspopup="menu">
            Add target
          </Button>
          <Menu open={menu} onClose={() => setMenu(false)} anchor={btn} items={items} placement="bottom-end" aria-label="Add target" />
        </div>
        {adding && (
          <div className={styles.boxRow}>
            <div className={styles.inlineRow} style={{ flex: 1 }}>
              <div className={styles.grow}>
                <Field
                  label={adding === 'include' ? `Include ${noun} matching` : `Exclude ${noun} matching`}
                  htmlFor={inputId}
                  hint={
                    repo
                      ? text.trim()
                        ? `${typedMatches.length} ${typedMatches.length === 1 ? noun.replace(/es$|s$/, '') : noun} match${typedMatches.length === 1 ? 'es' : ''}${typedMatches.length ? `: ${typedMatches.slice(0, 5).join(', ')}${typedMatches.length > 5 ? '…' : ''}` : ''}`
                        : 'Use * (not across /), ** (any depth) and ? wildcards, e.g. release/*.'
                      : 'Use * (not across /), ** (any depth) and ? wildcards, e.g. release/*.'
                  }
                >
                  <Input
                    id={inputId}
                    autoFocus
                    value={text}
                    placeholder={target === 'tag' ? 'v*' : 'release/*'}
                    spellCheck={false}
                    autoComplete="off"
                    onChange={(e) => setText(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') {
                        e.preventDefault();
                        add(adding, text);
                        setText('');
                        setAdding(null);
                      } else if (e.key === 'Escape') setAdding(null);
                    }}
                  />
                </Field>
              </div>
              <Button
                variant="primary"
                disabled={!text.trim()}
                onClick={() => {
                  add(adding, text);
                  setText('');
                  setAdding(null);
                }}
              >
                Add {adding === 'include' ? 'inclusion' : 'exclusion'} pattern
              </Button>
              <Button onClick={() => setAdding(null)}>Cancel</Button>
            </div>
          </div>
        )}
        {rows.length === 0 ? (
          <div className={styles.boxEmpty}>
            This ruleset does not target any {noun}. Add a target so its rules apply.
          </div>
        ) : (
          rows.map(([kind, p]) => (
            <div key={`${kind}:${p}`} className={styles.boxRow}>
              <span className={styles.small}>{kind === 'include' ? 'Include' : 'Exclude'}</span>
              <span className={p.startsWith('~') ? undefined : styles.mono}>{patternLabel(p, target)}</span>
              <span className={styles.spacer} />
              <IconButton icon={XIcon} size="sm" label={`Remove ${kind === 'include' ? 'inclusion' : 'exclusion'} ${patternLabel(p, target)}`} onClick={() => remove(kind, p)} />
            </div>
          ))
        )}
      </div>
      {error && <p className={styles.small} style={{ color: 'var(--danger)' }}>{error}</p>}
      {repo && rows.length > 0 && (
        <div className={styles.box} aria-label="Matching refs preview">
          <div className={styles.boxHead}>
            Applies to {matching.length} of {names.length} {noun}
          </div>
          <div className={styles.preview}>
            {refs.loading ? (
              <div className={styles.boxEmpty}>Loading {noun}…</div>
            ) : matching.length === 0 ? (
              <div className={styles.boxEmpty}>No existing {noun} match. The rules also apply to {noun} created later that match.</div>
            ) : (
              <>
                {matching.slice(0, PREVIEW_MAX).map((n) => (
                  <div key={n} className={styles.boxRow}>
                    <span className={styles.mono}>{n}</span>
                    {n === def && target === 'branch' && <span className={styles.muted}>default</span>}
                  </div>
                ))}
                {matching.length > PREVIEW_MAX && <div className={styles.boxEmpty}>and {matching.length - PREVIEW_MAX} more</div>}
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/** Organization rulesets: which repositories the ruleset applies to, with a live preview. */
export function RepoTargets({ form, set, org, error }: { form: RulesetForm; set: Set; org: string; error?: string }) {
  const repos = useOrgRepos(org);
  const selectId = useId();
  const all = repos.data ?? [];
  const cond = toInput(form, true).conditions;
  const matching = form.repoMode === 'property' ? [] : all.filter((r) => selectsRepo(cond, r));
  const [pick, setPick] = useState('');
  return (
    <div className={styles.rules}>
      <Field label="Target repositories" htmlFor={selectId}>
        <Select id={selectId} value={form.repoMode} onChange={(e) => set({ repoMode: e.target.value as RepoTargeting })}>
          <option value="all">All repositories</option>
          <option value="name">Dynamic list by name</option>
          <option value="id">Select repositories</option>
          {form.repoMode === 'property' && <option value="property">By custom property (not editable)</option>}
        </Select>
      </Field>
      {form.repoMode === 'name' && (
        <>
          <ChipInput
            label="Include repositories matching"
            values={form.repoInclude}
            onChange={(v) => set({ repoInclude: v })}
            placeholder="e.g. api-* — press Enter to add"
            hint="fnmatch patterns on the repository name (case-insensitive); ~ALL matches every repository."
          />
          <ChipInput label="Exclude repositories matching" values={form.repoExclude} onChange={(v) => set({ repoExclude: v })} placeholder="e.g. *-archive" />
          <Checkbox
            label="Prevent renaming of target repositories"
            description="Repositories selected by name can't be renamed out of this ruleset's targets."
            checked={form.repoProtected}
            onChange={(v) => set({ repoProtected: v })}
          />
        </>
      )}
      {form.repoMode === 'all' && (
        <Checkbox
          label="Prevent renaming of target repositories"
          description="Repositories selected by name can't be renamed out of this ruleset's targets."
          checked={form.repoProtected}
          onChange={(v) => set({ repoProtected: v })}
        />
      )}
      {form.repoMode === 'id' && (
        <div className={styles.box}>
          <div className={styles.boxHead}>
            <span>Selected repositories</span>
            <span className={styles.spacer} />
            <Select
              aria-label="Add a repository"
              value={pick}
              onChange={(e) => {
                const id = Number(e.target.value);
                if (id && !form.repoIds.includes(id)) set({ repoIds: [...form.repoIds, id] });
                setPick('');
              }}
            >
              <option value="">Add repository…</option>
              {all
                .filter((r) => !form.repoIds.includes(r.id))
                .map((r) => (
                  <option key={r.id} value={r.id}>
                    {r.name}
                  </option>
                ))}
            </Select>
          </div>
          {form.repoIds.length === 0 ? (
            <div className={styles.boxEmpty}>No repositories selected.</div>
          ) : (
            form.repoIds.map((id) => (
              <div key={id} className={styles.boxRow}>
                <RepoIcon size={16} />
                <span>{all.find((r) => r.id === id)?.name ?? `Repository #${id}`}</span>
                <span className={styles.spacer} />
                <IconButton icon={XIcon} size="sm" label="Remove repository" onClick={() => set({ repoIds: form.repoIds.filter((x) => x !== id) })} />
              </div>
            ))
          )}
        </div>
      )}
      {error && <p className={styles.small} style={{ color: 'var(--danger)' }}>{error}</p>}
      {form.repoMode !== 'id' && form.repoMode !== 'property' && (
        <div className={styles.box} aria-label="Matching repositories preview">
          <div className={styles.boxHead}>
            Applies to {matching.length} of {all.length} repositories
          </div>
          <div className={styles.preview}>
            {repos.loading ? (
              <div className={styles.boxEmpty}>Loading repositories…</div>
            ) : matching.length === 0 ? (
              <div className={styles.boxEmpty}>No repository matches.</div>
            ) : (
              matching.slice(0, PREVIEW_MAX).map((r) => (
                <div key={r.id} className={styles.boxRow}>
                  <RepoIcon size={16} />
                  <span>{r.name}</span>
                </div>
              ))
            )}
          </div>
        </div>
      )}
    </div>
  );
}

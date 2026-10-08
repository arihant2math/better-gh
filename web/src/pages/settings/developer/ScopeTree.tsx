import { useEffect, useRef } from 'react';
import { childScopes, topScopes, type ScopeInfo } from '@/api/scopes';
import { cx } from '@/ui/Button';
import styles from './developer.module.css';
import { scopeState, toggleScope, type CheckState } from './logic';

/** GitHub-style "Select scopes" tree: parent checkboxes check their children; partial selection shows indeterminate. */
export function ScopeTree({
  value,
  onChange,
  siteAdmin = false,
  disabled,
}: {
  value: ReadonlySet<string>;
  onChange: (next: Set<string>) => void;
  siteAdmin?: boolean;
  disabled?: boolean;
}) {
  return (
    <div className={styles.scopes} role="group" aria-label="Scopes">
      {topScopes()
        .filter((s) => siteAdmin || !s.siteAdminOnly)
        .map((s) => (
          <div key={s.id} className={styles.scopeGroup}>
            <ScopeRow scope={s} state={scopeState(value, s.id)} disabled={disabled} onToggle={(on) => onChange(toggleScope(value, s.id, on))} />
            {childScopes(s.id).map((c) => (
              <ScopeRow
                key={c.id}
                scope={c}
                child
                state={scopeState(value, c.id)}
                disabled={disabled}
                onToggle={(on) => onChange(toggleScope(value, c.id, on))}
              />
            ))}
          </div>
        ))}
    </div>
  );
}

function ScopeRow({
  scope,
  state,
  child,
  disabled,
  onToggle,
}: {
  scope: ScopeInfo;
  state: CheckState;
  child?: boolean;
  disabled?: boolean;
  onToggle: (on: boolean) => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = state === 'mixed';
  }, [state]);
  return (
    <div className={cx(styles.scopeRow, child && styles.scopeChild)}>
      <label className={cx(styles.scopeLabel, !child && styles.scopeParent)}>
        <input
          ref={ref}
          type="checkbox"
          name="scopes"
          value={scope.id}
          checked={state === 'on'}
          aria-checked={state === 'mixed' ? 'mixed' : state === 'on'}
          disabled={disabled}
          onChange={() => onToggle(state !== 'on')}
        />
        {scope.id}
      </label>
      <span className={styles.scopeDesc}>{scope.description}</span>
    </div>
  );
}

import { passwordStrength } from '../../api/auth';
import { authStyles as styles } from './AuthPage';

/** Four-segment strength meter + hint under a new-password input. */
export function PasswordStrength({ id, password, context }: { id: string; password: string; context?: string[] }) {
  const s = passwordStrength(password, context);
  return (
    <div id={id} className={styles.fieldGroup} aria-live="polite">
      {password && (
        <div className={styles.strength}>
          <div className={styles.meter} data-score={s.score} aria-hidden>
            <span />
            <span />
            <span />
            <span />
          </div>
          <span className={styles.strengthLabel}>{s.label}</span>
        </div>
      )}
      <div className={styles.fieldHint}>{password ? s.hint : 'Use at least 8 characters. A passphrase of 15 or more characters is best.'}</div>
    </div>
  );
}

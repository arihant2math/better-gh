import { observer } from 'mobx-react-lite';
import { theme, type Density, type ThemePref } from '../../../app/theme';
import { PageHeader, Section } from '../../../components/settings/kit';
import { cx } from '../../../ui/Button';
import { CheckIcon } from '../../../ui/icons';
import styles from './userSettings.module.css';

const THEMES: { value: ThemePref; label: string; description: string }[] = [
  { value: 'system', label: 'Sync with system', description: 'Follows your operating system setting.' },
  { value: 'light', label: 'Light', description: 'Bright surfaces, dark text.' },
  { value: 'dark', label: 'Dark', description: 'Dim surfaces, easy on the eyes at night.' },
];

const DENSITIES: { value: Density; label: string; description: string }[] = [
  { value: 'comfortable', label: 'Comfortable', description: 'Roomier rows and controls.' },
  { value: 'compact', label: 'Compact', description: 'Fits more on screen.' },
];

/** Miniature app preview drawn with the palette of `mode` (scoped via data-theme-scope). */
function Preview({ mode, density }: { mode: 'light' | 'dark' | 'split'; density?: Density }) {
  const pane = (m: 'light' | 'dark') => (
    <div className={styles.previewPane} data-theme-scope={m} data-density={density}>
      <div className={styles.previewSide}>
        <span />
        <span />
        <span />
      </div>
      <div className={styles.previewMain}>
        <span className={styles.previewAccent} />
        <span />
        <span />
        <span />
      </div>
    </div>
  );
  return <div className={styles.preview}>{mode === 'split' ? [pane('light'), pane('dark')].map((p, i) => <div key={i}>{p}</div>) : pane(mode)}</div>;
}

/** Radio cards with a visual preview; arrow keys move the selection. */
function CardGroup<T extends string>({
  value,
  options,
  onChange,
  label,
  render,
}: {
  value: T;
  options: { value: T; label: string; description: string }[];
  onChange: (v: T) => void;
  label: string;
  render: (v: T) => React.ReactNode;
}) {
  const move = (i: number, d: number, el: HTMLElement) => {
    const n = (i + d + options.length) % options.length;
    onChange(options[n]!.value);
    (el.parentElement?.children[n] as HTMLElement | undefined)?.focus();
  };
  return (
    <div role="radiogroup" aria-label={label} className={styles.themeGrid}>
      {options.map((o, i) => {
        const on = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={on}
            tabIndex={on ? 0 : -1}
            className={cx(styles.themeCard, on && styles.themeCardOn)}
            onClick={() => onChange(o.value)}
            onKeyDown={(e) => {
              if (e.key === 'ArrowRight' || e.key === 'ArrowDown') {
                e.preventDefault();
                move(i, 1, e.currentTarget);
              } else if (e.key === 'ArrowLeft' || e.key === 'ArrowUp') {
                e.preventDefault();
                move(i, -1, e.currentTarget);
              }
            }}
          >
            {render(o.value)}
            <span className={styles.themeText}>
              <span className={styles.themeLabel}>
                {o.label}
                {on && <CheckIcon size={14} />}
              </span>
              <span className={styles.muted}>{o.description}</span>
            </span>
          </button>
        );
      })}
    </div>
  );
}

export default observer(function AppearanceSettings() {
  return (
    <>
      <PageHeader title="Appearance" description="Choose how Better GitHub looks to you. Saved on this device." />
      <Section title="Theme mode" description="Pick a single theme, or follow the system light and dark setting.">
        <CardGroup
          label="Theme"
          value={theme.pref}
          options={THEMES}
          onChange={(v) => theme.set(v)}
          render={(v) => <Preview mode={v === 'system' ? 'split' : v} />}
        />
      </Section>
      <Section title="Density" description="Adjust row heights, control sizes and spacing across the app.">
        <CardGroup
          label="Density"
          value={theme.density}
          options={DENSITIES}
          onChange={(v) => theme.setDensity(v)}
          render={(v) => <Preview mode={theme.resolved} density={v} />}
        />
      </Section>
    </>
  );
});

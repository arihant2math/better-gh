import { useRef, useState } from 'react';
import { IconButton, cx } from '../../ui/Button';
import { SyncIcon } from '../../ui/icons';
import { Input } from '../../ui/Input';
import { Popover } from '../../ui/Popover';
import { LABEL_PRESETS, normalizeColor, randomLabelColor } from './colors';
import styles from './ColorPicker.module.css';

/**
 * Label color input: hex field (with or without `#`), a random-color button
 * and a swatch popover with GitHub's presets. `value` is 6 hex digits.
 */
export function ColorPicker({ value, onChange, id }: { value: string; onChange: (hex: string) => void; id?: string }) {
  const [text, setText] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const swatchRef = useRef<HTMLButtonElement>(null);
  const shown = text ?? `#${value}`;
  const invalid = text !== null && normalizeColor(text) === null;
  return (
    <div className={styles.picker}>
      <IconButton
        icon={SyncIcon}
        label="Random color"
        variant="secondary"
        className={styles.random}
        style={{ background: `#${value}` }}
        onClick={() => {
          setText(null);
          onChange(randomLabelColor());
        }}
      />
      <button ref={swatchRef} type="button" className={styles.swatchButton} aria-label="Choose from presets" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
        <span className={styles.swatch} style={{ background: `#${value}` }} />
      </button>
      <Input
        id={id}
        className={styles.hex}
        value={shown}
        invalid={invalid}
        maxLength={7}
        spellCheck={false}
        aria-label="Color (hex)"
        onChange={(e) => {
          setText(e.target.value);
          const c = normalizeColor(e.target.value);
          if (c) onChange(c);
        }}
        onBlur={() => setText(null)}
      />
      <Popover open={open} onClose={() => setOpen(false)} anchor={swatchRef} placement="bottom-start" className={styles.presets} aria-label="Color presets">
        <div className={styles.grid} role="listbox" aria-label="Preset colors">
          {LABEL_PRESETS.map((c) => (
            <button
              key={c}
              type="button"
              role="option"
              aria-selected={c === value}
              aria-label={`#${c}`}
              title={`#${c}`}
              className={cx(styles.preset, c === value && styles.presetOn)}
              style={{ background: `#${c}` }}
              onClick={() => {
                setText(null);
                onChange(c);
                setOpen(false);
              }}
            />
          ))}
        </div>
      </Popover>
    </div>
  );
}

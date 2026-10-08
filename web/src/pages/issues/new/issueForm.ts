/** Issue forms (GitHub's `.github/ISSUE_TEMPLATE/*.yml`): state, validation, markdown output. */
import type { IssueFormElement } from '@/api/endpoints';

export type FormValue = string | boolean[];
export type FormValues = Record<string, FormValue>;

export function fieldKey(el: IssueFormElement, index: number): string {
  return el.id ?? `field-${index}`;
}

export function dropdownOptions(el: IssueFormElement): string[] {
  return (el.attributes?.options ?? []).map((o) => (typeof o === 'string' ? o : o.label));
}

export function initialValues(form: IssueFormElement[]): FormValues {
  const v: FormValues = {};
  form.forEach((el, i) => {
    const k = fieldKey(el, i);
    if (el.type === 'input' || el.type === 'textarea') v[k] = el.attributes?.value ?? '';
    else if (el.type === 'dropdown') {
      const d = el.attributes?.default;
      v[k] = d != null ? (dropdownOptions(el)[d] ?? '') : '';
    } else if (el.type === 'checkboxes') v[k] = (el.attributes?.options ?? []).map(() => false);
  });
  return v;
}

/** Field keys that are required but empty (checkbox groups: each required option). */
export function missingRequired(form: IssueFormElement[], values: FormValues): string[] {
  const out: string[] = [];
  form.forEach((el, i) => {
    const k = fieldKey(el, i);
    const v = values[k];
    if (el.type === 'checkboxes') {
      const opts = el.attributes?.options ?? [];
      const checked = Array.isArray(v) ? v : [];
      if (opts.some((o, j) => typeof o !== 'string' && o.required && !checked[j])) out.push(k);
    } else if (el.type !== 'markdown' && el.validations?.required) {
      if (typeof v !== 'string' || !v.trim()) out.push(k);
    }
  });
  return out;
}

/** Render the submitted form as the issue body, exactly like GitHub (### Label / value). */
export function formToMarkdown(form: IssueFormElement[], values: FormValues): string {
  const parts: string[] = [];
  form.forEach((el, i) => {
    if (el.type === 'markdown') return;
    const k = fieldKey(el, i);
    const label = el.attributes?.label ?? k;
    const v = values[k];
    let text: string;
    if (el.type === 'checkboxes') {
      const opts = el.attributes?.options ?? [];
      const checked = Array.isArray(v) ? v : [];
      text = opts.map((o, j) => `- [${checked[j] ? 'X' : ' '}] ${typeof o === 'string' ? o : o.label}`).join('\n');
    } else {
      const s = typeof v === 'string' ? v.trim() : '';
      if (!s) text = '_No response_';
      else if (el.type === 'textarea' && el.attributes?.render) text = `\`\`\`${el.attributes.render}\n${s}\n\`\`\``;
      else text = s;
    }
    parts.push(`### ${label}\n\n${text}`);
  });
  return parts.join('\n\n');
}

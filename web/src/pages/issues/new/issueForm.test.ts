import { describe, expect, it } from 'vitest';
import type { IssueFormElement } from '../../../api/endpoints';
import { formToMarkdown, initialValues, missingRequired } from './issueForm';

const form: IssueFormElement[] = [
  { type: 'markdown', attributes: { value: 'Thanks!' } },
  { type: 'input', id: 'version', attributes: { label: 'Version' }, validations: { required: true } },
  { type: 'dropdown', id: 'area', attributes: { label: 'Area', options: ['API', 'CLI'], default: 1 } },
  { type: 'textarea', id: 'logs', attributes: { label: 'Logs', render: 'shell' } },
  { type: 'textarea', id: 'notes', attributes: { label: 'Notes' } },
  { type: 'checkboxes', id: 'terms', attributes: { label: 'Terms', options: [{ label: 'I agree', required: true }, 'Optional'] } },
];

describe('issue forms', () => {
  it('initializes defaults', () => {
    expect(initialValues(form)).toEqual({ version: '', area: 'CLI', logs: '', notes: '', terms: [false, false] });
  });

  it('validates required inputs and checkboxes', () => {
    const v = initialValues(form);
    expect(missingRequired(form, v)).toEqual(['version', 'terms']);
    expect(missingRequired(form, { ...v, version: '1.0', terms: [true, false] })).toEqual([]);
  });

  it('renders markdown like GitHub', () => {
    const md = formToMarkdown(form, { version: 'v2', area: 'API', logs: 'boom', notes: '', terms: [true, false] });
    expect(md).toBe(
      '### Version\n\nv2\n\n### Area\n\nAPI\n\n### Logs\n\n```shell\nboom\n```\n\n### Notes\n\n_No response_\n\n### Terms\n\n- [X] I agree\n- [ ] Optional',
    );
  });
});

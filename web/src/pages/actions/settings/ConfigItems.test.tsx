// @vitest-environment jsdom
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '@/api/client';

const api = vi.hoisted(() => ({
  listSecrets: vi.fn(),
  getSecretsPublicKey: vi.fn(),
  putSecret: vi.fn(),
  createVariable: vi.fn(),
  updateVariable: vi.fn(),
  deleteSecret: vi.fn(),
  deleteVariable: vi.fn(),
  listVariables: vi.fn(),
  listOrgSecretsForRepo: vi.fn(),
  listOrgVariablesForRepo: vi.fn(),
}));
vi.mock('@/api/actions', () => api);
vi.mock('./sealedBox', () => ({ sealSecret: () => 'sealed' }));

const { ConfigList } = await import('./ConfigItems');

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// jsdom has no modal dialogs.
HTMLDialogElement.prototype.showModal ??= function (this: HTMLDialogElement) {
  this.open = true;
};
HTMLDialogElement.prototype.close ??= function (this: HTMLDialogElement) {
  this.open = false;
};

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  host = document.createElement('div');
  document.body.append(host);
  root = createRoot(host);
  api.listSecrets.mockResolvedValue({ total_count: 0, secrets: [] });
  api.getSecretsPublicKey.mockResolvedValue({ key_id: 'k1', key: 'pub' });
});

afterEach(() => {
  act(() => root.unmount());
  document.body.innerHTML = '';
  vi.clearAllMocks();
});

const flush = () => act(() => new Promise((r) => setTimeout(r, 0)));

function type(el: HTMLInputElement | HTMLTextAreaElement, text: string) {
  const proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, 'value')!.set!.call(el, text);
  el.dispatchEvent(new Event('input', { bubbles: true }));
}

describe('secret form', () => {
  it('shows a 422 reason under the field it names', async () => {
    api.putSecret.mockRejectedValue(
      new ApiError('Validation Failed', 422, {
        message: 'Validation Failed',
        errors: [{ resource: 'Secret', field: 'name', code: 'custom', message: 'name is reserved for the runner' }],
      }),
    );
    await act(async () => root.render(<ConfigList scope={{ kind: 'repo', owner: 'octo', repo: 'demo' }} kind="secrets" title="Secrets" />));
    await flush();

    const add = [...document.querySelectorAll('button')].find((b) => /New repository secret/.test(b.textContent ?? ''))!;
    expect(add).toBeTruthy();
    await act(async () => add.click());

    const form = document.querySelector('form')!;
    await act(async () => {
      type(form.querySelector('input')!, 'RUNNER_TOKEN');
      type(form.querySelector('textarea')!, 'hunter2');
    });
    await act(async () => form.requestSubmit());
    await flush();

    expect(api.putSecret).toHaveBeenCalledOnce();
    expect(form.textContent).toContain('name is reserved for the runner');
    expect(form.querySelector('input')!.getAttribute('aria-invalid')).toBe('true');
  });
});

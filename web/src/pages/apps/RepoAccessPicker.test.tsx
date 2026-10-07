// @vitest-environment jsdom
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { RepoAccessPicker, type RepoSelection } from './RepoAccessPicker';
import { simpleUser } from '../../test/fixtures';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const REPOS = [
  { id: 1, full_name: 'octo/alpha', private: false },
  { id: 2, full_name: 'octo/beta', private: true },
];
vi.mock('../../api/cache', () => ({ useResource: () => ({ data: REPOS, loading: false }) }));

const account = simpleUser('octo', 1);
let root: Root | null = null;
afterEach(() => {
  act(() => root?.unmount());
  root = null;
  document.body.innerHTML = '';
});

function type(input: HTMLInputElement, text: string) {
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, text);
  input.dispatchEvent(new Event('input', { bubbles: true }));
}

const options = () => [...document.querySelectorAll('[role=option]')].map((o) => o.textContent?.trim());

describe('RepoAccessPicker', () => {
  it('drops a repository from the matches once value.repos picks it', () => {
    const host = document.body.appendChild(document.createElement('div'));
    root = createRoot(host);
    const render = (value: RepoSelection) => act(() => root!.render(<RepoAccessPicker account={account} value={value} onChange={() => {}} />));
    render({ mode: 'selected', repos: [] });
    act(() => type(document.querySelector<HTMLInputElement>('input[aria-label="Search repositories"]')!, 'octo'));
    expect(options()).toEqual(['octo/alpha', 'octo/beta']);

    render({ mode: 'selected', repos: [REPOS[0]!] });
    expect(options()).toEqual(['octo/beta']);

    render({ mode: 'selected', repos: [] });
    expect(options()).toEqual(['octo/alpha', 'octo/beta']);
  });
});

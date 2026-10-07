import { describe, expect, it } from 'vitest';
import type { RestEvent } from '../../api/profile';
import { summarizeEvent } from './activity';

const ev = (type: string, payload: Record<string, unknown> = {}): RestEvent => ({ id: '1', type, actor: { login: 'ada' }, repo: { name: 'acme/api' }, payload, created_at: '2024-01-01T00:00:00Z' });

describe('summarizeEvent', () => {
  it('summarizes common events', () => {
    expect(summarizeEvent(ev('PushEvent', { size: 3, ref: 'refs/heads/main' }))).toMatchObject({ kind: 'push', text: 'Pushed 3 commits to', repo: 'acme/api', detail: 'main' });
    expect(summarizeEvent(ev('PushEvent', { size: 1 })).text).toBe('Pushed 1 commit to');
    expect(summarizeEvent(ev('CreateEvent', { ref_type: 'repository' })).text).toBe('Created repository');
    expect(summarizeEvent(ev('WatchEvent')).kind).toBe('star');
    expect(summarizeEvent(ev('IssuesEvent', { action: 'closed', issue: { number: 4, title: 'Bug' } }))).toMatchObject({ text: 'Closed issue #4 in', path: '/issues/4', detail: 'Bug' });
    expect(summarizeEvent(ev('PullRequestEvent', { action: 'closed', pull_request: { number: 7, merged: true } })).text).toBe('Merged pull request #7 in');
    expect(summarizeEvent(ev('GollumEvent')).text).toBe('Gollum in');
  });
});

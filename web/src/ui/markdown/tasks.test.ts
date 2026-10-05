import { describe, expect, it } from 'vitest';
import { countTasks, setTask } from './tasks';

describe('task list editing', () => {
  const src = ['Checklist:', '', '- [ ] one', '- [x] two', '  * [ ] nested', '1. [ ] numbered', '> - [ ] quoted', '', '```', '- [ ] in code', '```', '- [] not a task', '- [ ]no space'].join('\n');

  it('counts task items outside code fences', () => {
    expect(countTasks(src)).toBe(5);
  });

  it('ticks and unticks the nth item only', () => {
    expect(setTask(src, 0, true)).toContain('- [x] one\n- [x] two');
    expect(setTask(src, 1, false)).toContain('- [ ] one\n- [ ] two');
    expect(setTask(src, 2, true)).toContain('  * [x] nested');
    expect(setTask(src, 3, true)).toContain('1. [x] numbered');
    expect(setTask(src, 4, true)).toContain('> - [x] quoted');
    expect(setTask(src, 4, true)).toContain('```\n- [ ] in code\n```');
  });

  it('returns null for a missing item', () => {
    expect(setTask(src, 5, true)).toBeNull();
    expect(setTask('no tasks', 0, true)).toBeNull();
  });
});

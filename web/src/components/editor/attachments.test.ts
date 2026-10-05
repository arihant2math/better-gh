import { describe, expect, it } from 'vitest';
import type { Attachment } from '../../api/uploads';
import { attachFiles, insertBlock, placeholderFor, removePlaceholder, replacePlaceholder } from './attachments';
import type { Edit } from './format';

const attachment = (markdown: string): Attachment => ({
  id: 1,
  uuid: 'u',
  name: 'x',
  content_type: 'image/png',
  size: 1,
  href: 'h',
  markdown,
  repository_id: null,
  created_at: '',
});

function editor(initial: Edit) {
  let state = initial;
  const history: string[] = [];
  return {
    read: () => state,
    write: (e: Edit) => {
      state = e;
      history.push(e.value);
    },
    get state() {
      return state;
    },
    history,
  };
}

const file = (name: string) => new File(['data'], name, { type: 'image/png' });

describe('attachment placeholders', () => {
  it('inserts the placeholder on its own line at the caret', () => {
    const e = insertBlock({ value: 'see:after', selStart: 4, selEnd: 4 }, placeholderFor('a.png'));
    expect(e.value).toBe('see:\n![Uploading a.png…]()\nafter');
    expect(e.selStart).toBe(e.value.indexOf('\nafter'));
  });

  it('replaces the placeholder with the uploaded markdown', async () => {
    const ed = editor({ value: 'Bug:', selStart: 4, selEnd: 4 });
    let finish!: (a: Attachment) => void;
    const done = attachFiles([file('shot.png')], {
      ...ed,
      upload: () => new Promise((r) => (finish = r)),
    });
    expect(ed.state.value).toBe('Bug:\n![Uploading shot.png…]()');
    // The user keeps typing meanwhile.
    ed.write({ value: `${ed.state.value}\nmore text`, selStart: 999, selEnd: 999 });
    finish(attachment('![shot.png](https://bgh.test/user-attachments/assets/u)'));
    expect(await done).toBe(1);
    expect(ed.state.value).toBe('Bug:\n![shot.png](https://bgh.test/user-attachments/assets/u)\nmore text');
  });

  it('removes the placeholder and reports when the upload fails', async () => {
    const ed = editor({ value: 'start\nend', selStart: 5, selEnd: 5 });
    const errors: string[] = [];
    const ok = await attachFiles([file('a.png'), file('b.exe')], {
      ...ed,
      upload: async (f) => {
        if (f.name.endsWith('.exe')) throw new Error("We don't support that file type");
        return attachment('![a.png](/a)');
      },
      onError: (f, err) => errors.push(`${f.name}: ${err.message}`),
    });
    expect(ok).toBe(1);
    expect(ed.state.value).toBe('start\n![a.png](/a)\nend');
    expect(ed.state.value).not.toContain('Uploading');
    expect(errors).toEqual(["b.exe: We don't support that file type"]);
  });

  it('keeps the selection stable around replacements', () => {
    const ph = placeholderFor('x.png');
    const value = `${ph}\nhello`;
    const caret = value.length;
    const r = replacePlaceholder({ value, selStart: caret, selEnd: caret }, ph, '![x](/u)');
    expect(r.value).toBe('![x](/u)\nhello');
    expect(r.selStart).toBe(r.value.length);
    const before = removePlaceholder({ value: `hi\n${ph}`, selStart: 1, selEnd: 1 }, ph);
    expect(before).toEqual({ value: 'hi', selStart: 1, selEnd: 1 });
    // Already gone (user deleted it): untouched.
    const same = { value: 'nothing', selStart: 0, selEnd: 0 };
    expect(replacePlaceholder(same, ph, 'x')).toBe(same);
  });
});

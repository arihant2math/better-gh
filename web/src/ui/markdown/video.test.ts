import { describe, expect, it } from 'vitest';
import { attachmentVideo } from './video';

describe('attachment videos', () => {
  const url = 'https://bgh.test/user-attachments/assets/0f8fad5b-d9cb-469f-a165-70867728950e';

  it('embeds a bare attachment URL as a video player', () => {
    expect(attachmentVideo(url)).toContain(`<video src="${url}" controls`);
    expect(attachmentVideo(' /user-attachments/assets/0f8fad5b-d9cb-469f-a165-70867728950e ')).toContain('<video');
  });

  it('leaves other paragraphs alone', () => {
    expect(attachmentVideo(`see ${url}`)).toBeNull();
    expect(attachmentVideo('https://example.com/video.mp4')).toBeNull();
    expect(attachmentVideo('https://bgh.test/user-attachments/files/1/log.txt')).toBeNull();
  });
});

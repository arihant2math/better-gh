/** A bare attachment URL on its own line is a video (images are inserted as `![]()`). */
const VIDEO_URL = /^\s*((?:https?:\/\/[^\s/]+)?\/user-attachments\/assets\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})\s*$/i;

/** `<video>` for a paragraph that is only an attachment URL, else null. */
export function attachmentVideo(text: string): string | null {
  const m = VIDEO_URL.exec(text);
  return m ? `<video src="${m[1]!}" controls preload="metadata" class="attachment-video"></video>\n` : null;
}

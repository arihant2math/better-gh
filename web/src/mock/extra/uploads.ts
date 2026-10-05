/**
 * Mock of `POST /_bgh/uploads` and `GET /user-attachments/...`
 * (crates/bgh-uploads, package P6). Images get a `data:` URL so they render
 * in mock mode (the browser fetches `<img>` sources from the network, not
 * through the mock transport); other files are served from memory.
 */
import type { MockServer } from '../server';
import { invalid, notFound, ok, param, state, type Ctx } from './util';

const MB = 1024 * 1024;
const KINDS: [RegExp, 'image' | 'video' | 'file', number][] = [
  [/\.(png|gif|jpe?g|svg)$/i, 'image', 10 * MB],
  [/\.(mp4|mov|webm)$/i, 'video', 100 * MB],
  [/\.(log|txt|md|patch|diff|pdf|zip|gz|tgz|docx|pptx|xlsx|json|jsonc|csv|tsv|html?|c|cs|cpp|css|h|java|jsx?|tsx?|py|rb|sh|sql|xml|ya?ml|go|rs|toml)$/i, 'file', 25 * MB],
];

interface Stored {
  id: number;
  uuid: string;
  name: string;
  type: string;
  data: string;
}

const origin = () => (typeof location !== 'undefined' ? location.origin : 'http://localhost');

function uuid(): string {
  const hex = Array.from({ length: 32 }, () => Math.floor(Math.random() * 16).toString(16)).join('');
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-4${hex.slice(13, 16)}-a${hex.slice(17, 20)}-${hex.slice(20)}`;
}

async function readRaw(raw: Ctx['raw']): Promise<{ bytes: Uint8Array; type: string }> {
  if (raw instanceof Blob) return { bytes: new Uint8Array(await raw.arrayBuffer()), type: raw.type };
  if (typeof raw === 'string') return { bytes: new TextEncoder().encode(raw), type: '' };
  if (raw instanceof ArrayBuffer) return { bytes: new Uint8Array(raw), type: '' };
  if (ArrayBuffer.isView(raw)) return { bytes: new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength), type: '' };
  return { bytes: new Uint8Array(), type: '' };
}

export function installUploadMocks(server: MockServer): void {
  const files = () => state(server, 'uploads', () => new Map<number, Stored>());
  let seq = 1000;

  server.route('POST', '/_bgh/uploads', async (ctx) => {
    const name = (ctx.url.searchParams.get('name') ?? '').split(/[/\\]/).pop()!.trim();
    if (!name) return invalid('Validation Failed', 'name', 'missing_field', 'Attachment');
    const kind = KINDS.find(([re]) => re.test(name));
    if (!kind) return invalid('Validation Failed', 'file', 'custom', 'Attachment');
    const { bytes, type } = await readRaw(ctx.raw);
    if (bytes.length > kind[2]) return invalid('Validation Failed', 'file', 'custom', 'Attachment');
    if (!bytes.length) return invalid('Validation Failed', 'file', 'custom', 'Attachment');
    const id = ++seq;
    const u = uuid();
    const contentType = type || 'application/octet-stream';
    let href: string;
    if (kind[1] === 'file') {
      href = `${origin()}/user-attachments/files/${id}/${encodeURIComponent(name)}`;
    } else if (kind[1] === 'image') {
      let bin = '';
      for (const b of bytes) bin += String.fromCharCode(b);
      href = `data:${contentType};base64,${btoa(bin)}`;
    } else {
      href = `${origin()}/user-attachments/assets/${u}`;
    }
    files().set(id, { id, uuid: u, name, type: contentType, data: new TextDecoder().decode(bytes) });
    const label = name.replace(/[[\]\\]/g, '\\$&');
    const markdown = kind[1] === 'image' ? `![${label}](${href})` : kind[1] === 'video' ? href : `[${label}](${href})`;
    const repoId = Number(ctx.url.searchParams.get('repository_id')) || null;
    return ok({ id, uuid: u, name, content_type: contentType, size: bytes.length, href, markdown, repository_id: repoId, created_at: server.now() }, 201);
  });

  server.route('GET', '/user-attachments/files/:id/:name', (ctx) => {
    const f = files().get(Number(param(ctx, 1)));
    if (!f || f.name !== param(ctx, 2)) return notFound();
    return { status: 200, text: f.data, headers: { 'content-type': f.type, 'content-disposition': `attachment; filename="${f.name}"`, 'x-content-type-options': 'nosniff' } };
  });
}

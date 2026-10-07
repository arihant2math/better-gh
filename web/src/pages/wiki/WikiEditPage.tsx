import { useRef, useState } from 'react';
import { ApiError } from '../../api/client';
import { UploadStatus, useAttachments } from '../../components/editor/useAttachments';
import { createWikiPage, deleteWikiPage, updateWikiPage, wikiSlug, type WikiPage } from '../../api/wiki';
import { navigate, useParams, useQuery } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { Button } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { BookIcon, TrashIcon } from '../../ui/icons';
import { Field, Input, Textarea } from '../../ui/Input';
import { Markdown } from '../../ui/Markdown';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { invalidateWiki, isNotFound, previewWikiLinks, useWikiIndex, useWikiPage } from './data';
import styles from './Wiki.module.css';

/** `/:owner/:repo/wiki/new` and `/:owner/:repo/wiki/:slug/edit`. */
export default function WikiEditPage() {
  const { owner, repo, slug } = useParams<{ owner: string; repo: string; slug?: string }>();
  const index = useWikiIndex(owner, repo);
  const page = useWikiPage(owner, repo, slug ?? null);
  if (slug && !page.data) {
    if (page.error) return <EmptyState icon={BookIcon} title={isNotFound(page.error) ? 'This page does not exist' : 'Could not load this page'} />;
    return (
      <div className={styles.editor}>
        <Skeleton width="40%" height={28} />
        <Skeleton width="100%" height={300} />
      </div>
    );
  }
  if (index.data && !index.data.canEdit) return <EmptyState icon={BookIcon} title="You do not have permission to edit this wiki" />;
  return <Editor key={page.data?.commit.sha ?? 'new'} owner={owner} repo={repo} page={page.data} />;
}

function Editor({ owner, repo, page }: { owner: string; repo: string; page?: WikiPage }) {
  const initialTitle = useQuery().get('title')?.replace(/-/g, ' ') ?? '';
  const [title, setTitle] = useState(page?.title ?? initialTitle);
  const [body, setBody] = useState(page?.raw ?? '');
  const [message, setMessage] = useState('');
  const [tab, setTab] = useState<'write' | 'preview'>('write');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const base = `/${owner}/${repo}/wiki`;
  const dirty = !page || title !== page.title || body !== page.raw;
  const textarea = useRef<HTMLTextAreaElement | null>(null);
  const attachments = useAttachments({ textarea, value: body, onChange: setBody, repo: `${owner}/${repo}` });

  const save = async () => {
    if (!title.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      const saved = page
        ? await updateWikiPage(owner, repo, page.slug, {
            title: title.trim() !== page.title ? title.trim() : undefined,
            body,
            message: message.trim() || undefined,
            expectedCommit: page.commit.sha,
          })
        : await createWikiPage(owner, repo, { title: title.trim(), body, message: message.trim() || undefined });
      invalidateWiki(owner, repo);
      toast({ kind: 'success', title: page ? 'Page saved' : 'Page created' });
      navigate(`${base}/${saved?.slug ?? wikiSlug(title)}`);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
      setBusy(false);
    }
  };

  useShortcuts('Wiki editor', {
    'mod+enter': { handler: () => void save(), description: 'Save page', group: 'Wiki', allowInInput: true },
    escape: { handler: () => navigate(page ? `${base}/${page.slug}` : base), description: 'Cancel', group: 'Wiki' },
  });

  return (
    <form
      className={styles.editor}
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <h1 className={styles.h1}>{page ? `Editing ${page.title}` : 'Create new page'}</h1>
      <Input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="Title" aria-label="Title" size="lg" autoFocus={!page} />
      <div className={styles.editorTabs}>
        <Tabs
          size="sm"
          value={tab}
          onChange={(t) => setTab(t as 'write' | 'preview')}
          items={[
            { id: 'write', label: 'Write' },
            { id: 'preview', label: 'Preview' },
          ]}
        />
        <span className={styles.muted}>Markdown · link pages with [[Page Name]] or [[Text|Page Name]] · paste or drop files to attach</span>
        {attachments.button}
      </div>
      <div className={styles.editorPanes} data-tab={tab}>
        <div>
          <Textarea
            ref={textarea}
            className={styles.textarea}
            value={body}
            onChange={(e) => setBody(e.target.value)}
            onPaste={attachments.onPaste}
            onDrop={attachments.onDrop}
            onDragOver={attachments.onDragOver}
            onDragLeave={attachments.onDragLeave}
            aria-label="Page content"
            autoFocus={!!page}
            rows={22}
          />
          <UploadStatus a={attachments} className={styles.muted} />
        </div>
        <div className={styles.preview} aria-label="Preview">
          <Markdown source={previewWikiLinks(body, base)} repo={`${owner}/${repo}`} />
        </div>
      </div>
      <Field label="Edit message" htmlFor="wiki-msg">
        <Input
          id="wiki-msg"
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          placeholder={page ? `Update ${title || page.title}` : `Create ${title || 'page'}`}
        />
      </Field>
      {error && (
        <div className={styles.error} role="alert">
          {error}
        </div>
      )}
      <div className={styles.editorActions}>
        {page && page.slug !== 'Home' && (
          <Button
            variant="danger"
            leadingIcon={TrashIcon}
            onClick={() =>
              void (async () => {
                if (!confirm(`Delete “${page.title}”?`)) return;
                try {
                  await deleteWikiPage(owner, repo, page.slug);
                  invalidateWiki(owner, repo);
                  toast({ kind: 'success', title: 'Page deleted' });
                  navigate(base);
                } catch (e) {
                  setError(e instanceof ApiError ? e.message : String(e));
                }
              })()
            }
          >
            Delete page
          </Button>
        )}
        <span className={styles.grow} />
        <Button onClick={() => navigate(page ? `${base}/${page.slug}` : base)}>Cancel</Button>
        <Button type="submit" variant="primary" loading={busy} disabled={!title.trim() || !dirty} kbd="⌘↵">
          Save page
        </Button>
      </div>
    </form>
  );
}

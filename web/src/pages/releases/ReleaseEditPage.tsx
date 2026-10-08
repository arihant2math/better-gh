import { observer } from 'mobx-react-lite';
import { useId, useRef, useState, type DragEvent, type KeyboardEvent } from 'react';
import { useResource } from '../../api/cache';
import { ApiError } from '../../api/client';
import {
  codeKeys,
  createRelease,
  deleteReleaseAsset,
  findReleaseByTag,
  generateReleaseNotes,
  listReleases,
  updateRelease,
  uploadReleaseAsset,
  type ReleaseInput,
  type RestAsset,
  type RestRelease,
} from '../../api/code';
import { describeCode, errorMessage, validationErrors } from '../../api/errors';
import { RefPicker, useRefs } from '../../components/code/RefPicker';
import { UploadStatus, useAttachments } from '../../components/editor/useAttachments';
import { Link, navigate, useParams } from '../../router';
import { useShortcuts } from '../../shortcuts/useShortcuts';
import { canPush as canPushTo } from '../../sync/selectors';
import { Button, IconButton, cx } from '../../ui/Button';
import { EmptyState, Skeleton } from '../../ui/EmptyState';
import { AlertIcon, CheckIcon, ChevronRightIcon, PackageIcon, PlusIcon, SyncIcon, TagIcon, TrashIcon, UploadIcon, XIcon } from '../../ui/icons';
import { Field, Input, Select, Textarea } from '../../ui/Input';
import { Markdown } from '../../ui/Markdown';
import { Tabs } from '../../ui/Tabs';
import { toast } from '../../ui/Toast';
import { useRouteRepo } from '../repo/useRouteRepo';
import styles from './Releases.module.css';
import { AssetRow, PER_PAGE, formatBytes, invalidateReleases, isNotFound, primeRelease, releaseHref, releasesBase, useLatestRelease } from './shared';

/** `/:owner/:repo/releases/new` and `/:owner/:repo/releases/edit/:tag`. */
export default observer(function ReleaseEditPage() {
  const { owner, repo, tag } = useParams<{ owner: string; repo: string; tag?: string }>();
  const repoRow = useRouteRepo();
  const canPush = canPushTo(repoRow.id);
  const existing = useResource<RestRelease>(tag ? codeKeys.release(owner, repo, tag) : null, () => findReleaseByTag(owner, repo, tag!));
  const latest = useLatestRelease(owner, repo);
  const defaultBranch = repoRow.defaultBranch;

  if (!canPush) {
    return (
      <div className={styles.editor}>
        <EmptyState icon={TagIcon} title="You don’t have permission to edit releases in this repository" />
      </div>
    );
  }
  if (tag && (!existing.data || (latest.data === undefined && !latest.error))) {
    if (existing.error) {
      return (
        <div className={styles.editor}>
          <EmptyState
            icon={isNotFound(existing.error) ? TagIcon : AlertIcon}
            title={isNotFound(existing.error) ? 'Release not found' : 'Could not load this release'}
            action={<Button onClick={() => navigate(releasesBase(owner, repo))}>View all releases</Button>}
          />
        </div>
      );
    }
    return (
      <div className={styles.editor} aria-busy>
        <Skeleton width="30%" height={24} />
        <Skeleton width="60%" height={28} />
        <Skeleton width="100%" height={28} />
        <Skeleton width="100%" height={280} />
      </div>
    );
  }
  const release = existing.data ?? null;
  return (
    <Editor
      key={release?.id ?? 'new'}
      owner={owner}
      repo={repo}
      initial={release}
      wasLatest={!!release && latest.data?.id === release.id}
      defaultBranch={defaultBranch}
    />
  );
});

// ------------------------------------------------------------------ editor

interface Upload {
  key: number;
  file: File;
  progress: number;
  status: 'queued' | 'uploading' | 'error';
  error?: string;
}

type Errors = Partial<Record<'tag' | 'target' | 'name' | 'form', string>>;
type Intent = 'publish' | 'draft' | 'update';

let uploadSeq = 0;

/** Map a GitHub 422 `errors[]` onto the form fields. */
function releaseErrors(e: unknown): Errors | null {
  if (!(e instanceof ApiError) || e.status !== 422) return null;
  const list = validationErrors(e);
  const out: Errors = {};
  for (const err of list) {
    if (err.field === 'tag_name') {
      out.tag =
        err.code === 'already_exists'
          ? 'A release with this tag already exists.'
          : err.code === 'missing_field'
            ? 'Choose an existing tag or create a new one.'
            : (err.message ?? 'This is not a valid tag name.');
    } else if (err.field === 'target_commitish') out.target = err.message ?? 'The target is not a valid branch or commit.';
    else if (err.field === 'name') out.name = err.message ?? 'Invalid title.';
    else out.form = err.message ?? describeCode(err.field ?? 'release', err.code);
  }
  if (!list.length) out.form = e.message;
  return out;
}

const Editor = observer(function Editor({
  owner,
  repo,
  initial,
  wasLatest,
  defaultBranch,
}: {
  owner: string;
  repo: string;
  initial: RestRelease | null;
  wasLatest: boolean;
  defaultBranch: string;
}) {
  const [current, setCurrent] = useState<RestRelease | null>(initial);
  const [tagName, setTagName] = useState(initial?.tag_name ?? '');
  const [target, setTarget] = useState(initial?.target_commitish || defaultBranch);
  const [prevTag, setPrevTag] = useState('');
  const [title, setTitle] = useState(initial?.name ?? '');
  const [body, setBody] = useState(initial?.body ?? '');
  const [prerelease, setPrerelease] = useState(initial?.prerelease ?? false);
  const [makeLatest, setMakeLatest] = useState(initial ? wasLatest : true);
  const [tab, setTab] = useState<'write' | 'preview'>('write');
  const notesRef = useRef<HTMLTextAreaElement | null>(null);
  const notesAttachments = useAttachments({ textarea: notesRef, value: body, onChange: setBody, repo: `${owner}/${repo}` });
  const [assets, setAssets] = useState<RestAsset[]>(initial?.assets ?? []);
  const [uploads, setUploads] = useState<Upload[]>([]);
  const [errors, setErrors] = useState<Errors>({});
  const [busy, setBusy] = useState<Intent | null>(null);
  const [generating, setGenerating] = useState(false);
  const [dragging, setDragging] = useState(false);
  const fileInput = useRef<HTMLInputElement>(null);
  const controllers = useRef(new Map<number, AbortController>());
  const inflight = useRef(new Map<number, Promise<boolean>>());
  const ids = useId();

  const refs = useRefs(owner, repo);
  const firstPage = useResource<RestRelease[]>(codeKeys.releases(owner, repo, 1), () => listReleases(owner, repo, 1, PER_PAGE));
  const tags = (refs.data?.tags ?? []).map((t) => t.name);
  const trimmedTag = tagName.trim();
  const tagExists = tags.includes(trimmedTag);
  const newTag = !!trimmedTag && !!refs.data && !tagExists;
  const conflict = firstPage.data?.find((r) => r.tag_name === trimmedTag && r.id !== current?.id && !r.draft);
  const mode: 'new' | 'draft' | 'published' = !current ? 'new' : current.draft ? 'draft' : 'published';
  const base = releasesBase(owner, repo);

  // ---------------------------------------------------------------- uploads

  const patchUpload = (key: number, patch: Partial<Upload>) => setUploads((us) => us.map((u) => (u.key === key ? { ...u, ...patch } : u)));

  const startUpload = (rel: RestRelease, u: Upload): Promise<boolean> => {
    const ctrl = new AbortController();
    controllers.current.set(u.key, ctrl);
    patchUpload(u.key, { status: 'uploading', progress: 0, error: undefined });
    const p = uploadReleaseAsset(owner, repo, rel.id, u.file, (f) => patchUpload(u.key, { progress: f }), ctrl.signal).then(
      (asset) => {
        if (ctrl.signal.aborted) {
          // Finished despite the cancel: remove it again.
          void deleteReleaseAsset(owner, repo, asset.id).catch(() => undefined);
          return true;
        }
        setUploads((us) => us.filter((x) => x.key !== u.key));
        setAssets((as) => [...as, asset]);
        return true;
      },
      (e: unknown) => {
        if (ctrl.signal.aborted) return true;
        patchUpload(u.key, { status: 'error', error: errorMessage(e) });
        return false;
      },
    );
    const tracked = p.finally(() => {
      inflight.current.delete(u.key);
      controllers.current.delete(u.key);
    });
    inflight.current.set(u.key, tracked);
    return tracked;
  };

  const addFiles = (files: FileList | File[] | null) => {
    if (!files?.length) return;
    const taken = new Set([...assets.map((a) => a.name), ...uploads.map((u) => u.file.name)]);
    const added: Upload[] = [];
    for (const file of Array.from(files)) {
      if (taken.has(file.name)) {
        toast({ kind: 'error', title: `${file.name} is already attached`, description: 'Asset names must be unique within a release.' });
        continue;
      }
      if (!file.size) {
        toast({ kind: 'error', title: `${file.name} is empty`, description: 'Empty files can’t be uploaded.' });
        continue;
      }
      taken.add(file.name);
      added.push({ key: ++uploadSeq, file, progress: 0, status: 'queued' });
    }
    if (!added.length) return;
    setUploads((us) => [...us, ...added]);
    // Releases that already exist upload right away; new ones on submit.
    if (current) for (const u of added) void startUpload(current, u);
  };

  const cancelUpload = (u: Upload) => {
    controllers.current.get(u.key)?.abort();
    setUploads((us) => us.filter((x) => x.key !== u.key));
  };

  const removeAsset = (a: RestAsset) => {
    setAssets((as) => as.filter((x) => x.id !== a.id));
    deleteReleaseAsset(owner, repo, a.id).then(
      () => invalidateReleases(owner, repo),
      (e: unknown) => {
        setAssets((as) => (as.some((x) => x.id === a.id) ? as : [...as, a]));
        toast({ kind: 'error', title: `Could not delete ${a.name}`, description: errorMessage(e) });
      },
    );
  };

  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    setDragging(false);
    addFiles(e.dataTransfer.files);
  };

  // ---------------------------------------------------------------- notes

  const generate = async () => {
    if (!trimmedTag) {
      setErrors({ tag: 'Choose a tag first to generate release notes.' });
      return;
    }
    setGenerating(true);
    try {
      const notes = await generateReleaseNotes(owner, repo, {
        tag_name: trimmedTag,
        target_commitish: newTag ? target : undefined,
        previous_tag_name: prevTag || undefined,
      });
      if (!title.trim()) setTitle(notes.name);
      setBody((b) => (b.trim() ? `${b.replace(/\s+$/, '')}\n\n${notes.body}` : notes.body));
      setTab('write');
    } catch (e) {
      const v = releaseErrors(e);
      toast({ kind: 'error', title: 'Could not generate release notes', description: v ? Object.values(v)[0] : errorMessage(e) });
    } finally {
      setGenerating(false);
    }
  };

  // ---------------------------------------------------------------- submit

  const makeLatestValue = (): ReleaseInput['make_latest'] => {
    if (prerelease) return wasLatest ? 'false' : undefined;
    if (makeLatest) return 'true';
    return mode === 'new' || wasLatest ? 'false' : undefined;
  };

  const submit = async (intent: Intent) => {
    if (busy) return;
    if (!trimmedTag) {
      setErrors({ tag: 'Choose an existing tag or create a new one.' });
      return;
    }
    setBusy(intent);
    setErrors({});
    const finalDraft = intent === 'draft' ? true : intent === 'publish' ? false : (current?.draft ?? false);
    const fields: ReleaseInput = {
      tag_name: trimmedTag,
      target_commitish: tagExists ? undefined : target,
      name: title.trim(),
      body,
      prerelease,
      make_latest: makeLatestValue(),
    };
    let rel = current;
    try {
      const pending = uploads.filter((u) => u.status !== 'uploading');
      let needsUpdate = !!rel;
      if (!rel) {
        // With files to attach, create a draft first so a failed upload never
        // leaves a half-published release; publish once all files are in.
        const draftFirst = pending.length > 0 && !finalDraft;
        rel = await createRelease(owner, repo, { ...fields, draft: draftFirst || finalDraft });
        setCurrent(rel);
        needsUpdate = draftFirst;
      }
      const saved: RestRelease = rel;
      const results = await Promise.all([...inflight.current.values(), ...pending.map((u) => startUpload(saved, u))]);
      if (results.some((ok) => !ok)) {
        invalidateReleases(owner, repo);
        toast({
          kind: 'error',
          title: 'Some files failed to upload',
          description: saved.draft ? 'The release was saved as a draft. Retry or remove the failed files, then publish.' : 'Retry or remove the failed files, then save again.',
        });
        setBusy(null);
        return;
      }
      if (needsUpdate) rel = await updateRelease(owner, repo, saved.id, { ...fields, draft: finalDraft });
      invalidateReleases(owner, repo);
      primeRelease(owner, repo, rel);
      toast({
        kind: 'success',
        title: finalDraft ? 'Draft saved' : mode === 'published' ? 'Release updated' : `Published ${rel.name || rel.tag_name}`,
      });
      navigate(releaseHref(owner, repo, rel.tag_name));
    } catch (e) {
      setBusy(null);
      if (rel) invalidateReleases(owner, repo);
      const v = releaseErrors(e);
      if (v) setErrors(v);
      else toast({ kind: 'error', title: 'Could not save the release', description: errorMessage(e) });
    }
  };

  const primary: Intent = mode === 'published' ? 'update' : 'publish';
  useShortcuts('Release editor', {
    'mod+enter': { handler: () => void submit(primary), description: mode === 'published' ? 'Update release' : 'Publish release', group: 'Releases', allowInInput: true },
  });

  const uploading = uploads.some((u) => u.status === 'uploading');

  return (
    <form
      className={styles.editor}
      onSubmit={(e) => {
        e.preventDefault();
        void submit(primary);
      }}
      onDragOver={(e) => {
        if (!e.dataTransfer.types.includes('Files')) return;
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) setDragging(false);
      }}
      onDrop={onDrop}
    >
      <nav className={styles.crumbs} aria-label="Breadcrumbs">
        <Link to={base}>Releases</Link>
        <ChevronRightIcon size={14} />
        <strong>{mode === 'new' ? 'New release' : `Edit ${current!.tag_name}`}</strong>
      </nav>
      <h1 className={styles.h1}>{mode === 'new' ? 'Draft a new release' : mode === 'draft' ? 'Edit draft release' : 'Edit release'}</h1>

      {errors.form && (
        <div className={styles.error} role="alert">
          {errors.form}
        </div>
      )}

      <div className={styles.refRow}>
        <div>
          <label className={styles.refLabel} htmlFor={`${ids}-tag`}>
            Tag
          </label>
          <TagCombo id={`${ids}-tag`} value={tagName} onChange={setTagName} tags={tags} invalid={!!errors.tag} />
          {errors.tag ? (
            <div className={cx(styles.tagStatus, styles.fieldError)}>{errors.tag}</div>
          ) : conflict ? (
            <div className={cx(styles.tagStatus, styles.fieldError)}>
              <AlertIcon size={12} />
              <span>
                A <Link to={releaseHref(owner, repo, conflict.tag_name)}>release</Link> for this tag already exists.
              </span>
            </div>
          ) : newTag ? (
            <div className={cx(styles.tagStatus, styles.tagStatusNew)}>
              <PlusIcon size={12} />
              Creates tag <strong>{trimmedTag}</strong> on publish
            </div>
          ) : tagExists ? (
            <div className={styles.tagStatus}>
              <CheckIcon size={12} />
              Existing tag
            </div>
          ) : null}
        </div>
        {newTag && (
          <div>
            <span className={styles.refLabel}>Target</span>
            <RefPicker owner={owner} repo={repo} value={target} onSelect={(ref) => setTarget(ref)} branchesOnly size="md" />
            {errors.target && <div className={cx(styles.tagStatus, styles.fieldError)}>{errors.target}</div>}
          </div>
        )}
        <div className={styles.notesRow}>
          <div>
            <label className={styles.refLabel} htmlFor={`${ids}-prev`}>
              Previous tag
            </label>
            <Select id={`${ids}-prev`} value={prevTag} onChange={(e) => setPrevTag(e.target.value)}>
              <option value="">auto</option>
              {tags
                .filter((t) => t !== trimmedTag)
                .map((t) => (
                  <option key={t} value={t}>
                    {t}
                  </option>
                ))}
            </Select>
          </div>
          <Button leadingIcon={SyncIcon} onClick={() => void generate()} loading={generating} disabled={!trimmedTag}>
            Generate release notes
          </Button>
        </div>
      </div>

      <Field label="Release title" htmlFor={`${ids}-title`} error={errors.name}>
        <Input id={`${ids}-title`} size="lg" value={title} onChange={(e) => setTitle(e.target.value)} placeholder={trimmedTag || 'Release title'} invalid={!!errors.name} />
      </Field>

      <div>
        <div className={styles.bodyHead}>
          <Tabs
            size="sm"
            items={[
              { id: 'write', label: 'Write' },
              { id: 'preview', label: 'Preview' },
            ]}
            value={tab}
            onChange={(t) => setTab(t as 'write' | 'preview')}
          />
          <span className={styles.muted}>Markdown is supported · paste or drop files to attach</span>
          {tab === 'write' && notesAttachments.button}
        </div>
        {tab === 'write' ? (
          <>
            <Textarea
              ref={notesRef}
              className={styles.textarea}
              value={body}
              onChange={(e) => setBody(e.target.value)}
              onPaste={notesAttachments.onPaste}
              onDrop={(e) => {
                e.stopPropagation();
                notesAttachments.onDrop(e);
              }}
              onDragOver={(e) => {
                e.stopPropagation();
                notesAttachments.onDragOver(e);
              }}
              onDragLeave={notesAttachments.onDragLeave}
              placeholder="Describe this release"
              aria-label="Release notes"
            />
            <UploadStatus a={notesAttachments} className={styles.muted} />
          </>
        ) : (
          <div className={styles.preview}>
            <Markdown source={body} repo={`${owner}/${repo}`} />
          </div>
        )}
      </div>

      <div>
        <div className={cx(styles.drop, dragging && styles.dropActive)}>
          <UploadIcon size={16} />
          <span>
            Attach binaries by dropping them here or{' '}
            <button type="button" className={styles.dropLink} onClick={() => fileInput.current?.click()}>
              selecting them
            </button>
            .
          </span>
          <input
            ref={fileInput}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              addFiles(e.target.files);
              e.target.value = '';
            }}
          />
        </div>
        <ul className={styles.uploads}>
          {assets.map((a) => (
            <AssetRow key={a.id} asset={a} trailing={<IconButton icon={TrashIcon} label={`Delete ${a.name}`} size="sm" onClick={() => removeAsset(a)} />} />
          ))}
          {uploads.map((u) => (
            <UploadRow key={u.key} upload={u} onCancel={() => cancelUpload(u)} onRetry={current ? () => void startUpload(current, u) : undefined} />
          ))}
        </ul>
      </div>

      <div className={styles.checks}>
        <label className={styles.check}>
          <input
            type="checkbox"
            checked={prerelease}
            onChange={(e) => {
              setPrerelease(e.target.checked);
              if (e.target.checked) setMakeLatest(false);
            }}
          />
          <span>
            Set as a pre-release
            <span className={styles.checkHint}>This release will be labeled as non-production ready.</span>
          </span>
        </label>
        <label className={styles.check} aria-disabled={prerelease}>
          <input type="checkbox" checked={makeLatest && !prerelease} disabled={prerelease} onChange={(e) => setMakeLatest(e.target.checked)} />
          <span>
            Set as the latest release
            <span className={styles.checkHint}>This release will be labeled as the latest release for this repository.</span>
          </span>
        </label>
      </div>

      <div className={styles.footer}>
        <Button type="submit" variant="primary" loading={busy === primary} disabled={!!busy && busy !== primary} kbd="⌘↵">
          {mode === 'published' ? 'Update release' : 'Publish release'}
        </Button>
        {mode !== 'published' && (
          <Button onClick={() => void submit('draft')} loading={busy === 'draft'} disabled={!!busy && busy !== 'draft'}>
            Save draft
          </Button>
        )}
        <span className={styles.footerSpacer} />
        {uploading && <span className={styles.muted}>Uploading files…</span>}
        <Button variant="ghost" onClick={() => navigate(current ? releaseHref(owner, repo, current.tag_name) : base)} disabled={!!busy}>
          Cancel
        </Button>
      </div>
    </form>
  );
});

function UploadRow({ upload, onCancel, onRetry }: { upload: Upload; onCancel: () => void; onRetry?: () => void }) {
  return (
    <li className={styles.asset}>
      <PackageIcon size={16} className={styles.assetIcon} />
      <span className={styles.assetName}>{upload.file.name}</span>
      <span className={styles.assetMeta}>
        {upload.status === 'error' ? (
          <span className={styles.uploadError} title={upload.error}>
            {upload.error ?? 'Upload failed'}
          </span>
        ) : upload.status === 'queued' ? (
          <span>Uploads on save</span>
        ) : (
          <span
            className={styles.progress}
            role="progressbar"
            aria-label={`Uploading ${upload.file.name}`}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={Math.round(upload.progress * 100)}
          >
            <span className={styles.progressBar} style={{ width: `${Math.max(2, upload.progress * 100)}%`, display: 'block' }} />
          </span>
        )}
        <span>{formatBytes(upload.file.size)}</span>
      </span>
      {upload.status === 'error' && onRetry && <IconButton icon={SyncIcon} label="Retry upload" size="sm" onClick={onRetry} />}
      <IconButton icon={XIcon} label={upload.status === 'uploading' ? 'Cancel upload' : 'Remove'} size="sm" onClick={onCancel} />
    </li>
  );
}

/** Tag input with a filtered list of existing tags; free text creates a new tag. */
function TagCombo({ id, value, onChange, tags, invalid }: { id: string; value: string; onChange: (v: string) => void; tags: string[]; invalid: boolean }) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const q = value.trim().toLowerCase();
  const matches = (q && tags.includes(value.trim()) ? tags : tags.filter((t) => t.toLowerCase().includes(q))).slice(0, 50);
  const listId = `${id}-list`;
  const pick = (t: string) => {
    onChange(t);
    setOpen(false);
  };
  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      setOpen(true);
      const n = matches.length || 1;
      setActive((a) => (e.key === 'ArrowDown' ? (a + 1) % n : (a - 1 + n) % n));
    } else if (e.key === 'Enter' && open && matches[active]) {
      e.preventDefault();
      pick(matches[active]);
    } else if (e.key === 'Escape' && open) {
      e.stopPropagation();
      setOpen(false);
    }
  };
  return (
    <div className={styles.tagBox}>
      <Input
        id={id}
        leadingIcon={TagIcon}
        value={value}
        onChange={(e) => {
          onChange(e.target.value.replace(/\s/g, ''));
          setOpen(true);
          setActive(0);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        onKeyDown={onKeyDown}
        placeholder="Choose or create a tag"
        autoComplete="off"
        spellCheck={false}
        invalid={invalid}
        role="combobox"
        aria-expanded={open && matches.length > 0}
        aria-controls={listId}
        aria-autocomplete="list"
      />
      {open && matches.length > 0 && (
        <ul id={listId} role="listbox" className={styles.tagList}>
          {matches.map((t, i) => (
            <li
              key={t}
              role="option"
              aria-selected={i === active}
              className={styles.tagOption}
              onMouseDown={(e) => {
                e.preventDefault();
                pick(t);
              }}
              onMouseEnter={() => setActive(i)}
            >
              <TagIcon size={12} />
              {t}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

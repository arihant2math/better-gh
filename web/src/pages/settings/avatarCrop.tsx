/**
 * Square avatar cropper on a <canvas>: drag (or arrow keys) to pan, slider /
 * wheel / +− to zoom; exports a 460×460 PNG (JPEG fallback when the PNG
 * would exceed the 1 MiB upload limit).
 */
import { useCallback, useEffect, useId, useRef, useState } from 'react';
import { Button } from '../../ui/Button';
import { Dialog } from '../../ui/Dialog';
import { Spinner } from '../../ui/Spinner';
import styles from './avatarCrop.module.css';

export const AVATAR_OUT = 460;
const VIEW = 300;
const MAX_ZOOM = 5;

export interface CropState {
  /** Canvas px per image px. */
  scale: number;
  /** Viewport centre, in image px. */
  cx: number;
  cy: number;
}

/** Smallest scale at which the image still covers the square viewport. */
export function coverScale(w: number, h: number, view = VIEW): number {
  return view / Math.min(w, h);
}

/** Keep the viewport inside the image. */
export function clampCrop(c: CropState, w: number, h: number, view = VIEW): CropState {
  const min = coverScale(w, h, view);
  const scale = Math.min(Math.max(c.scale, min), min * MAX_ZOOM);
  const half = view / 2 / scale;
  return { scale, cx: Math.min(Math.max(c.cx, half), w - half), cy: Math.min(Math.max(c.cy, half), h - half) };
}

/** Source rectangle (image px) for the current crop. */
export function cropRect(c: CropState, view = VIEW): { sx: number; sy: number; size: number } {
  const size = view / c.scale;
  return { sx: c.cx - size / 2, sy: c.cy - size / 2, size };
}

function toBlob(canvas: HTMLCanvasElement, type: string, quality?: number): Promise<Blob> {
  return new Promise((resolve, reject) => canvas.toBlob((b) => (b ? resolve(b) : reject(new Error('Could not encode the image'))), type, quality));
}

export async function exportCrop(img: CanvasImageSource, c: CropState, limit = 1024 * 1024): Promise<Blob> {
  const out = document.createElement('canvas');
  out.width = AVATAR_OUT;
  out.height = AVATAR_OUT;
  const ctx = out.getContext('2d')!;
  ctx.imageSmoothingQuality = 'high';
  const { sx, sy, size } = cropRect(c);
  ctx.drawImage(img, sx, sy, size, size, 0, 0, AVATAR_OUT, AVATAR_OUT);
  const png = await toBlob(out, 'image/png');
  if (png.size <= limit) return png;
  // Photos can exceed 1 MiB as PNG; the server also accepts JPEG.
  ctx.globalCompositeOperation = 'destination-over';
  ctx.fillStyle = '#fff';
  ctx.fillRect(0, 0, AVATAR_OUT, AVATAR_OUT);
  return toBlob(out, 'image/jpeg', 0.9);
}

async function decode(file: Blob): Promise<HTMLImageElement> {
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    img.src = url;
    await img.decode();
    return img;
  } finally {
    // The decoded image stays usable after revoking.
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }
}

export function AvatarCropDialog({
  file,
  onClose,
  onCropped,
}: {
  file: Blob | null;
  onClose: () => void;
  /** Receives the exported image; may throw (error shown in the dialog). */
  onCropped: (blob: Blob) => Promise<void>;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [img, setImg] = useState<HTMLImageElement | null>(null);
  const [crop, setCrop] = useState<CropState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const drag = useRef<{ x: number; y: number; start: CropState } | null>(null);
  const zoomId = useId();

  useEffect(() => {
    setImg(null);
    setCrop(null);
    setError(null);
    if (!file) return;
    let cancelled = false;
    decode(file).then(
      (i) => {
        if (cancelled) return;
        setImg(i);
        setCrop({ scale: coverScale(i.naturalWidth, i.naturalHeight), cx: i.naturalWidth / 2, cy: i.naturalHeight / 2 });
      },
      () => !cancelled && setError('This file could not be read as an image.'),
    );
    return () => {
      cancelled = true;
    };
  }, [file]);

  const set = useCallback(
    (next: CropState) => {
      if (img) setCrop(clampCrop(next, img.naturalWidth, img.naturalHeight));
    },
    [img],
  );

  // Paint.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !img || !crop) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = VIEW * dpr;
    canvas.height = VIEW * dpr;
    const ctx = canvas.getContext('2d')!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, VIEW, VIEW);
    const { sx, sy, size } = cropRect(crop);
    ctx.drawImage(img, sx, sy, size, size, 0, 0, VIEW, VIEW);
  }, [img, crop]);

  const zoomTo = (scale: number) => crop && set({ ...crop, scale });
  const min = img ? coverScale(img.naturalWidth, img.naturalHeight) : 1;
  const zoom = crop ? crop.scale / min : 1;

  const onKey = (e: React.KeyboardEvent) => {
    if (!crop) return;
    const step = (e.shiftKey ? 50 : 10) / crop.scale;
    const moves: Record<string, [number, number]> = { ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step] };
    const m = moves[e.key];
    if (m) {
      e.preventDefault();
      set({ ...crop, cx: crop.cx + m[0], cy: crop.cy + m[1] });
    } else if (e.key === '+' || e.key === '=') {
      e.preventDefault();
      zoomTo(crop.scale * 1.1);
    } else if (e.key === '-') {
      e.preventDefault();
      zoomTo(crop.scale / 1.1);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      void save();
    }
  };

  const save = async () => {
    if (!img || !crop || busy) return;
    setBusy(true);
    setError(null);
    try {
      const blob = await exportCrop(img, crop);
      if (blob.size > 1024 * 1024) throw new Error('The cropped image is larger than 1 MB. Try a smaller image.');
      await onCropped(blob);
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Upload failed');
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open={!!file}
      onClose={onClose}
      title="Crop your new profile picture"
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" loading={busy} disabled={!crop} onClick={() => void save()}>
            Set new profile picture
          </Button>
        </>
      }
    >
      <div className={styles.wrap}>
        <div className={styles.stage} style={{ width: VIEW, height: VIEW }}>
          {!img && !error && <Spinner size={20} />}
          <canvas
            ref={canvasRef}
            className={styles.canvas}
            style={{ width: VIEW, height: VIEW, display: img ? 'block' : 'none' }}
            tabIndex={0}
            role="img"
            aria-label="Crop area. Drag or use the arrow keys to move the image, plus and minus to zoom."
            data-testid="avatar-crop-canvas"
            onKeyDown={onKey}
            onPointerDown={(e) => {
              if (!crop) return;
              e.currentTarget.setPointerCapture(e.pointerId);
              drag.current = { x: e.clientX, y: e.clientY, start: crop };
            }}
            onPointerMove={(e) => {
              const d = drag.current;
              if (!d) return;
              set({ ...d.start, cx: d.start.cx - (e.clientX - d.x) / d.start.scale, cy: d.start.cy - (e.clientY - d.y) / d.start.scale });
            }}
            onPointerUp={() => (drag.current = null)}
            onPointerCancel={() => (drag.current = null)}
            onWheel={(e) => {
              if (!crop) return;
              zoomTo(crop.scale * (e.deltaY < 0 ? 1.08 : 1 / 1.08));
            }}
          />
          {img && <div className={styles.mask} aria-hidden />}
        </div>
        <div className={styles.zoom}>
          <label htmlFor={zoomId}>Zoom</label>
          <input
            id={zoomId}
            type="range"
            min={1}
            max={MAX_ZOOM}
            step={0.01}
            value={zoom}
            disabled={!crop}
            onChange={(e) => zoomTo(min * Number(e.target.value))}
          />
        </div>
        <p className={styles.hint}>Drag to reposition · arrow keys move · + / − zoom</p>
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
      </div>
    </Dialog>
  );
}

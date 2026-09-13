/**
 * Camera / gallery capture (WP-M5 / F10-17) on top of `@capacitor/camera`.
 *
 * The service only knows how to turn a native photo into a `File`; the chat
 * composer then feeds that file through its *existing* `addFiles()` path
 * (validation, downscaling, MIME sniffing, the `images[]` payload), so a
 * captured photo is sent exactly like a desktop file pick or a paste.
 *
 * `@capacitor/camera` is imported dynamically and only on Capacitor - the
 * desktop bundle never contains it. `imagesSupported()` mirrors the engine's
 * catalog gate (`model_supports_images`) closely enough to hide the controls
 * for the well-known text-only families; the engine remains the authority
 * and still rejects an image for a model that cannot take one.
 */

import { Injectable } from '@angular/core';

import { isCapacitorRuntime } from './secure-store';

export type CameraSourceKind = 'camera' | 'photos';

/** The slice of the `@capacitor/camera` plugin this service uses. */
export interface CameraBridge {
  getPhoto(options: {
    resultType: 'base64';
    source: 'CAMERA' | 'PHOTOS';
    quality: number;
    width: number;
    correctOrientation: boolean;
    saveToGallery: boolean;
  }): Promise<{ base64String?: string; format: string }>;
}

/** Longest edge requested from the native side (matches the composer's cap). */
export const CAPTURE_MAX_EDGE = 2048;
export const CAPTURE_QUALITY = 85;

/**
 * Model ids (lower-cased, `provider/model` or bare) known not to accept
 * image input. Anything else is assumed capable - same default as the
 * engine's catalog for unknown/custom models.
 */
const TEXT_ONLY_PATTERNS: readonly RegExp[] = [
  /(^|\/)deepseek/,
  /(^|\/)o1-mini/,
  /(^|\/)o3-mini/,
  /(^|\/)gpt-3\.5/,
  /(^|\/)gpt-4-turbo-preview/,
  /(^|\/)text-/,
  /(^|\/)codestral/,
  /(^|\/)mistral-(tiny|small|medium|large)/,
  /(^|\/)mixtral/,
  /(^|\/)llama-3\.[013]-(?!.*vision)/,
  /(^|\/)llama3/,
  /(^|\/)qwen2\.5-coder/,
  /(^|\/)gemma/,
  /(^|\/)phi-/,
  /-instruct$/,
];

/** Client-side mirror of the engine's `supports_images` gate (see above). */
export function modelSupportsImages(modelId: string): boolean {
  const id = (modelId ?? '').trim().toLowerCase();
  if (!id) {
    return true;
  }
  return !TEXT_ONLY_PATTERNS.some((re) => re.test(id));
}

/** Decode a base64 payload into a `File` the composer can stage. */
export function fileFromBase64(base64: string, format: string, stem = 'photo'): File {
  const clean = base64.replace(/^data:[^,]*,/, '').replace(/\s+/g, '');
  const binary = atob(clean);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  const ext = (format || 'jpeg').toLowerCase().replace(/^jpg$/, 'jpeg');
  const type = `image/${ext}`;
  return new File([bytes], `${stem}-${Date.now()}.${ext === 'jpeg' ? 'jpg' : ext}`, { type });
}

/** Errors the plugin raises when the user backs out of the picker/camera. */
function isCancellation(err: unknown): boolean {
  const message = (err instanceof Error ? err.message : String(err)).toLowerCase();
  return message.includes('cancel') || message.includes('no image picked');
}

@Injectable({ providedIn: 'root' })
export class CameraService {
  /** Native capture exists only inside the Capacitor shell. */
  available = isCapacitorRuntime();
  private loader: () => Promise<CameraBridge> = defaultLoader;

  /** Test seam. */
  configure(overrides: { available?: boolean; bridge?: CameraBridge }): void {
    if (overrides.available !== undefined) {
      this.available = overrides.available;
    }
    if (overrides.bridge) {
      const bridge = overrides.bridge;
      this.loader = async () => bridge;
    }
  }

  imagesSupported(modelId: string): boolean {
    return modelSupportsImages(modelId);
  }

  /**
   * Open the camera or the photo picker. Resolves with the captured image as
   * a `File`, or `null` when the user cancelled. Rejects on a real failure
   * (permission denied, no camera, plugin missing).
   */
  async pick(source: CameraSourceKind): Promise<File | null> {
    if (!this.available) {
      return null;
    }
    const camera = await this.loader();
    let photo: { base64String?: string; format: string };
    try {
      photo = await camera.getPhoto({
        resultType: 'base64',
        source: source === 'camera' ? 'CAMERA' : 'PHOTOS',
        quality: CAPTURE_QUALITY,
        width: CAPTURE_MAX_EDGE,
        correctOrientation: true,
        saveToGallery: false,
      });
    } catch (err) {
      if (isCancellation(err)) {
        return null;
      }
      throw err;
    }
    if (!photo.base64String) {
      return null;
    }
    return fileFromBase64(photo.base64String, photo.format, source === 'camera' ? 'camera' : 'photo');
  }
}

async function defaultLoader(): Promise<CameraBridge> {
  const { Camera } = await import('@capacitor/camera');
  return Camera as unknown as CameraBridge;
}

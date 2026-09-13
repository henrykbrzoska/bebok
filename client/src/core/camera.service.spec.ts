/**
 * WP-M5 / F10-17: `CameraService` - success path (a native photo becomes a
 * typed `File`), user cancellation, unsupported shell, and the
 * `supports_images` mirror.
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';

import { CameraBridge, CameraService, modelSupportsImages } from './camera.service';

const JPEG_BASE64 = '/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAgGBgcGBQgHBwcJCQgKDBQNDAsLDBkSEw8UHRofHh0aHBwgJC4nICIsIxwcKDcpLDAxNDQ0Hyc5PTgyPC4zNDL/wAALCAABAAEBAREA/8QAFAABAAAAAAAAAAAAAAAAAAAACf/EABQQAQAAAAAAAAAAAAAAAAAAAAD/2gAIAQEAAD8AKp//2Q==';

describe('CameraService (F10-17)', () => {
  let service: CameraService;

  beforeEach(() => {
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    service = TestBed.inject(CameraService);
  });

  it('is unavailable outside Capacitor and then never touches the plugin', async () => {
    expect(service.available).toBeFalse();
    let called = false;
    service.configure({
      bridge: {
        getPhoto: async () => {
          called = true;
          return { base64String: JPEG_BASE64, format: 'jpeg' };
        },
      },
    });
    expect(await service.pick('photos')).toBeNull();
    expect(called).toBeFalse();
  });

  it('turns a native photo into a File with the right type and name', async () => {
    const seen: unknown[] = [];
    const bridge: CameraBridge = {
      getPhoto: async (options) => {
        seen.push(options);
        return { base64String: JPEG_BASE64, format: 'jpeg' };
      },
    };
    service.configure({ available: true, bridge });
    const file = await service.pick('camera');
    expect(file).not.toBeNull();
    expect(file!.type).toBe('image/jpeg');
    expect(file!.name).toMatch(/^camera-\d+\.jpg$/);
    expect(file!.size).toBeGreaterThan(100);
    expect(seen[0]).toEqual(
      jasmine.objectContaining({ resultType: 'base64', source: 'CAMERA', saveToGallery: false }),
    );

    await service.pick('photos');
    expect(seen[1]).toEqual(jasmine.objectContaining({ source: 'PHOTOS' }));
  });

  it('resolves null when the user cancels and rethrows real failures', async () => {
    service.configure({
      available: true,
      bridge: {
        getPhoto: async () => {
          throw new Error('User cancelled photos app');
        },
      },
    });
    expect(await service.pick('photos')).toBeNull();

    service.configure({
      bridge: {
        getPhoto: async () => {
          throw new Error('User denied access to camera');
        },
      },
    });
    await expectAsync(service.pick('camera')).toBeRejectedWithError(/denied/);
  });

  it('mirrors the engine gate for well-known text-only models', () => {
    expect(modelSupportsImages('openai/gpt-4.1')).toBeTrue();
    expect(modelSupportsImages('anthropic/claude-sonnet-4')).toBeTrue();
    expect(modelSupportsImages('')).toBeTrue();
    expect(modelSupportsImages('mycorp/custom-model')).toBeTrue();
    expect(modelSupportsImages('deepseek/deepseek-chat')).toBeFalse();
    expect(modelSupportsImages('deepseek-reasoner')).toBeFalse();
    expect(modelSupportsImages('groq/llama-3.3-70b-versatile')).toBeFalse();
    expect(modelSupportsImages('groq/llama-3.2-11b-vision-preview')).toBeTrue();
    expect(modelSupportsImages('openai/o3-mini')).toBeFalse();
    expect(service.imagesSupported('openai/gpt-3.5-turbo')).toBeFalse();
  });
});

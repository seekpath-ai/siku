import { invoke, convertFileSrc } from '@tauri-apps/api/core';

export interface ResolveImageOptions {
  /** Absolute base directory used to resolve relative attachment paths. */
  attachmentsDir?: string;
}

const remoteCache = new Map<string, Promise<string>>();

function isRemoteUrl(src: string): boolean {
  return /^https?:\/\//.test(src);
}

function isAssetUrl(src: string): boolean {
  return (
    src.startsWith('asset:') ||
    src.startsWith('http://asset.localhost') ||
    src.startsWith('https://asset.localhost')
  );
}

function isAbsolutePath(src: string): boolean {
  // Unix absolute or Windows absolute (e.g. C:\...)
  return src.startsWith('/') || /^[A-Za-z]:[\\/]/.test(src);
}

function resolveLocalImageSrc(src: string, options?: ResolveImageOptions): string {
  if (!src) return src;
  if (src.startsWith('data:') || isAssetUrl(src)) return src;

  let path = src;
  if (!isAbsolutePath(src) && options?.attachmentsDir) {
    const base = options.attachmentsDir.replace(/\\/g, '/').replace(/\/$/, '');
    if (src.startsWith('blobs/') && base.endsWith('/blobs')) {
      // The backend stores blobs under {app_data_dir}/blobs/ and returns
      // relative paths like "blobs/{hash}.png". attachmentsDir itself is that
      // same blobs directory, so we must not append another "blobs/" segment.
      path = `${base}/${src.slice('blobs/'.length)}`;
    } else {
      // Legacy vault attachments or plain filenames resolve under attachmentsDir.
      const relative = src.startsWith('attachments/') ? src.slice('attachments/'.length) : src;
      path = `${base}/${relative}`;
    }
  }

  try {
    return convertFileSrc(path);
  } catch {
    return src;
  }
}

/** Resolve an image src for safe loading inside the Tauri WebView.
 *
 * - data: / asset: URLs pass through unchanged.
 * - Relative attachment paths are resolved against `attachmentsDir` and then
 *   converted to the asset protocol.
 * - Absolute local file paths are converted to the asset protocol.
 * - Remote http(s) URLs are downloaded/cached by the Rust backend and then
 *   converted to the asset protocol so they comply with the existing CSP.
 */
export async function resolveImageUrl(
  src: string,
  options?: ResolveImageOptions
): Promise<string> {
  if (!src) return src;
  if (src.startsWith('data:') || isAssetUrl(src)) return src;

  if (isRemoteUrl(src)) {
    let pending = remoteCache.get(src);
    if (!pending) {
      pending = invoke<string>('cache_remote_image', { url: src })
        .then((relPath) => invoke<string>('resolve_cached_image_path', { relPath }))
        .then((absPath) => convertFileSrc(absPath))
        .catch((err) => {
          remoteCache.delete(src);
          throw err;
        });
      remoteCache.set(src, pending);
    }
    // If caching fails, fall back to the original URL so the browser shows a
    // broken-image indicator rather than swallowing the error silently.
    return pending.catch(() => src);
  }

  return resolveLocalImageSrc(src, options);
}

/** Synchronous variant for contexts that cannot await (e.g. CodeMirror widgets).
 *  Remote URLs are not handled here — use resolveImageUrl for those. */
export function resolveLocalImageUrl(
  src: string,
  options?: ResolveImageOptions
): string {
  return resolveLocalImageSrc(src, options);
}

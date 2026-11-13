/** Device-independent session-artifact references.
 *
 * `siku://artifact/<sha256>.<ext>` points at a content-addressed blob; the
 * backend resolves it to an absolute path on THIS device (and errors while the
 * blob is still being fetched from a peer). */
export const SIKU_ARTIFACT_SCHEME = 'siku://artifact/';

export function isArtifactUri(value?: string | null): value is string {
  return !!value && value.startsWith(SIKU_ARTIFACT_SCHEME);
}

/** Human label fallback when the markdown link text is missing. */
export function artifactUriLabel(uri: string): string {
  const rest = uri
    .slice(SIKU_ARTIFACT_SCHEME.length)
    .split(/[?#]/)[0]
    .trim();
  return rest || '产物';
}

export function safeDecodeUri(uri: string): string {
  try {
    return decodeURIComponent(uri);
  } catch {
    return uri;
  }
}

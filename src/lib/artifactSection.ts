import { SIKU_ARTIFACT_SCHEME } from './artifact';

export interface ArtifactEntry {
  uri: string;
  label: string;
  ext: string;
}

export interface SplitContent {
  body: string;
  artifacts: ArtifactEntry[];
}

const MARKER = /(?:^|\n)[ \t]*📎[ \t]*本轮产物[：:][ \t]*\n?/g;
const BULLET = /^[-*]\s*\[((?:\\.|[^\]])*)\]\((siku:\/\/artifact\/[^)\s]+)\)$/;

function unescapeLabel(label: string): string {
  return label.replace(/\\([\\[\]])/g, '$1');
}

function extOf(uri: string): string {
  const rest = uri.slice(SIKU_ARTIFACT_SCHEME.length).split(/[?#]/)[0];
  return rest.includes('.') ? rest.slice(rest.lastIndexOf('.') + 1).toLowerCase() : '';
}

/**
 * Split the machine-generated artifact footer out of an assistant message.
 *
 * The backend appends:
 *
 *   📎 本轮产物：
 *   - [name](siku://artifact/<hash>.<ext>)
 *
 * Rendering that as Markdown produces one block-level `<li>` per artifact,
 * which wastes a full row per item and makes 10 slide images look like a
 * document outline. We keep the raw DB content (copy/export still include it)
 * and render the footer as a compact grid instead.
 *
 * This parser is deliberately conservative: if the section contains anything
 * that is not an artifact bullet (a user quoting the marker, a future format),
 * the content is left untouched.
 */
export function splitArtifactSection(content: string): SplitContent {
  MARKER.lastIndex = 0;
  let marker: RegExpExecArray | null = null;
  let match: RegExpExecArray | null;
  while ((match = MARKER.exec(content)) !== null) {
    marker = match;
  }
  if (!marker) return { body: content, artifacts: [] };

  const sectionStart = marker.index + marker[0].length;
  const section = content.slice(sectionStart);
  if (!section.trim()) return { body: content, artifacts: [] };

  const artifacts: ArtifactEntry[] = [];
  for (const line of section.split(/\r?\n/)) {
    const text = line.trim();
    if (!text) continue;
    const bullet = text.match(BULLET);
    if (!bullet) return { body: content, artifacts: [] };
    const uri = bullet[2];
    if (artifacts.some((a) => a.uri === uri)) continue;
    artifacts.push({
      uri,
      label: unescapeLabel(bullet[1]).trim() || uri.slice(SIKU_ARTIFACT_SCHEME.length),
      ext: extOf(uri),
    });
  }
  if (artifacts.length === 0) return { body: content, artifacts: [] };

  return {
    body: content.slice(0, marker.index).replace(/\s+$/, ''),
    artifacts,
  };
}

import {
  File,
  FileArchive,
  FileCode2,
  FileImage,
  FileSpreadsheet,
  FileText,
  Presentation,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { SIKU_ARTIFACT_SCHEME } from '@/lib/artifact';

export type ArtifactKind = 'document' | 'image' | 'sheet' | 'code' | 'archive' | 'other';

export function artifactExt(uri: string): string {
  if (!uri.startsWith(SIKU_ARTIFACT_SCHEME)) return '';
  const rest = uri.slice(SIKU_ARTIFACT_SCHEME.length).split(/[?#]/)[0];
  const ext = rest.includes('.') ? rest.slice(rest.lastIndexOf('.') + 1) : '';
  return ext.toLowerCase();
}

export function artifactKind(ext: string): ArtifactKind {
  const e = ext.toLowerCase();
  if (['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'avif', 'ico'].includes(e)) {
    return 'image';
  }
  if (['xls', 'xlsx', 'csv', 'ods'].includes(e)) return 'sheet';
  if (['ppt', 'pptx', 'key', 'odp'].includes(e)) return 'document';
  if (['pdf', 'doc', 'docx', 'md', 'txt', 'rtf', 'odt'].includes(e)) return 'document';
  if (
    [
      'py', 'ps1', 'sh', 'bash', 'zsh', 'ts', 'tsx', 'js', 'jsx', 'json', 'yaml', 'yml',
      'toml', 'rs', 'go', 'java', 'c', 'cc', 'cpp', 'h', 'hpp', 'html', 'css', 'scss',
      'sql', 'rb', 'php',
    ].includes(e)
  ) {
    return 'code';
  }
  if (['zip', 'tar', 'gz', 'tgz', '7z', 'rar', 'bz2', 'xz'].includes(e)) return 'archive';
  return 'other';
}

const ICONS: Record<ArtifactKind, LucideIcon> = {
  document: FileText,
  image: FileImage,
  sheet: FileSpreadsheet,
  code: FileCode2,
  archive: FileArchive,
  other: File,
};

const COLORS: Record<ArtifactKind, string> = {
  document: 'text-sky-400',
  image: 'text-emerald-400',
  sheet: 'text-lime-400',
  code: 'text-amber-400',
  archive: 'text-purple-400',
  other: 'text-codex-muted',
};

export function artworkIcon(ext: string): LucideIcon {
  // `.pptx` deserves the presentation glyph rather than a generic document.
  if (['ppt', 'pptx', 'key', 'odp'].includes(ext.toLowerCase())) return Presentation;
  return ICONS[artifactKind(ext)];
}

export function artworkColor(ext: string): string {
  return COLORS[artifactKind(ext)];
}

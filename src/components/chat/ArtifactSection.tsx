import { useEffect, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import { ChevronDown, ChevronRight, FileImage, FolderOpen, Package } from 'lucide-react';
import { artifactResolve, openLocalPath, revealInFileManager } from '@/lib/tauri';
import { useDialog } from '@/hooks/useDialog';
import { useSyncTick } from '@/stores/syncTickStore';
import type { ArtifactEntry } from '@/lib/artifactSection';
import { ArtifactTypeIcon } from './artifactVisual';
import { artifactKind } from './artifactMeta';

const PATH_CACHE = new Map<string, string>();

function useArtifactPath(uri: string): { path: string | null; failed: boolean } {
  const syncTick = useSyncTick((s) => s.tick);
  const [path, setPath] = useState<string | null>(PATH_CACHE.get(uri) ?? null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const cached = PATH_CACHE.get(uri);
    if (cached) {
      setPath(cached);
      setFailed(false);
      return;
    }
    let live = true;
    setPath(null);
    setFailed(false);
    artifactResolve(uri)
      .then((resolved) => {
        PATH_CACHE.set(uri, resolved);
        if (live) setPath(resolved);
      })
      .catch(() => {
        if (live) setFailed(true);
      });
    return () => {
      live = false;
    };
  }, [uri, syncTick]);

  return { path, failed };
}

function openArtifact(
  path: string | null,
  failed: boolean,
  alert: (message: string, opts?: { title?: string; kind?: 'info' | 'warning' | 'error' }) => void,
) {
  if (!path) {
    alert(failed ? '产物尚未同步到本机，请稍后重试' : '产物正在准备，请稍后重试', {
      title: '打开产物',
      kind: failed ? 'warning' : 'info',
    });
    return;
  }
  openLocalPath(path).catch((err) => alert(String(err), { title: '打开产物', kind: 'error' }));
}

function RevealButton({ path, label }: { path: string; label: string }) {
  const { alert } = useDialog();
  return (
    <button
      type="button"
      onClick={(event) => {
        event.stopPropagation();
        revealInFileManager(path).catch((err) =>
          alert(String(err), { title: '在文件夹中显示', kind: 'error' }),
        );
      }}
      className="shrink-0 rounded p-0.5 text-codex-muted opacity-100 hover:text-codex-primary sm:opacity-0 sm:group-hover:opacity-100 sm:focus-visible:opacity-100"
      title={`在文件夹中显示：${label}`}
    >
      <FolderOpen size={12} />
    </button>
  );
}

function PrimaryArtifact({ artifact }: { artifact: ArtifactEntry }) {
  const { path, failed } = useArtifactPath(artifact.uri);
  const { alert } = useDialog();
  return (
    <div className="group flex min-w-0 items-center gap-2 rounded-lg border border-codex-border/60 bg-codex-bg/30 px-2.5 py-2 transition-colors hover:bg-codex-hover/50">
      <ArtifactTypeIcon ext={artifact.ext} size={18} />
      <button
        type="button"
        onClick={() => openArtifact(path, failed, alert)}
        className="min-w-0 flex-1 text-left"
        title={path ?? (failed ? '尚未同步到本机' : '正在解析产物路径…')}
      >
        <div className={`truncate text-[12px] leading-snug ${path ? 'text-codex-primary' : 'text-codex-muted'}`}>
          {artifact.label}
        </div>
        <div className="mt-0.5 text-[10px] uppercase tracking-wide text-codex-muted">
          {artifact.ext || 'file'}
        </div>
      </button>
      {path && <RevealButton path={path} label={artifact.label} />}
    </div>
  );
}

function ImageArtifact({ artifact }: { artifact: ArtifactEntry }) {
  const { path, failed } = useArtifactPath(artifact.uri);
  const { alert } = useDialog();
  const [broken, setBroken] = useState(false);

  useEffect(() => setBroken(false), [path]);

  return (
    <div className="group relative min-w-0 overflow-hidden rounded-md border border-codex-border/60 bg-codex-bg/30">
      <button
        type="button"
        onClick={() => openArtifact(path, failed, alert)}
        className="block w-full text-left"
        title={path ?? (failed ? '尚未同步到本机' : '正在解析产物路径…')}
      >
        <div className="flex aspect-[4/3] items-center justify-center overflow-hidden bg-codex-bg/50">
          {path && !broken ? (
            <img
              src={convertFileSrc(path)}
              alt={artifact.label}
              loading="lazy"
              onError={() => setBroken(true)}
              className="h-full w-full object-cover transition-transform duration-200 group-hover:scale-[1.02]"
            />
          ) : (
            <FileImage size={20} className="text-emerald-400/70" />
          )}
        </div>
        <div className="truncate px-1.5 py-1 text-[10px] leading-snug text-codex-muted group-hover:text-codex-primary">
          {artifact.label}
        </div>
      </button>
      {path && (
        <div className="absolute right-1 top-1 rounded bg-codex-bg/80 p-0.5 shadow-sm">
          <RevealButton path={path} label={artifact.label} />
        </div>
      )}
    </div>
  );
}

function ProcessArtifact({ artifact }: { artifact: ArtifactEntry }) {
  const { path, failed } = useArtifactPath(artifact.uri);
  const { alert } = useDialog();
  return (
    <div className="group inline-flex max-w-full items-center gap-1.5 rounded-md border border-codex-border/50 bg-codex-bg/20 px-1.5 py-0.5">
      <ArtifactTypeIcon ext={artifact.ext} size={11} />
      <button
        type="button"
        onClick={() => openArtifact(path, failed, alert)}
        className={`max-w-[220px] truncate text-[11px] ${path ? 'text-codex-primary' : 'text-codex-muted'}`}
        title={path ?? (failed ? '尚未同步到本机' : '正在解析产物路径…')}
      >
        {artifact.label}
      </button>
      {path && <RevealButton path={path} label={artifact.label} />}
    </div>
  );
}

const IMAGE_PREVIEW_LIMIT = 8;

export function ArtifactSection({ artifacts }: { artifacts: ArtifactEntry[] }) {
  const primary = artifacts.filter((artifact) => {
    const kind = artifactKind(artifact.ext);
    return kind === 'document' || kind === 'sheet';
  });
  const images = artifacts.filter((artifact) => artifactKind(artifact.ext) === 'image');
  const process = artifacts.filter((artifact) => {
    const kind = artifactKind(artifact.ext);
    return kind !== 'document' && kind !== 'sheet' && kind !== 'image';
  });
  const [processOpen, setProcessOpen] = useState(false);
  const [imagesOpen, setImagesOpen] = useState(false);

  if (artifacts.length === 0) return null;

  // If the turn produced only process files, keep them visible; otherwise
  // they start collapsed so the final deliverable is the first thing seen.
  const processVisible = processOpen || (primary.length === 0 && images.length === 0);

  const visibleImages = imagesOpen ? images : images.slice(0, IMAGE_PREVIEW_LIMIT);
  const hiddenImages = images.length - visibleImages.length;

  return (
    <section className="not-prose mt-3 rounded-lg border border-codex-border/60 bg-codex-hover/15 p-2.5">
      <div className="mb-2 flex items-center gap-1.5 text-[11px] font-medium text-codex-muted">
        <Package size={13} className="text-codex-muted" />
        <span>本轮产物</span>
        <span className="rounded-full bg-codex-hover px-1.5 py-0.5 text-[10px] leading-none text-codex-muted">
          {artifacts.length}
        </span>
      </div>

      {primary.length > 0 && (
        <div className="grid gap-1.5 sm:grid-cols-2">
          {primary.map((artifact) => (
            <PrimaryArtifact key={artifact.uri} artifact={artifact} />
          ))}
        </div>
      )}

      {images.length > 0 && (
        <div className={primary.length > 0 ? 'mt-2' : ''}>
          <div className="grid grid-cols-3 gap-1.5 sm:grid-cols-4">
            {visibleImages.map((artifact) => (
              <ImageArtifact key={artifact.uri} artifact={artifact} />
            ))}
          </div>
          {hiddenImages > 0 && (
            <button
              type="button"
              onClick={() => setImagesOpen(true)}
              className="mt-1.5 text-[11px] text-codex-muted hover:text-codex-primary"
            >
              展开余下 {hiddenImages} 张图片
            </button>
          )}
        </div>
      )}

      {process.length > 0 && (
        <div className={primary.length > 0 || images.length > 0 ? 'mt-2' : ''}>
          <button
            type="button"
            onClick={() => setProcessOpen((open) => !open)}
            className="inline-flex items-center gap-0.5 text-[11px] text-codex-muted hover:text-codex-primary"
          >
            {processVisible ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
            过程文件 {process.length}
          </button>
          {processVisible && (
            <div className="mt-1.5 flex flex-wrap gap-1.5">
              {process.map((artifact) => (
                <ProcessArtifact key={artifact.uri} artifact={artifact} />
              ))}
            </div>
          )}
        </div>
      )}
    </section>
  );
}

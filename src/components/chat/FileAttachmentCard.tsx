import { useEffect, useMemo, useState } from 'react';
import { convertFileSrc } from '@tauri-apps/api/core';
import {
  FileArchive,
  FileSpreadsheet,
  FileText,
  ImageIcon,
  Loader2,
  AlertTriangle,
  ExternalLink,
} from 'lucide-react';
import type { ChatAttachment } from '@/lib/types';
import { fileBrowserGetInfo, fileBrowserOpenInSystem } from '@/lib/tauri';

function humanSize(bytes?: number): string {
  if (!bytes || bytes <= 0) return '';
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

function iconFor(mime: string) {
  if (mime.startsWith('image/')) return ImageIcon;
  if (mime.includes('spreadsheet') || mime.includes('excel')) return FileSpreadsheet;
  if (mime.includes('zip') || mime.includes('gzip') || mime.includes('tar')) return FileArchive;
  return FileText;
}

/**
 * One file the agent sent to the user's chat app.
 *
 * The payload never lives in the message row (docs/remote-channels.md §7.3):
 * this card is the local handle to the file plus the delivery state, so the
 * siku conversation and the phone show the same thing. A file recorded on
 * another device resolves to a greyed-out card instead of a broken link.
 */
export function FileAttachmentCard({ att }: { att: ChatAttachment }) {
  const [missing, setMissing] = useState(false);
  const [busy, setBusy] = useState(false);
  const name = att.name || '文件';
  const size = humanSize(att.size);
  const status = att.status ?? 'ok';
  const Icon = iconFor(att.mime || '');
  const isImage = (att.mime || '').startsWith('image/');
  const thumb = useMemo(
    () => (isImage && att.path && status !== 'pending' ? convertFileSrc(att.path) : ''),
    [att.path, isImage, status],
  );

  // A conversation synced from another device carries the sender's absolute
  // path, which does not exist here. Probe once so the card reads
  // 「文件不在此设备」instead of a broken thumbnail / dead click.
  useEffect(() => {
    if (!att.path || status === 'pending') return;
    let cancelled = false;
    fileBrowserGetInfo(att.path)
      .then(() => {
        if (!cancelled) setMissing(false);
      })
      .catch(() => {
        if (!cancelled) setMissing(true);
      });
    return () => {
      cancelled = true;
    };
  }, [att.path, status]);

  const openFile = async () => {
    if (!att.path || missing || busy) return;
    setBusy(true);
    try {
      await fileBrowserOpenInSystem(att.path);
    } catch {
      // Synced from another device: the path is correct but the file isn't here.
      setMissing(true);
    } finally {
      setBusy(false);
    }
  };

  const statusNode =
    status === 'pending' ? (
      <span className="inline-flex items-center gap-1 text-codex-muted">
        <Loader2 size={10} className="animate-spin" />
        发送中
      </span>
    ) : status === 'failed' ? (
      <span
        className="inline-flex items-center gap-1 text-codex-danger"
        title={att.error || '发送失败'}
      >
        <AlertTriangle size={10} />
        发送失败
      </span>
    ) : (
      <span className="text-codex-muted">已发送到手机</span>
    );

  return (
    <button
      type="button"
      onClick={openFile}
      disabled={missing || status === 'pending' || !att.path}
      title={missing ? '文件仅存在于发送它的设备上' : att.path || name}
      className="group flex items-center gap-2 max-w-[280px] rounded-lg border border-codex-border/60 bg-codex-surface/80 px-2.5 py-2 text-left hover:border-codex-accent/50 disabled:cursor-default disabled:hover:border-codex-border/60"
    >
      {thumb && !missing ? (
        <img
          src={thumb}
          alt={name}
          className="h-9 w-9 shrink-0 rounded object-cover border border-codex-border/50"
          onError={() => setMissing(true)}
        />
      ) : (
        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded bg-codex-hover text-codex-muted">
          <Icon size={16} />
        </span>
      )}
      <span className="min-w-0 flex-1">
        <span className="block truncate text-[12px] text-codex-primary">{name}</span>
        <span className="flex items-center gap-1.5 text-[10px] text-codex-muted">
          {size && <span>{size}</span>}
          {size && <span>·</span>}
          {statusNode}
          {missing && <span>· 文件不在此设备</span>}
        </span>
      </span>
      {!missing && status !== 'pending' && att.path && (
        <ExternalLink size={11} className="shrink-0 text-codex-muted opacity-0 group-hover:opacity-100" />
      )}
    </button>
  );
}

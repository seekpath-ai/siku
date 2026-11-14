import { useCallback, useEffect, useState } from 'react';
import {
  AlertTriangle,
  ArrowDownToLine,
  CheckCircle2,
  Clock3,
  Database,
  FileSearch,
  FileWarning,
  FolderOpen,
  HelpCircle,
  Loader2,
  RefreshCw,
  Trash2,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { useDialog } from '@/hooks/useDialog';
import {
  revealInFileManager,
  storageAudit,
  storageEmptyTrash,
  storageGc,
  storageOpenTrash,
  type StorageAudit,
  type StorageBreakdown,
} from '@/lib/tauri';

const GC_GRACE_DAYS = 7;

function formatBytes(bytes: number): string {
  if (!bytes) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index += 1;
  }
  const fixed = index === 0 || value >= 10 ? value.toFixed(0) : value.toFixed(1);
  return `${fixed} ${units[index]}`;
}

function formatScannedAt(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString('zh-CN');
}

function StatCard({
  label,
  data,
  icon: Icon,
  tone,
  hint,
}: {
  label: string;
  data: StorageBreakdown;
  icon: LucideIcon;
  tone: string;
  hint?: string;
}) {
  return (
    <div className="rounded-xl border border-surface-hover bg-surface p-4">
      <div className="flex items-center gap-2 text-xs text-text-secondary">
        <Icon size={14} className={tone} />
        <span>{label}</span>
      </div>
      <div className="mt-2 text-xl font-semibold text-text-primary">{formatBytes(data.bytes)}</div>
      <div className="mt-0.5 text-[11px] text-text-secondary">
        {data.files.toLocaleString('zh-CN')} 个文件
      </div>
      {hint && <div className="mt-1 text-[10px] leading-relaxed text-text-secondary/80">{hint}</div>}
    </div>
  );
}

export function StorageSettings() {
  const { confirm, alert } = useDialog();
  const [audit, setAudit] = useState<StorageAudit | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [gcBusy, setGcBusy] = useState(false);
  const [trashBusy, setTrashBusy] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setAudit(await storageAudit());
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const reveal = (path: string) => {
    revealInFileManager(path).catch((err) => setError(String(err)));
  };

  const openTrash = () => {
    storageOpenTrash().catch((err) => setError(String(err)));
  };

  const handleGc = async () => {
    if (gcBusy) return;
    setGcBusy(true);
    setError(null);
    try {
      const plan = await storageGc(true, GC_GRACE_DAYS);
      if (plan.eligible.files === 0) {
        await alert(
          `没有符合条件的文件。只清理 ${GC_GRACE_DAYS} 天前、且当前和历史都没有引用的 blob；` +
            '当前无引用文件可能都还在宽限期内。',
          { title: '清理无引用文件', kind: 'info' },
        );
        return;
      }
      const ok = await confirm(
        `将 ${plan.eligible.files.toLocaleString('zh-CN')} 个文件（${formatBytes(plan.eligible.bytes)}）移到回收站？\n\n` +
          `只处理 ${GC_GRACE_DAYS} 天前、当前和历史都没有引用的 blob。` +
          '文件会移到 blobs/.trash，不会永久删除。',
        { title: '清理无引用文件', danger: true },
      );
      if (!ok) return;
      const result = await storageGc(false, GC_GRACE_DAYS);
      await refresh();
      const failure = result.errors.length
        ? `\n\n${result.errors.length} 个文件失败，例如：${result.errors[0]}`
        : '';
      await alert(
        `已移动 ${result.moved.files} 个文件（${formatBytes(result.moved.bytes)}）到回收站。${failure}`,
        { title: '清理完成', kind: result.errors.length ? 'warning' : 'success' },
      );
    } catch (err) {
      setError(String(err));
    } finally {
      setGcBusy(false);
    }
  };

  const handleEmptyTrash = async () => {
    if (trashBusy || !audit || audit.trash.files === 0) return;
    const ok = await confirm(
      `永久删除回收站中的 ${audit.trash.files.toLocaleString('zh-CN')} 个文件` +
        `（${formatBytes(audit.trash.bytes)}）？此操作无法恢复。`,
      { title: '清空回收站', danger: true },
    );
    if (!ok) return;
    setTrashBusy(true);
    setError(null);
    try {
      const result = await storageEmptyTrash();
      await refresh();
      await alert(`已永久删除 ${result.files} 个文件（${formatBytes(result.bytes)}）。`, {
        title: '回收站已清空',
        kind: 'success',
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setTrashBusy(false);
    }
  };

  return (
    <div className="space-y-6">
      <div>
        <div className="flex items-center gap-2 text-sm font-medium text-text-primary">
          <Database size={16} className="text-primary" />
          <span>存储审计</span>
        </div>
        <p className="mt-1 text-xs leading-relaxed text-text-secondary">
          扫描本地 blob 与数据库引用，统计当前引用、仅历史引用、无引用文件和同步临时文件。
          审计本身只读；清理操作只把旧的无引用 blob 移到回收站。
        </p>
      </div>

      <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-surface-hover bg-surface p-4">
        <div className="min-w-0">
          <div className="text-xs text-text-secondary">本地数据目录</div>
          <div className="mt-1 break-all text-xs text-text-primary">
            {audit?.data_dir ?? '扫描中…'}
          </div>
          {audit && (
            <div className="mt-1 break-all text-[10px] text-text-secondary">
              blob 根目录：{audit.blob_root}
            </div>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-2">
          <button
            type="button"
            onClick={() => audit && reveal(audit.data_dir)}
            disabled={!audit}
            className="inline-flex items-center gap-1.5 rounded-lg border border-surface-hover px-3 py-2 text-xs text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary disabled:opacity-50"
          >
            <FolderOpen size={14} />
            打开目录
          </button>
          <button
            type="button"
            onClick={() => void refresh()}
            disabled={loading}
            className="inline-flex items-center gap-1.5 rounded-lg border border-surface-hover px-3 py-2 text-xs text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary disabled:opacity-50"
          >
            {loading ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />}
            {loading ? '扫描中…' : '重新扫描'}
          </button>
        </div>
      </div>

      {error && (
        <div className="flex items-start gap-2 rounded-lg border border-red-500/30 bg-red-500/10 p-3 text-xs leading-relaxed text-red-300">
          <AlertTriangle size={14} className="mt-0.5 shrink-0" />
          <span className="break-all">{error}</span>
        </div>
      )}

      {!audit && loading && (
        <div className="flex items-center justify-center gap-2 rounded-xl border border-surface-hover bg-surface py-10 text-xs text-text-secondary">
          <Loader2 size={16} className="animate-spin" />
          正在扫描本地 blob…
        </div>
      )}

      {audit && (
        <>
          <div className="flex flex-wrap items-baseline justify-between gap-2">
            <div className="text-sm text-text-primary">
              Blob 总计 <span className="font-semibold">{formatBytes(audit.total.bytes)}</span>
              <span className="ml-1 text-xs text-text-secondary">
                / {audit.total.files.toLocaleString('zh-CN')} 个文件
              </span>
            </div>
            <div className="text-[10px] text-text-secondary">
              扫描于 {formatScannedAt(audit.scanned_at)}
            </div>
          </div>

          <div className="grid grid-cols-2 gap-3">
            <StatCard
              label="当前引用"
              data={audit.current}
              icon={CheckCircle2}
              tone="text-emerald-400"
              hint="文献、笔记、Vault、产物和聊天内容正在使用"
            />
            <StatCard
              label="仅历史引用"
              data={audit.history_only}
              icon={Clock3}
              tone="text-sky-400"
              hint="只被笔记版本、工具步骤或记忆引用"
            />
            <StatCard
              label="无引用"
              data={audit.orphan}
              icon={FileWarning}
              tone="text-amber-400"
              hint="本地数据库没有找到引用；先不要手动删除"
            />
            <StatCard
              label="无法识别"
              data={audit.unknown}
              icon={HelpCircle}
              tone="text-text-secondary"
              hint="不是标准 blob 文件名，通常是系统临时文件"
            />
          </div>

          <div className="grid grid-cols-2 gap-3">
            <StatCard
              label="同步临时"
              data={audit.staging}
              icon={ArrowDownToLine}
              tone="text-violet-400"
              hint="blobs/.incoming 中未完成的分片"
            />
            <StatCard
              label="回收站"
              data={audit.trash}
              icon={Trash2}
              tone="text-rose-400"
              hint="blobs/.trash 中等待清理的文件"
            />
          </div>

          {audit.duplicate_ext_groups > 0 && (
            <div className="rounded-lg border border-surface-hover bg-surface px-3 py-2 text-xs text-text-secondary">
              发现 {audit.duplicate_ext_groups} 组同内容多扩展名的 blob；它们可能各有一份副本。
            </div>
          )}

          <div className="rounded-xl border border-surface-hover bg-surface p-4">
            <div className="flex items-center gap-2 text-xs font-medium text-text-secondary">
              <Trash2 size={14} />
              清理
            </div>
            <p className="mt-1 text-[11px] leading-relaxed text-text-secondary">
              只处理 {GC_GRACE_DAYS} 天前、当前和历史都没有引用的 blob；先移到回收站，不直接删除。
            </p>
            <div className="mt-3 flex flex-wrap gap-2">
              <button
                type="button"
                onClick={() => void handleGc()}
                disabled={loading || gcBusy || trashBusy}
                className="inline-flex items-center gap-1.5 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs text-amber-300 transition-colors hover:bg-amber-500/15 disabled:opacity-50"
              >
                {gcBusy ? <Loader2 size={14} className="animate-spin" /> : <Trash2 size={14} />}
                {gcBusy ? '正在扫描…' : '清理无引用文件'}
              </button>
              <button
                type="button"
                onClick={() => void handleEmptyTrash()}
                disabled={loading || gcBusy || trashBusy || audit.trash.files === 0}
                className="inline-flex items-center gap-1.5 rounded-lg border border-rose-500/30 bg-rose-500/10 px-3 py-2 text-xs text-rose-300 transition-colors hover:bg-rose-500/15 disabled:opacity-50"
              >
                {trashBusy ? <Loader2 size={14} className="animate-spin" /> : <Trash2 size={14} />}
                清空回收站
              </button>
              <button
                type="button"
                onClick={openTrash}
                className="inline-flex items-center gap-1.5 rounded-lg border border-surface-hover px-3 py-2 text-xs text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
              >
                <FolderOpen size={14} />
                打开回收站
              </button>
            </div>
          </div>

          <div className="rounded-xl border border-surface-hover bg-surface p-4">
            <div className="flex items-center gap-2 text-xs font-medium text-text-secondary">
              <FileSearch size={14} />
              最大的无引用文件
            </div>
            {audit.top_orphans.length === 0 ? (
              <p className="mt-3 text-xs text-text-secondary">没有发现无引用 blob。</p>
            ) : (
              <div className="mt-3 space-y-1.5">
                {audit.top_orphans.map((file) => (
                  <div
                    key={file.path}
                    className="flex items-center gap-2 rounded-lg px-2 py-1.5 text-xs hover:bg-surface-hover"
                  >
                    <FileWarning size={13} className="shrink-0 text-amber-400" />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-text-primary" title={file.path}>
                        {file.name}
                      </div>
                      <div className="text-[10px] text-text-secondary">
                        {formatBytes(file.bytes)} · {file.ext || '无扩展名'}
                      </div>
                    </div>
                    <button
                      type="button"
                      onClick={() => reveal(file.path)}
                      className="shrink-0 rounded p-1 text-text-secondary hover:text-text-primary"
                      title="在文件夹中显示"
                    >
                      <FolderOpen size={13} />
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>

          <div className="flex items-start gap-2 rounded-lg border border-amber-500/20 bg-amber-500/5 p-3 text-[11px] leading-relaxed text-amber-300/90">
            <AlertTriangle size={14} className="mt-0.5 shrink-0" />
            <span>
              审计本身不会删除文件。清理只会把满足宽限期、当前与历史都无引用的 blob 移到回收站；
              只有“清空回收站”才会永久删除。无引用也不等于可以立即删除：文件可能刚写入，或被其他设备引用。
            </span>
          </div>
        </>
      )}
    </div>
  );
}

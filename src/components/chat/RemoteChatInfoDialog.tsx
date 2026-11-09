import { useEffect, useState } from 'react';
import { Users, X, Loader2, Crown } from 'lucide-react';
import { remoteChatInfo, type RemoteChatInfo } from '@/lib/tauri';
import { Modal } from '@/components/ui/Modal';

interface Props {
  channel: string;
  chatId: string;
  onClose: () => void;
}

/** 「查看群信息」模态框：群名、群公告、成员列表（飞书限定，微信无群概念）。 */
export function RemoteChatInfoDialog({ channel, chatId, onClose }: Props) {
  const [info, setInfo] = useState<RemoteChatInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    remoteChatInfo(channel, chatId).then(setInfo).catch((e) => setError(String(e)));
  }, [channel, chatId]);


  return (
    <Modal
      onClose={onClose}
      backdropClassName="bg-black/50 backdrop-blur-sm"
    >
      <div className="relative w-[420px] max-w-[94vw] max-h-[80vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-2 px-4 py-2.5 border-b border-surface-hover shrink-0">
          <Users size={15} className="text-text-secondary" />
          <span className="text-sm font-medium text-text-primary">群信息</span>
          <div className="flex-1" />
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭"
          >
            <X size={14} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto px-4 py-3">
          {!info && !error && (
            <div className="flex items-center justify-center py-10 text-text-secondary">
              <Loader2 size={18} className="animate-spin" />
            </div>
          )}
          {error && <div className="text-sm text-red-400 whitespace-pre-wrap">{error}</div>}
          {info && (
            <>
              <div className="text-base font-semibold text-text-primary">{info.name || '（未命名群）'}</div>
              {info.description && (
                <div className="mt-1 text-xs text-text-secondary whitespace-pre-wrap">{info.description}</div>
              )}
              <div className="mt-1 text-[11px] text-text-secondary/60">成员 {info.member_count || info.members.length} 人</div>
              <div className="mt-3 space-y-1">
                {info.members.map((m) => (
                  <div key={m.id || m.name} className="flex items-center gap-2 px-2 py-1.5 rounded-lg hover:bg-surface-hover">
                    <span className="w-6 h-6 rounded-full bg-primary/15 text-primary flex items-center justify-center text-[11px] shrink-0">
                      {(m.name || '?').slice(0, 1)}
                    </span>
                    <span className="text-sm text-text-primary truncate">{m.name || m.id}</span>
                    {m.is_owner && (
                      <span className="flex items-center gap-0.5 text-[10px] text-amber-400 shrink-0">
                        <Crown size={10} />
                        群主
                      </span>
                    )}
                  </div>
                ))}
              </div>
            </>
          )}
        </div>
      </div>
    </Modal>
  );
}

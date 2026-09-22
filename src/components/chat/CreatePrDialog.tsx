import { useCallback, useEffect, useState } from 'react';
import { GitPullRequest, Loader2, X, ExternalLink } from 'lucide-react';
import {
  gitHostSetToken,
  gitHostTokenStatus,
  gitPushBranch,
  gitRemoteInfo,
  prCreate,
  type GitRemoteInfo,
} from '@/lib/tauri';

interface CreatePrDialogProps {
  projectId: string;
  projectName: string;
  onClose: () => void;
}

const TOKEN_GUIDE: Record<string, string> = {
  github: 'https://github.com/settings/tokens（需 repo 权限）',
  gitee: 'https://gitee.com/profile/personal_access_tokens（需 projects 权限）',
};

/** Create-PR dialog for a project: inspect origin (GitHub/Gitee), collect a
 *  PAT on first use (device-local, API-only — git push uses the user's own
 *  credentials), push the current branch, then open the PR via the platform
 *  API. */
export function CreatePrDialog({ projectId, projectName, onClose }: CreatePrDialogProps) {
  const [info, setInfo] = useState<GitRemoteInfo | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [hasToken, setHasToken] = useState(false);
  const [tokenInput, setTokenInput] = useState('');
  const [title, setTitle] = useState('');
  const [body, setBody] = useState('');
  const [base, setBase] = useState('main');
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [prUrl, setPrUrl] = useState<string | null>(null);

  useEffect(() => {
    gitRemoteInfo(projectId)
      .then(async (i) => {
        setInfo(i);
        setTitle(i.branch);
        const status = await gitHostTokenStatus().catch(() => null);
        setHasToken(status ? status[i.platform] : false);
      })
      .catch((e) => setLoadError(e instanceof Error ? e.message : String(e)));
  }, [projectId]);

  const saveToken = async () => {
    if (!info || !tokenInput.trim()) return;
    setBusy('token');
    setError(null);
    try {
      await gitHostSetToken(info.platform, tokenInput.trim());
      setHasToken(true);
      setTokenInput('');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  };

  const handleCreate = async () => {
    if (!info || !title.trim()) return;
    setError(null);
    try {
      setBusy('push');
      await gitPushBranch(projectId);
      setBusy('pr');
      const pr = await prCreate(projectId, title.trim(), body.trim() || undefined, base.trim() || undefined);
      setPrUrl(pr.htmlUrl);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  };

  const openPr = useCallback(async (url: string) => {
    const { open } = await import('@tauri-apps/plugin-shell');
    open(url).catch(() => window.open(url, '_blank', 'noopener'));
  }, []);

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-full max-w-md mx-4 bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        <div className="flex items-center gap-3 px-4 py-3 border-b border-surface-hover">
          <GitPullRequest size={18} className="text-primary" />
          <span className="text-sm font-medium text-text-primary">创建拉取请求（{projectName}）</span>
          <button
            onClick={onClose}
            className="ml-auto p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          >
            <X size={14} />
          </button>
        </div>

        <div className="px-4 py-4 space-y-3">
          {loadError && <div className="text-xs text-red-400">{loadError}</div>}

          {!loadError && !info && (
            <div className="flex items-center gap-2 text-xs text-text-secondary">
              <Loader2 size={13} className="animate-spin" /> 正在读取仓库信息…
            </div>
          )}

          {info && (
            <>
              <div className="text-xs text-text-secondary rounded-lg bg-background/40 border border-surface-hover px-3 py-2">
                {info.platform === 'github' ? 'GitHub' : 'Gitee'} · {info.owner}/{info.repo} · 分支{' '}
                <span className="font-mono">{info.branch}</span>
              </div>

              {!hasToken ? (
                <div className="space-y-2">
                  <p className="text-xs text-text-secondary leading-relaxed">
                    首次使用需要 {info.platform === 'github' ? 'GitHub' : 'Gitee'} 访问令牌（PAT）。
                    令牌仅存储在本机（不同步），只用于平台 API；git push 使用你本机的 git 凭证。
                  </p>
                  <p className="text-[11px] text-text-secondary/60">
                    创建令牌：{TOKEN_GUIDE[info.platform]}
                  </p>
                  <div className="flex items-center gap-2">
                    <input
                      type="password"
                      value={tokenInput}
                      onChange={(e) => setTokenInput(e.target.value)}
                      placeholder="粘贴 PAT"
                      className="flex-1 h-8 px-2.5 rounded-lg bg-background border border-surface-hover text-xs text-text-primary outline-none focus:border-primary/50"
                    />
                    <button
                      onClick={saveToken}
                      disabled={!tokenInput.trim() || busy === 'token'}
                      className="h-8 px-3 rounded-lg text-xs bg-primary/15 text-primary hover:bg-primary/25 transition-colors disabled:opacity-40 shrink-0"
                    >
                      保存
                    </button>
                  </div>
                </div>
              ) : prUrl ? (
                <div className="text-center py-2 space-y-2">
                  <p className="text-sm text-emerald-400">拉取请求已创建</p>
                  <button
                    onClick={() => openPr(prUrl)}
                    className="inline-flex items-center gap-1.5 text-xs text-primary hover:underline"
                  >
                    <ExternalLink size={12} />
                    {prUrl}
                  </button>
                </div>
              ) : (
                <>
                  <div>
                    <label className="block text-xs text-text-secondary mb-1">标题</label>
                    <input
                      value={title}
                      onChange={(e) => setTitle(e.target.value)}
                      className="w-full h-8 px-2.5 rounded-lg bg-background border border-surface-hover text-xs text-text-primary outline-none focus:border-primary/50"
                    />
                  </div>
                  <div>
                    <label className="block text-xs text-text-secondary mb-1">描述（可选）</label>
                    <textarea
                      value={body}
                      onChange={(e) => setBody(e.target.value)}
                      rows={3}
                      className="w-full px-2.5 py-2 rounded-lg bg-background border border-surface-hover text-xs text-text-primary outline-none focus:border-primary/50 resize-none"
                    />
                  </div>
                  <div>
                    <label className="block text-xs text-text-secondary mb-1">目标分支（base）</label>
                    <input
                      value={base}
                      onChange={(e) => setBase(e.target.value)}
                      className="w-full h-8 px-2.5 rounded-lg bg-background border border-surface-hover text-xs text-text-primary outline-none focus:border-primary/50"
                    />
                  </div>
                </>
              )}
            </>
          )}

          {error && <div className="text-xs text-red-400 break-all">{error}</div>}
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-surface-hover">
          <button
            onClick={onClose}
            className="px-3 py-1.5 rounded-lg text-xs text-text-secondary hover:text-text-primary border border-surface-hover transition-colors"
          >
            关闭
          </button>
          {info && hasToken && !prUrl && (
            <button
              onClick={handleCreate}
              disabled={!!busy || !title.trim()}
              className="px-3 py-1.5 rounded-lg text-xs bg-primary/15 text-primary hover:bg-primary/25 transition-colors disabled:opacity-40 disabled:cursor-not-allowed flex items-center gap-1.5"
            >
              {busy && <Loader2 size={12} className="animate-spin" />}
              {busy === 'push' ? '推送分支…' : busy === 'pr' ? '创建 PR…' : '推送并创建 PR'}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

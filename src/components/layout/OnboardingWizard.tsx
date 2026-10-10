import { useEffect, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { X, MessageSquare, Library, Compass } from 'lucide-react';
import { ModelSetupStep } from '../onboarding/ModelSetupStep';
import { ImportPapersStep } from '../onboarding/ImportPapersStep';
import { NotesStep } from '../onboarding/NotesStep';
import { llmProviderList } from '@/lib/tauri';

const steps = [
  {
    key: 'welcome',
    title: '欢迎使用 思库',
    desc: 'AI-Native 桌面智能体平台，让灵感涌动。管理文献、划词翻译、智能体对话、多端同步——接下来的几步会带你快速上手。',
  },
  {
    key: 'model',
    title: '配置 AI 模型',
    desc: '对话、翻译、智能体都依赖模型。选一个厂商，填入 API Key 即可解锁全部 AI 功能。',
  },
  {
    key: 'papers',
    title: '导入第一篇文献',
    desc: '导入 PDF 后自动提取元数据，阅读时可划词翻译、摘录到智思。此步可跳过。',
  },
  {
    key: 'notes',
    title: '准备你的笔记',
    desc: '笔记支持 Markdown、大纲与多端同步。此步可跳过。',
  },
  {
    key: 'finish',
    title: '开始探索',
    desc: '桌面上的小球是 AI 助手，点开即可对话；不需要可在 设置 → 通用 关闭。',
  },
] as const;

interface Props {
  /** markComplete=false: dismissed early (Esc / ✕) — the wizard returns on
   *  next launch. true: finished or "不再显示" — never show again. */
  onDone: (markComplete: boolean) => void;
}

export function OnboardingWizard({ onDone }: Props) {
  const [step, setStep] = useState(0);
  const [modelReady, setModelReady] = useState(false);
  const navigate = useNavigate();

  const current = steps[step];
  const isLast = step === steps.length - 1;

  // Esc: skip for THIS launch only — a dismissed wizard must not strand a
  // new user without an LLM configured, so it reappears next launch.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onDone(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onDone]);

  // The finish step enables "试试对话" only when a model is actually usable.
  useEffect(() => {
    if (!isLast) return;
    llmProviderList()
      .then((list) => {
        setModelReady(list.some((p) => p.api_key || p.provider === 'ollama'));
      })
      .catch(() => {});
  }, [isLast]);

  const jumpTo = (to: string) => {
    onDone(true);
    navigate({ to });
  };

  return (
    <div className="fixed inset-0 z-[9000] flex items-center justify-center bg-[rgba(10,10,14,0.85)] backdrop-blur">
      {/* Draggable top strip so the window can still be moved during onboarding */}
      <div
        data-tauri-drag-region
        className="titlebar-drag absolute top-0 left-0 right-0 h-[38px] z-[1]"
      />

      <div className="relative z-[2] w-[92%] max-w-[520px] rounded-2xl bg-surface border border-surface-hover shadow-2xl px-8 py-7 text-text-primary">
        {/* Close = skip this launch (the wizard returns next time) */}
        <button
          onClick={() => onDone(false)}
          className="absolute top-3 right-3 p-1.5 rounded-lg text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          title="暂时跳过（下次启动还会显示）"
          aria-label="暂时跳过"
        >
          <X size={16} />
        </button>

        {/* Brand */}
        <div className="flex items-center justify-center gap-2 mb-4">
          <img src="/splash-logo.svg" alt="思库" className="w-8 h-8" />
          <span className="text-sm font-semibold tracking-wide">思库</span>
        </div>

        {/* Step indicator: completed ✓, active pill, upcoming dot */}
        <div className="flex items-center justify-center gap-1.5 mb-5">
          {steps.map((_, i) =>
            i < step ? (
              <span
                key={i}
                className="w-5 h-5 rounded-full bg-primary/20 text-primary flex items-center justify-center"
              >
                <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
                  <path d="M1.5 5.5L4 8L8.5 2.5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
                </svg>
              </span>
            ) : (
              <span
                key={i}
                className={`h-2 rounded-full transition-all duration-300 ${
                  i === step ? 'w-5 bg-primary' : 'w-2 bg-surface-hover'
                }`}
              />
            )
          )}
        </div>

        {/* Content (animated on step change) */}
        <div key={step} className="flex flex-col" style={{ animation: 'siku-onboard-in 0.28s ease' }}>
          <h2 className="text-lg font-semibold mb-1.5 text-center text-text-primary">{current.title}</h2>
          {current.desc && (
            <p className="text-xs text-text-secondary leading-relaxed mb-4 text-center">{current.desc}</p>
          )}

          {current.key === 'model' && (
            <div className="mb-4">
              <ModelSetupStep />
            </div>
          )}
          {current.key === 'papers' && (
            <div className="mb-4">
              <ImportPapersStep />
            </div>
          )}
          {current.key === 'notes' && (
            <div className="mb-4">
              <NotesStep />
            </div>
          )}

          {current.key === 'finish' && (
            <div className="flex flex-col gap-2 mb-2">
              <button
                onClick={() => jumpTo('/chat')}
                disabled={!modelReady}
                className="flex items-center justify-center gap-2 px-4 py-2.5 rounded-xl bg-primary text-white text-sm font-medium hover:opacity-90 disabled:opacity-40 transition-opacity"
                title={modelReady ? undefined : '先在上一步配置模型'}
              >
                <MessageSquare size={15} />
                试试对话{modelReady ? '' : '（需先配置模型）'}
              </button>
              <div className="flex gap-2">
                <button
                  onClick={() => jumpTo('/library')}
                  className="flex-1 flex items-center justify-center gap-2 px-4 py-2.5 rounded-xl border border-surface-hover text-sm text-text-primary hover:bg-surface-hover transition-colors"
                >
                  <Library size={15} />
                  去图书馆
                </button>
                <button
                  onClick={() => onDone(true)}
                  className="flex-1 flex items-center justify-center gap-2 px-4 py-2.5 rounded-xl border border-surface-hover text-sm text-text-secondary hover:bg-surface-hover transition-colors"
                >
                  <Compass size={15} />
                  随便看看
                </button>
              </div>
            </div>
          )}
        </div>

        {/* Navigation row */}
        {!isLast && (
          <div className="flex items-center justify-center gap-2 mt-1">
            {step > 0 && (
              <button
                onClick={() => setStep((s) => s - 1)}
                className="px-4 py-2 rounded-lg border border-surface-hover text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors text-sm"
              >
                上一步
              </button>
            )}
            <button
              onClick={() => setStep((s) => s + 1)}
              className="px-6 py-2 rounded-lg bg-primary text-white text-sm font-medium hover:opacity-90 transition-opacity"
            >
              下一步
            </button>
          </div>
        )}

        {/* Opt out entirely — the only mid-wizard path that marks complete */}
        {!isLast && (
          <div className="mt-3 flex justify-center">
            <button
              onClick={() => onDone(true)}
              className="px-2 py-1 text-text-secondary/60 text-xs hover:text-text-secondary transition-colors"
            >
              不再显示引导
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

import { useEffect, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { X } from 'lucide-react';

const steps = [
  {
    emoji: '📚',
    title: '欢迎使用 思库',
    desc: 'AI-Native 桌面智能体平台，让灵感涌动。管理文献、翻译论文、构建知识体系，让 AI 成为你的研究伙伴。',
  },
  {
    emoji: '⚙️',
    title: '配置 AI 模型',
    desc: '前往设置页面，配置 LLM API Key 和模型。支持 DeepSeek、OpenAI、Claude 等多种 provider。',
    action: { label: '现在就去配置', to: '/settings' },
  },
  {
    emoji: '📄',
    title: '导入文献',
    desc: '支持导入 PDF 论文，自动提取元数据。你可以拖拽 PDF 文件到窗口，或在文献库中点击导入。',
    action: { label: '现在就去导入', to: '/library' },
  },
  {
    emoji: '🚀',
    title: '开始探索',
    desc: '使用智能体对话、双语翻译、知识库管理、科研追踪等功能，让 AI 成为你的研究助手。',
  },
];

interface Props {
  onDone: () => void;
}

export function OnboardingWizard({ onDone }: Props) {
  const [step, setStep] = useState(0);
  const navigate = useNavigate();

  const current = steps[step];
  const isLast = step === steps.length - 1;

  // Esc closes the onboarding wizard at any step.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onDone();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onDone]);

  // Primary action: advance through steps, finish on the last one.
  const handlePrimary = () => {
    if (isLast) {
      onDone();
    } else {
      setStep((s) => s + 1);
    }
  };

  // Separate intent: leave the wizard and jump straight to the target page.
  const handleActionJump = () => {
    onDone();
    if (current.action) {
      navigate({ to: current.action.to });
    }
  };

  return (
    <div className="fixed inset-0 z-[9000] flex items-center justify-center bg-[rgba(10,10,14,0.85)] backdrop-blur">
      {/* Draggable top strip so the window can still be moved during onboarding */}
      <div
        data-tauri-drag-region
        className="titlebar-drag absolute top-0 left-0 right-0 h-[38px] z-[1]"
      />

      <div className="relative z-[2] w-[90%] max-w-[420px] rounded-2xl bg-surface border border-surface-hover shadow-2xl px-10 py-9 text-center text-text-primary">
        {/* Close button */}
        <button
          onClick={onDone}
          className="absolute top-3 right-3 p-1.5 rounded-lg text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
          aria-label="关闭引导"
        >
          <X size={16} />
        </button>

        {/* Brand */}
        <div className="flex items-center justify-center gap-2 mb-5">
          <div className="w-8 h-8 rounded-full bg-gradient-to-br from-primary to-amber-700 flex items-center justify-center shadow-lg">
            <div className="relative w-[18px] h-[18px]">
              <div className="absolute top-1 left-0 w-1.5 h-1.5 rounded-full bg-background" />
              <div className="absolute top-1 right-0 w-1.5 h-1.5 rounded-full bg-background" />
              <div className="absolute bottom-0 left-1/2 -translate-x-1/2 w-2.5 h-1 rounded-b-full bg-background/80" />
            </div>
          </div>
          <span className="text-sm font-semibold tracking-wide">思库</span>
        </div>

        {/* Step indicator: completed ✓, active pill, upcoming dot */}
        <div className="flex items-center justify-center gap-1.5 mb-6">
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
        <div key={step} className="flex flex-col items-center" style={{ animation: 'siku-onboard-in 0.28s ease' }}>
          <div className="text-[3rem] leading-none mb-4">{current.emoji}</div>
          <h2 className="text-xl font-semibold mb-3 text-text-primary">{current.title}</h2>
          <p className="text-sm text-text-secondary leading-relaxed mb-6 min-h-[60px]">{current.desc}</p>
        </div>

        {/* Primary row: step navigation */}
        <div className="flex items-center justify-center gap-2">
          {step > 0 && !isLast && (
            <button
              onClick={() => setStep((s) => s - 1)}
              className="px-4 py-2 rounded-lg border border-surface-hover text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors text-sm"
            >
              上一步
            </button>
          )}
          <button
            onClick={handlePrimary}
            className="px-6 py-2 rounded-lg bg-primary text-white text-sm font-medium hover:opacity-90 transition-opacity"
          >
            {isLast ? '开始使用' : '下一步'}
          </button>
        </div>

        {/* Secondary row: optional jump link (close via ✕ / Esc) */}
        {!isLast && current.action && (
          <div className="mt-3 flex justify-center">
            <button
              onClick={handleActionJump}
              className="px-2 py-1 text-primary text-xs underline underline-offset-4 hover:opacity-80 transition-opacity"
            >
              {current.action.label}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

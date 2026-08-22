import { useState } from 'react';
import { Bot } from 'lucide-react';
import type { AskAnswer } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';
import { agentAnswerUser } from '@/lib/tauri';

/** Dialog for the agent's AskUserQuestion tool. */
export function AskUserDialog() {
  const questions = useChatStore((s) => s.pendingQuestions);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const setPendingQuestions = useChatStore((s) => s.setPendingQuestions);
  const [answers, setAnswers] = useState<Record<number, string[]>>({});
  const [submitting, setSubmitting] = useState(false);

  if (!questions || questions.length === 0) return null;

  const toggle = (qi: number, label: string) => {
    setAnswers((prev) => {
      const cur = prev[qi] ?? [];
      const multi = questions[qi]?.multi_select;
      if (multi) {
        return {
          ...prev,
          [qi]: cur.includes(label) ? cur.filter((x) => x !== label) : [...cur, label],
        };
      }
      return { ...prev, [qi]: [label] };
    });
  };

  const handleSubmit = async () => {
    if (!activeSessionId) return;
    const allAnswered = questions.every((_, i) => (answers[i] ?? []).length > 0);
    if (!allAnswered) return;
    setSubmitting(true);
    try {
      const result: AskAnswer[] = questions.map((q, i) => ({
        question: q.question,
        answer: (answers[i] ?? []).join(', '),
      }));
      await agentAnswerUser(activeSessionId, result);
      setPendingQuestions(null);
      setAnswers({});
    } catch (err) {
      console.error('Failed to answer:', err);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-[5000] flex items-center justify-center bg-black/60"
      onClick={(e) => e.target === e.currentTarget && setPendingQuestions(null)}
    >
      <div className="w-[480px] max-w-[92vw] max-h-[80vh] overflow-y-auto rounded-2xl bg-codex-surface border border-codex-border shadow-2xl p-5">
        <div className="flex items-center gap-2 mb-4">
          <Bot size={18} className="text-codex-accent" />
          <h3 className="text-base font-semibold text-codex-primary">智能体需要确认</h3>
        </div>
        <div className="space-y-5">
          {questions.map((q, qi) => (
            <div key={qi}>
              {q.header && <div className="text-[11px] text-codex-muted mb-1">{q.header}</div>}
              <div className="text-sm text-codex-primary mb-2">{q.question}</div>
              <div className="space-y-1">
                {q.options.map((opt) => {
                  const selected = (answers[qi] ?? []).includes(opt.label);
                  return (
                    <button
                      key={opt.label}
                      onClick={() => toggle(qi, opt.label)}
                      className={`w-full text-left px-3 py-2 rounded-lg text-[13px] transition-colors ${
                        selected
                          ? 'bg-codex-accent/15 border border-codex-accent/50 text-codex-primary'
                          : 'bg-codex-bg border border-codex-border text-codex-secondary hover:bg-codex-hover'
                      }`}
                    >
                      {opt.label}
                      {opt.description && (
                        <div className="text-[11px] text-codex-muted mt-0.5">{opt.description}</div>
                      )}
                    </button>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
        <div className="flex justify-end gap-2 mt-5">
          <button
            onClick={() => setPendingQuestions(null)}
            className="px-3 py-1.5 rounded-lg border border-codex-border text-[13px] text-codex-secondary hover:bg-codex-hover"
          >
            暂不回答
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting || questions.some((_, i) => (answers[i] ?? []).length === 0)}
            className="px-4 py-1.5 rounded-lg bg-codex-accent text-black text-[13px] font-semibold hover:bg-codex-accent-hover disabled:opacity-50"
          >
            提交
          </button>
        </div>
      </div>
    </div>
  );
}

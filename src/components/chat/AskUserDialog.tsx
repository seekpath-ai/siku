import { useEffect, useState } from 'react';
import { Bot, Timer } from 'lucide-react';
import type { AskAnswer } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';
import { usePetStore } from '@/stores/petStore';
import { agentAnswerUser } from '@/lib/tauri';
import type { AskQuestion } from '@/lib/types';

/** Backend wait for AskUserQuestion answers (mirrors the approval timeout). */
const ASK_USER_TIMEOUT_SEC = 300;

/** Display-only countdown: the backend still enforces its own timeout. */
function TimeoutCountdown() {
  const [remaining, setRemaining] = useState(ASK_USER_TIMEOUT_SEC);
  useEffect(() => {
    const t = setInterval(() => setRemaining((r) => Math.max(0, r - 1)), 1000);
    return () => clearInterval(t);
  }, []);
  if (remaining <= 0) {
    return <span className="text-[11px] text-codex-danger">已超时</span>;
  }
  const mm = Math.floor(remaining / 60);
  const ss = String(remaining % 60).padStart(2, '0');
  return (
    <span className="flex items-center gap-1 text-[11px] text-codex-muted">
      <Timer size={11} />
      {mm}:{ss}
    </span>
  );
}

interface AskUserQuestionsProps {
  questions: AskQuestion[];
  /** Hand the answers back to the agent that asked. */
  onAnswer: (answers: AskAnswer[]) => Promise<void> | void;
  /** Closing without answering must still unblock the backend — otherwise it
   *  waits out the full timeout with the whole turn frozen. */
  onDismiss: () => Promise<void> | void;
}

/** Presentation for the agent's AskUserQuestion tool, shared by the chat panel
 *  and the pet panel: the two have separate stores but must behave identically,
 *  including the countdown and the "answer to unblock" rule. */
export function AskUserQuestionsDialog({ questions, onAnswer, onDismiss }: AskUserQuestionsProps) {
  const [answers, setAnswers] = useState<Record<number, string[]>>({});
  const [submitting, setSubmitting] = useState(false);

  if (questions.length === 0) return null;

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
    const allAnswered = questions.every((_, i) => (answers[i] ?? []).length > 0);
    if (!allAnswered) return;
    setSubmitting(true);
    try {
      const result: AskAnswer[] = questions.map((q, i) => ({
        question: q.question,
        answer: (answers[i] ?? []).join(', '),
      }));
      await onAnswer(result);
      setAnswers({});
    } catch (err) {
      console.error('Failed to answer:', err);
    } finally {
      setSubmitting(false);
    }
  };

  const dismiss = async () => {
    try {
      await onDismiss();
    } catch (err) {
      console.error('Failed to dismiss ask_user:', err);
    }
    setAnswers({});
  };

  return (
    <div
      className="fixed inset-0 z-[5000] flex items-center justify-center bg-black/60"
      onClick={(e) => e.target === e.currentTarget && dismiss()}
    >
      <div className="w-[480px] max-w-[92vw] max-h-[80vh] overflow-y-auto rounded-2xl bg-codex-surface border border-codex-border shadow-2xl p-5">
        <div className="flex items-center gap-2 mb-4">
          <Bot size={18} className="text-codex-accent" />
          <h3 className="text-base font-semibold text-codex-primary">智能体需要确认</h3>
          <span className="ml-auto">
            <TimeoutCountdown />
          </span>
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
            onClick={dismiss}
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

/** Chat panel: answers go to the active chat session. */
export function AskUserDialog() {
  const questions = useChatStore((s) => s.pendingQuestions);
  const questionsSessionId = useChatStore((s) => s.pendingQuestionsSessionId);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const setPendingQuestions = useChatStore((s) => s.setPendingQuestions);

  // Only render for the session the questions were raised in; they survive
  // session switches and re-appear when switching back.
  if (!questions || questions.length === 0 || questionsSessionId !== activeSessionId) return null;

  return (
    <AskUserQuestionsDialog
      questions={questions}
      onAnswer={async (answers) => {
        if (!activeSessionId) return;
        await agentAnswerUser(activeSessionId, answers);
        setPendingQuestions(null);
      }}
      onDismiss={async () => {
        if (activeSessionId) {
          await agentAnswerUser(
            activeSessionId,
            questions.map((q) => ({ question: q.question, answer: '用户暂不回答' }))
          );
        }
        setPendingQuestions(null);
      }}
    />
  );
}

/** Pet panel: the agent asks here too, and the answer has to reach the pet
 *  session — without this the question was never shown and the turn froze for
 *  the whole backend timeout. */
export function PetAskUserDialog() {
  const questions = usePetStore((s) => s.pendingQuestions);
  const sessionId = usePetStore((s) => s.session?.id ?? null);
  const setPendingQuestions = usePetStore((s) => s.setPendingQuestions);

  if (!questions || questions.length === 0 || !sessionId) return null;

  return (
    <AskUserQuestionsDialog
      questions={questions}
      onAnswer={async (answers) => {
        await agentAnswerUser(sessionId, answers);
        setPendingQuestions(null);
      }}
      onDismiss={async () => {
        await agentAnswerUser(
          sessionId,
          questions.map((q) => ({ question: q.question, answer: '用户暂不回答' }))
        );
        setPendingQuestions(null);
      }}
    />
  );
}

import { useEffect, useState } from 'react';
import { Bot, Timer, MessageSquarePlus } from 'lucide-react';
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
  /** Hand the answers — plus the user's free-form message, if any — back to the
   *  agent that asked. */
  onAnswer: (answers: AskAnswer[], note: string) => Promise<void> | void;
  /** Closing without answering must still unblock the backend — otherwise it
   *  waits out the full timeout with the whole turn frozen. */
  onDismiss: () => Promise<void> | void;
}

/** Presentation for the agent's AskUserQuestion tool, shared by the chat panel
 *  and the pet panel: the two have separate stores but must behave identically,
 *  including the countdown and the "answer to unblock" rule.
 *
 *  The agent's options are a menu, never a fence: every question also accepts a
 *  typed answer, and the dialog carries one free-form message for the agent.
 *  That message is the escape hatch — it lets the user redirect the agent
 *  ("别问了，直接改成周报格式") without first satisfying every question. */
export function AskUserQuestionsDialog({ questions, onAnswer, onDismiss }: AskUserQuestionsProps) {
  const [answers, setAnswers] = useState<Record<number, string[]>>({});
  /** Per-question text the user typed instead of picking an option. */
  const [customs, setCustoms] = useState<Record<number, string>>({});
  /** Free-form message for the agent, tied to no particular question. */
  const [note, setNote] = useState('');
  const [submitting, setSubmitting] = useState(false);

  if (questions.length === 0) return null;

  const customText = (qi: number) => (customs[qi] ?? '').trim();
  /** The answer that will be sent for one question; '' = unanswered. */
  const answerText = (qi: number) => customText(qi) || (answers[qi] ?? []).join(', ');
  const answeredCount = questions.filter((_, i) => answerText(i) !== '').length;
  const allAnswered = answeredCount === questions.length;
  // Typing a message is itself a valid reply: the user may override the whole
  // dialog instead of answering it point by point.
  const canSubmit = allAnswered || note.trim() !== '';

  const toggle = (qi: number, label: string) => {
    // Picking an option drops any typed answer for that question: the two are
    // mutually exclusive, so the model never receives a contradictory pair.
    setCustoms((prev) => ({ ...prev, [qi]: '' }));
    setAnswers((prev) => {
      const cur = prev[qi] ?? [];
      if (questions[qi]?.multi_select) {
        return {
          ...prev,
          [qi]: cur.includes(label) ? cur.filter((x) => x !== label) : [...cur, label],
        };
      }
      return { ...prev, [qi]: [label] };
    });
  };

  const typeCustom = (qi: number, text: string) => {
    setCustoms((prev) => ({ ...prev, [qi]: text }));
    if (text.trim() !== '') {
      setAnswers((prev) => ({ ...prev, [qi]: [] }));
    }
  };

  const reset = () => {
    setAnswers({});
    setCustoms({});
    setNote('');
  };

  const handleSubmit = async () => {
    if (!canSubmit || submitting) return;
    setSubmitting(true);
    try {
      const result: AskAnswer[] = questions.map((q, i) => {
        const typed = customText(i);
        return {
          question: q.question,
          answer: answerText(i),
          // Typed answers are labelled for the model so it cannot read them as
          // one of its own options.
          custom: typed !== '',
        };
      });
      await onAnswer(result, note.trim());
      reset();
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
    reset();
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
                  const selected =
                    (answers[qi] ?? []).includes(opt.label) && customText(qi) === '';
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
              <input
                value={customs[qi] ?? ''}
                onChange={(e) => typeCustom(qi, e.target.value)}
                placeholder="其它（也可以自己输入）"
                spellCheck={false}
                className={`mt-1.5 w-full rounded-lg px-3 py-2 text-[13px] outline-none transition-colors ${
                  customText(qi) !== ''
                    ? 'bg-codex-accent/15 border border-codex-accent/50 text-codex-primary placeholder:text-codex-muted'
                    : 'bg-codex-bg border border-codex-border text-codex-primary placeholder:text-codex-secondary/50 focus:border-codex-border-light'
                }`}
              />
            </div>
          ))}
        </div>

        {/* Free-form message: an answer that outranks the questions above. */}
        <div className="mt-5 pt-4 border-t border-codex-border">
          <div className="flex items-center gap-1.5 text-[11px] text-codex-muted mb-1.5">
            <MessageSquarePlus size={12} />
            想对智能体说的话（可选）
          </div>
          <textarea
            value={note}
            onChange={(e) => setNote(e.target.value)}
            rows={2}
            spellCheck={false}
            placeholder="补充说明、纠正方向，或直接说「别问了，按 X 做」——填了它就可以不逐题作答"
            className="w-full resize-y rounded-lg bg-codex-bg border border-codex-border px-3 py-2 text-[13px] text-codex-primary outline-none focus:border-codex-border-light placeholder:text-codex-secondary/50"
          />
        </div>

        <div className="flex items-center justify-end gap-2 mt-4">
          {!canSubmit && (
            <span className="mr-auto text-[11px] text-codex-muted">
              每题选择或自行输入，或直接在下方留言
            </span>
          )}
          {canSubmit && !allAnswered && (
            <span className="mr-auto text-[11px] text-codex-muted">
              已答 {answeredCount}/{questions.length} 题，将连同留言一起发送
            </span>
          )}
          <button
            onClick={dismiss}
            className="px-3 py-1.5 rounded-lg border border-codex-border text-[13px] text-codex-secondary hover:bg-codex-hover"
          >
            暂不回答
          </button>
          <button
            onClick={handleSubmit}
            disabled={submitting || !canSubmit}
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
      onAnswer={async (answers, note) => {
        // Clearing local state is unconditional: when the backend has already
        // given up (timeout), agentAnswerUser fails against the dead channel,
        // and the dialog must still close.
        try {
          if (activeSessionId) {
            await agentAnswerUser(activeSessionId, answers, note);
          }
        } finally {
          setPendingQuestions(null);
        }
      }}
      onDismiss={async () => {
        try {
          if (activeSessionId) {
            await agentAnswerUser(
              activeSessionId,
              questions.map((q) => ({ question: q.question, answer: '用户暂不回答' }))
            );
          }
        } finally {
          setPendingQuestions(null);
        }
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
      onAnswer={async (answers, note) => {
        try {
          await agentAnswerUser(sessionId, answers, note);
        } finally {
          setPendingQuestions(null);
        }
      }}
      onDismiss={async () => {
        try {
          await agentAnswerUser(
            sessionId,
            questions.map((q) => ({ question: q.question, answer: '用户暂不回答' }))
          );
        } finally {
          setPendingQuestions(null);
        }
      }}
    />
  );
}

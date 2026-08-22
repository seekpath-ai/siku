import { create } from 'zustand';

/**
 * Per-snippet in-flight translation state. A key's presence in `texts` means a
 * translation stream is running for that snippet id — batch "translate all",
 * the single per-card translate button, or the reader's "translate selection"
 * flow. `SnippetCard` renders the accumulated text (or a loading state) from
 * this store, and any producer can begin/append/finish/fail a stream.
 */
interface TranslationStreamState {
  /** snippet id -> text streamed so far (empty string = started, no deltas yet) */
  texts: Record<string, string>;
  /** snippet id -> error message for a failed translation */
  errors: Record<string, string>;
  /** Start a stream: marks the snippet as translating with empty text. */
  begin: (id: string) => void;
  /** Append a delta to an in-flight stream. */
  append: (id: string, delta: string) => void;
  /** Finish a stream successfully: clears streaming and error state. */
  finish: (id: string) => void;
  /** Fail a stream: clears streaming state and records an error message. */
  fail: (id: string, message: string) => void;
}

export const useTranslationStreamStore = create<TranslationStreamState>((set) => ({
  texts: {},
  errors: {},
  begin: (id) =>
    set((s) => {
      if (s.texts[id] !== undefined && s.errors[id] === undefined) return s;
      const texts = { ...s.texts, [id]: '' };
      const errors = { ...s.errors };
      delete errors[id];
      return { texts, errors };
    }),
  append: (id, delta) =>
    set((s) =>
      s.texts[id] === undefined ? s : { texts: { ...s.texts, [id]: s.texts[id] + delta } }
    ),
  finish: (id) =>
    set((s) => {
      if (s.texts[id] === undefined && s.errors[id] === undefined) return s;
      const texts = { ...s.texts };
      delete texts[id];
      const errors = { ...s.errors };
      delete errors[id];
      return { texts, errors };
    }),
  fail: (id, message) =>
    set((s) => {
      if (s.texts[id] === undefined && s.errors[id] === message) return s;
      const texts = { ...s.texts };
      delete texts[id];
      return { texts, errors: { ...s.errors, [id]: message } };
    }),
}));

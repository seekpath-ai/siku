import { create } from 'zustand';
import { persist } from 'zustand/middleware';

export type TargetLang = 'zh' | 'en' | 'ja';

interface TranslationState {
  targetLang: TargetLang;
  setTargetLang: (lang: TargetLang) => void;
}

const STORAGE_KEY = 'siku.translation';

export const useTranslationStore = create<TranslationState>()(
  persist(
    (set) => ({
      targetLang: 'zh',
      setTargetLang: (targetLang) => set({ targetLang }),
    }),
    { name: STORAGE_KEY }
  )
);

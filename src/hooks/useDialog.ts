import { useCallback } from 'react';
import { useDialogStore } from '@/stores/dialogStore';

export function useDialog() {
  const alert = useDialogStore(useCallback((state) => state.alert, []));
  const confirm = useDialogStore(useCallback((state) => state.confirm, []));
  const prompt = useDialogStore(useCallback((state) => state.prompt, []));
  const select = useDialogStore(useCallback((state) => state.select, []));

  return { alert, confirm, prompt, select };
}

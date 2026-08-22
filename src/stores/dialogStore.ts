import { create } from 'zustand';

export type DialogType = 'alert' | 'confirm' | 'prompt' | 'select';

export interface SelectOption {
  label: string;
  value: string;
  /** Tree nesting depth (0 = root level). */
  indent?: number;
  /** Value of the parent option; used to build tree hierarchies. */
  parent?: string | null;
  /** Whether the node has children (shows an expand/collapse arrow). */
  expandable?: boolean;
}

interface PromptOptions {
  defaultValue?: string;
  placeholder?: string;
  /** Render a multi-line textarea instead of a single-line input. */
  multiline?: boolean;
}

interface SelectOptions {
  options: SelectOption[];
}

interface DialogState {
  open: boolean;
  type: DialogType | null;
  title: string;
  message: string;
  promptOptions?: PromptOptions;
  selectOptions?: SelectOptions;
  resolve: ((value: unknown) => void) | null;
}

interface DialogActions {
  alert: (message: string, title?: string) => Promise<void>;
  confirm: (message: string, title?: string) => Promise<boolean>;
  prompt: (message: string, options?: PromptOptions & { title?: string }) => Promise<string | null>;
  select: (message: string, options: SelectOptions & { title?: string }) => Promise<string | null>;
  close: (value?: string | boolean | null) => void;
}

const initialState: Omit<DialogState, 'resolve'> & { resolve: DialogState['resolve'] } = {
  open: false,
  type: null,
  title: '',
  message: '',
  promptOptions: undefined,
  selectOptions: undefined,
  resolve: null,
};

export const useDialogStore = create<DialogState & DialogActions>((set, get) => ({
  ...initialState,

  alert: (message, title = '提示') =>
    new Promise((resolve) => {
      set({
        open: true,
        type: 'alert',
        title,
        message,
        promptOptions: undefined,
        resolve: () => {
          resolve();
          set({ open: false });
        },
      });
    }),

  confirm: (message, title = '确认') =>
    new Promise((resolve) => {
      set({
        open: true,
        type: 'confirm',
        title,
        message,
        promptOptions: undefined,
        resolve: (value) => {
          resolve(!!value);
          set({ open: false });
        },
      });
    }),

  prompt: (message, options = {}) =>
    new Promise((resolve) => {
      const { title = '输入', ...promptOptions } = options;
      set({
        open: true,
        type: 'prompt',
        title,
        message,
        promptOptions,
        resolve: (value) => {
          resolve(value as string | null);
          set({ open: false });
        },
      });
    }),

  select: (message, options) =>
    new Promise((resolve) => {
      const { title = '选择', ...selectOptions } = options;
      set({
        open: true,
        type: 'select',
        title,
        message,
        selectOptions,
        resolve: (value) => {
          resolve(value as string | null);
          set({ open: false });
        },
      });
    }),

  close: (value = null) => {
    const { resolve } = get();
    resolve?.(value);
  },
}));

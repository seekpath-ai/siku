import { create } from 'zustand';

export type DialogType = 'alert' | 'confirm' | 'prompt' | 'select';

/** alert 的语义类型：决定图标与配色（与 notify 的 ToastKind 同一套语义）。 */
export type AlertKind = 'success' | 'error' | 'warning' | 'info';

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

/** alert 的第二个参数：字符串等价于 { title }（旧调用形态）。 */
export type AlertOptions = string | { title?: string; kind?: AlertKind };

/** confirm 的第二个参数：字符串等价于 { title }。danger 用于删除/清空等
 * 破坏性操作——确认按钮用红色，与普通确认区分。 */
export type ConfirmOptions = string | { title?: string; danger?: boolean };

interface DialogState {
  open: boolean;
  type: DialogType | null;
  title: string;
  message: string;
  alertKind: AlertKind;
  confirmDanger: boolean;
  promptOptions?: PromptOptions;
  selectOptions?: SelectOptions;
  resolve: ((value: unknown) => void) | null;
}

interface DialogActions {
  alert: (message: string, options?: AlertOptions) => Promise<void>;
  confirm: (message: string, options?: ConfirmOptions) => Promise<boolean>;
  prompt: (message: string, options?: PromptOptions & { title?: string }) => Promise<string | null>;
  select: (message: string, options: SelectOptions & { title?: string }) => Promise<string | null>;
  close: (value?: string | boolean | null) => void;
}

const initialState: Omit<DialogState, 'resolve'> & { resolve: DialogState['resolve'] } = {
  open: false,
  type: null,
  title: '',
  message: '',
  alertKind: 'info',
  confirmDanger: false,
  promptOptions: undefined,
  selectOptions: undefined,
  resolve: null,
};

export const useDialogStore = create<DialogState & DialogActions>((set, get) => ({
  ...initialState,

  alert: (message, options = {}) =>
    new Promise((resolve) => {
      const { title = '提示', kind = 'info' } =
        typeof options === 'string' ? { title: options } : options;
      set({
        open: true,
        type: 'alert',
        title,
        message,
        alertKind: kind,
        promptOptions: undefined,
        selectOptions: undefined,
        resolve: () => {
          resolve();
          set({ open: false });
        },
      });
    }),

  confirm: (message, options = {}) =>
    new Promise((resolve) => {
      const { title = '确认', danger = false } =
        typeof options === 'string' ? { title: options } : options;
      set({
        open: true,
        type: 'confirm',
        title,
        message,
        confirmDanger: danger,
        promptOptions: undefined,
        selectOptions: undefined,
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
        selectOptions: undefined,
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
        promptOptions: undefined,
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

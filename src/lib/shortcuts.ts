/**
 * 全局快捷键注册表。
 *
 * 此前快捷键散落在各组件的 onKeyDown/useEffect 里，没有总览也无法冲突检测。
 * 本模块提供声明式注册：id、按键、描述、是否允许输入框中触发、handler。
 * AppShell 的全局快捷键已迁移到这里；「?」打开的快捷键帮助面板直接读注册表。
 *
 * 作用域组件（对话框内 Esc、编辑器内按键）不经过这里——它们的生命周期跟随组件。
 */

export interface ShortcutDef {
  /** 唯一 id，如 'nav.library'。重复注册会抛错（冲突检测）。 */
  id: string;
  /** 按键描述，如 'Ctrl+1' / 'Ctrl+Shift+N'。解析见 matches()。 */
  keys: string;
  /** 帮助面板里显示的中文描述。 */
  description: string;
  /** 输入框/文本域聚焦时是否仍触发（导航类通常为 true）。 */
  allowInInput?: boolean;
  /** 需要 Shift 之外的精确修饰键匹配。 */
  handler: (e: KeyboardEvent) => void;
}

interface ParsedKey {
  key: string;
  ctrl: boolean;
  shift: boolean;
  alt: boolean;
}

function parse(keys: string): ParsedKey {
  const parts = keys.toLowerCase().split('+');
  const key = parts[parts.length - 1];
  return {
    key,
    ctrl: parts.includes('ctrl'),
    shift: parts.includes('shift'),
    alt: parts.includes('alt'),
  };
}

const registry = new Map<string, ShortcutDef & ParsedKey>();

export function registerShortcut(def: ShortcutDef): () => void {
  if (registry.has(def.id)) throw new Error(`duplicate shortcut id: ${def.id}`);
  const entry = { ...def, ...parse(def.keys) };
  registry.set(def.id, entry);
  return () => registry.delete(def.id);
}

export function listShortcuts(): ShortcutDef[] {
  return [...registry.values()];
}

function matches(e: KeyboardEvent, s: ParsedKey): boolean {
  const ctrl = e.ctrlKey || e.metaKey; // macOS Cmd 视同 Ctrl
  return (
    e.key.toLowerCase() === s.key &&
    ctrl === s.ctrl &&
    e.shiftKey === s.shift &&
    e.altKey === s.alt
  );
}

function isTypingTarget(target: EventTarget | null): boolean {
  const el = target as HTMLElement | null;
  return !!el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.isContentEditable);
}

let installed = false;

/** 安装全局监听（App 挂载一次）。重复调用安全。 */
export function installShortcutListener(): void {
  if (installed) return;
  installed = true;
  window.addEventListener('keydown', (e) => {
    for (const s of registry.values()) {
      if (!matches(e, s)) continue;
      if (!s.allowInInput && isTypingTarget(e.target)) continue;
      e.preventDefault();
      s.handler(e);
      return;
    }
  });
}

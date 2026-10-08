import { useEffect, useState } from 'react';
import {
  X, Info, Palette, Layout, NotebookPen, KeyRound, Blocks,
  GitMerge, Link2, Zap, Command, Tag, CalendarDays, LayoutTemplate, History,
  Plug, Minus, Plus,
} from 'lucide-react';
import { useNotesSettingsStore, FONT_SIZE_MIN, FONT_SIZE_MAX, type NotesDefaultMode } from '@/stores/notesSettingsStore';

interface Props {
  onClose: () => void;
}

interface OptionItem {
  key: string;
  label: string;
  icon: React.ReactNode;
  desc: string;
}

interface PluginItem {
  key: string;
  label: string;
  icon: React.ReactNode;
  desc: string;
}

const OPTION_ITEMS: OptionItem[] = [
  { key: 'about', label: '关于', icon: <Info size={13} />, desc: '查看应用版本与信息' },
  { key: 'appearance', label: '外观', icon: <Palette size={13} />, desc: '主题、字体与界面显示' },
  { key: 'interface', label: '界面', icon: <Layout size={13} />, desc: '界面行为与显示选项' },
  { key: 'editor', label: '编辑器', icon: <NotebookPen size={13} />, desc: '编辑与显示行为' },
  { key: 'keychain', label: '钥匙串', icon: <KeyRound size={13} />, desc: '凭据与安全设置' },
];

const CORE_PLUGINS: PluginItem[] = [
  { key: 'canvas', label: '白板', icon: <Blocks size={13} />, desc: '在无限画布上自由组织笔记与卡片' },
  { key: 'reorganize', label: '笔记重组', icon: <GitMerge size={13} />, desc: '快速重组笔记的结构与关联' },
  { key: 'backlinks', label: '反向链接', icon: <Link2 size={13} />, desc: '显示链接到当前笔记的其他笔记' },
  { key: 'quick-switcher', label: '快速切换', icon: <Zap size={13} />, desc: '通过搜索快速跳转到任何笔记' },
  { key: 'command-palette', label: '命令面板', icon: <Command size={13} />, desc: '通过命令面板执行任何命令' },
  { key: 'tag-pane', label: '标签面板', icon: <Tag size={13} />, desc: '以面板形式浏览所有标签' },
  { key: 'daily-notes', label: '日记', icon: <CalendarDays size={13} />, desc: '创建并导航每日笔记' },
  { key: 'templates', label: '模板', icon: <LayoutTemplate size={13} />, desc: '从模板快速创建笔记' },
  { key: 'file-recovery', label: '文件恢复', icon: <History size={13} />, desc: '恢复意外丢失的内容' },
];

/** Small iOS-style toggle used by the editor settings rows. */
function Toggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      onClick={() => onChange(!checked)}
      className={`relative w-8 h-[18px] rounded-full transition-colors shrink-0 ${
        checked ? 'bg-primary' : 'bg-surface-hover'
      }`}
    >
      <span
        className={`absolute top-[2px] w-[14px] h-[14px] rounded-full bg-white transition-all ${
          checked ? 'left-[16px]' : 'left-[2px]'
        }`}
      />
    </button>
  );
}

function SettingRow({ label, desc, children }: { label: string; desc: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-6 py-3 border-b border-surface-hover/60 last:border-0">
      <div className="min-w-0">
        <div className="text-[13px] text-text-primary">{label}</div>
        <div className="text-[11px] text-text-secondary/70 mt-0.5 leading-relaxed">{desc}</div>
      </div>
      <div className="shrink-0 flex items-center">{children}</div>
    </div>
  );
}

const DEFAULT_MODE_OPTIONS: { value: NotesDefaultMode; label: string }[] = [
  { value: 'edit', label: '编辑' },
  { value: 'source', label: '源码' },
  { value: 'reading', label: '阅读' },
];

/** 「编辑器」分区：默认打开模式 / 代码块折行 / 严格换行 / 字号。 */
function EditorSettings() {
  const { defaultMode, codeBlockWrap, strictLineBreaks, editorFontSize, set } = useNotesSettingsStore();
  return (
    <div>
      <h2 className="text-sm font-semibold text-text-primary mb-1">编辑器</h2>
      <p className="text-xs text-text-secondary/70 mb-2">编辑与显示行为（仅本设备，不随同步）。</p>
      <SettingRow
        label="默认打开模式"
        desc="打开已有笔记时的默认视图；手动切换过的笔记仍记住自己的选择。新建笔记始终以编辑模式打开。"
      >
        <div className="flex rounded-md border border-surface-hover overflow-hidden">
          {DEFAULT_MODE_OPTIONS.map((o) => (
            <button
              key={o.value}
              onClick={() => set({ defaultMode: o.value })}
              className={`px-2.5 py-1 text-[12px] transition-colors ${
                defaultMode === o.value
                  ? 'bg-primary/15 text-primary'
                  : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover'
              }`}
            >
              {o.label}
            </button>
          ))}
        </div>
      </SettingRow>
      <SettingRow
        label="代码块自动折行"
        desc="阅读视图与对话气泡中的代码块默认折行；每个代码块上的折行按钮仍可单独覆盖。"
      >
        <Toggle checked={codeBlockWrap} onChange={(v) => set({ codeBlockWrap: v })} />
      </SettingRow>
      <SettingRow
        label="严格换行"
        desc="开启后阅读视图中单个回车即换行（Obsidian 风格）；关闭后遵循标准 Markdown，单回车合并为空格。"
      >
        <Toggle checked={strictLineBreaks} onChange={(v) => set({ strictLineBreaks: v })} />
      </SettingRow>
      <SettingRow label="编辑器字号" desc="编辑视图与阅读视图正文的字号（12–24px）。">
        <div className="flex items-center gap-1.5">
          <button
            onClick={() => set({ editorFontSize: editorFontSize - 1 })}
            disabled={editorFontSize <= FONT_SIZE_MIN}
            className="p-1 rounded border border-surface-hover text-text-secondary hover:text-text-primary disabled:opacity-30"
            aria-label="减小字号"
          >
            <Minus size={12} />
          </button>
          <span className="w-10 text-center text-[12px] text-text-primary tabular-nums">{editorFontSize}px</span>
          <button
            onClick={() => set({ editorFontSize: editorFontSize + 1 })}
            disabled={editorFontSize >= FONT_SIZE_MAX}
            className="p-1 rounded border border-surface-hover text-text-secondary hover:text-text-primary disabled:opacity-30"
            aria-label="增大字号"
          >
            <Plus size={12} />
          </button>
        </div>
      </SettingRow>
    </div>
  );
}

/** Obsidian-style settings modal for the notes page (stubbed, two columns). */
export function NotesSettingsModal({ onClose }: Props) {
  const [section, setSection] = useState<'options' | 'core' | 'community'>('options');
  const [selectedKey, setSelectedKey] = useState('about');

  useEffect(() => {
    const onDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onDown);
    return () => window.removeEventListener('keydown', onDown);
  }, [onClose]);

  const selected =
    section === 'options'
      ? OPTION_ITEMS.find((i) => i.key === selectedKey) ?? OPTION_ITEMS[0]
      : section === 'core'
        ? CORE_PLUGINS.find((i) => i.key === selectedKey) ?? CORE_PLUGINS[0]
        : null;

  const selectIn = (s: typeof section, key: string) => {
    setSection(s);
    setSelectedKey(key);
  };

  const navBtn = (active: boolean) =>
    `w-full flex items-center gap-1.5 px-2.5 py-1.5 rounded text-[12px] text-left transition-colors ${
      active ? 'bg-surface-hover text-text-primary' : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover/50'
    }`;

  return (
    <div className="fixed inset-0 z-[200] flex items-center justify-center">
      <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />
      <div className="relative w-[720px] max-w-[92vw] h-[520px] max-h-[82vh] flex flex-col bg-surface border border-surface-hover rounded-xl shadow-2xl overflow-hidden">
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-2.5 border-b border-surface-hover shrink-0">
          <span className="text-sm font-medium text-text-primary">设置</span>
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
            aria-label="关闭设置"
          >
            <X size={14} />
          </button>
        </div>

        <div className="flex flex-1 min-h-0">
          {/* Left nav */}
          <div className="w-[220px] shrink-0 border-r border-surface-hover p-2 overflow-y-auto flex flex-col gap-3">
            <div>
              <div className="px-2.5 pb-1 text-[10px] uppercase tracking-wider text-text-secondary/50">选项</div>
              {OPTION_ITEMS.map((item) => (
                <button
                  key={item.key}
                  onClick={() => selectIn('options', item.key)}
                  className={navBtn(section === 'options' && selectedKey === item.key)}
                >
                  {item.icon}
                  {item.label}
                </button>
              ))}
            </div>

            <div>
              <div className="px-2.5 pb-1 text-[10px] uppercase tracking-wider text-text-secondary/50">核心插件</div>
              {CORE_PLUGINS.map((item) => (
                <button
                  key={item.key}
                  onClick={() => selectIn('core', item.key)}
                  className={navBtn(section === 'core' && selectedKey === item.key)}
                >
                  {item.icon}
                  {item.label}
                </button>
              ))}
            </div>

            <div>
              <div className="px-2.5 pb-1 text-[10px] uppercase tracking-wider text-text-secondary/50">第三方插件</div>
              <button
                onClick={() => {
                  setSection('community');
                  setSelectedKey('community');
                }}
                className={navBtn(section === 'community')}
              >
                <Plug size={13} />
                社区插件
              </button>
            </div>
          </div>

          {/* Right content */}
          <div className="flex-1 min-w-0 overflow-y-auto p-5">
            {section === 'community' ? (
              <div>
                <h2 className="text-sm font-semibold text-text-primary mb-1">社区插件</h2>
                <p className="text-xs text-text-secondary/70 mb-4">浏览、安装和管理社区插件。</p>
                <div className="rounded-lg border border-dashed border-surface-hover p-6 text-center text-xs text-text-secondary/50">
                  尚未安装任何社区插件<br />插件市场即将推出
                </div>
              </div>
            ) : selectedKey === 'editor' && section === 'options' ? (
              <EditorSettings />
            ) : selected ? (
              <div>
                <h2 className="text-sm font-semibold text-text-primary mb-1">{selected.label}</h2>
                <p className="text-xs text-text-secondary/70 mb-4">{selected.desc}</p>
                <div className="rounded-lg border border-dashed border-surface-hover p-6 text-center text-xs text-text-secondary/50">
                  「{selected.label}」设置尚未实现，敬请期待
                </div>
              </div>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
}

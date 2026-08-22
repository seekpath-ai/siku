import { useEffect, useState } from 'react';
import {
  X, Info, Palette, Layout, NotebookPen, KeyRound, Blocks,
  GitMerge, Link2, Zap, Command, List, Tag, CalendarDays, LayoutTemplate, History,
  Plug,
} from 'lucide-react';

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
  { key: 'outline', label: '大纲', icon: <List size={13} />, desc: '显示当前笔记的标题大纲' },
  { key: 'tag-pane', label: '标签面板', icon: <Tag size={13} />, desc: '以面板形式浏览所有标签' },
  { key: 'daily-notes', label: '日记', icon: <CalendarDays size={13} />, desc: '创建并导航每日笔记' },
  { key: 'templates', label: '模板', icon: <LayoutTemplate size={13} />, desc: '从模板快速创建笔记' },
  { key: 'file-recovery', label: '文件恢复', icon: <History size={13} />, desc: '恢复意外丢失的内容' },
];

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

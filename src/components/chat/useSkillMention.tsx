import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Loader2, Plus, X } from 'lucide-react';
import { agentSetSessionSkills, skillsList, type SkillInfo } from '@/lib/tauri';
import { useDialog } from '@/hooks/useDialog';
import { SkillIcon } from './SkillIcon';

/**
 * 技能 @ 提及：输入框键入 @（或 /）弹出技能面板，选中即挂载到当前会话并
 * 以 chip 呈现（Kimi 风格，不往文本里插 @文字）；工具栏「+」按钮是同一
 * 面板的鼠标入口。chip 列表 = 会话已挂载技能：点击 chip 指定下一条消息
 * 优先使用（focus），× 为从会话卸载。发送方用 `takeFocusHint()` 取一次性
 * 提示文本注入用户消息。
 *
 * 用法：
 *   const m = useSkillMention({ input, setInput, textareaRef, sessionId, ... });
 *   <textarea onChange={(e) => { setInput(...); m.handleChange(e); }}
 *             onKeyDown={(e) => { if (m.handleKeyDown(e)) return; ... }} />
 *   {m.chips}    // 输入框上方（附件 chip 区域旁）
 *   {m.button}   // 工具栏
 *   {m.popover}  // 输入框容器内（容器需 relative）
 */

interface Options {
  input: string;
  setInput: (v: string) => void;
  textareaRef: React.RefObject<HTMLTextAreaElement | null>;
  /** null = 尚无会话（按钮与键入触发均禁用）。 */
  sessionId: string | null;
  selectedSkills: string[];
  onSkillsChange: (next: string[]) => void;
  disabled?: boolean;
  /** 挂载失败的上报通道；缺省用全局 alert（宠物独立窗口里全局 Dialog
   *  不渲染，调用方应传入自己的 toast）。 */
  onError?: (msg: string) => void | Promise<void>;
}

interface Segment {
  /** '@' 或 '/' 在文本中的下标。 */
  start: number;
  /** 光标位置（查询串右端）。 */
  end: number;
  query: string;
}

/** 光标前的文本中，最后一个触发符（@ 或 /）构成的活跃查询段。
 *  触发符须在行首或空白之后（排除 a@b.com 这类邮箱/路径误触发）。 */
function detectSegment(text: string, caret: number): Segment | null {
  const before = text.slice(0, caret);
  const m = /(^|[\s\n])[@/]([^@/\s]*)$/.exec(before);
  if (!m) return null;
  const triggerAt = before.length - m[2].length - 1;
  return { start: triggerAt, end: caret, query: m[2] };
}

export function useSkillMention({
  input,
  setInput,
  textareaRef,
  sessionId,
  selectedSkills,
  onSkillsChange,
  disabled,
  onError,
}: Options) {
  const { alert } = useDialog();
  const reportError = useCallback(
    (msg: string) => (onError ? onError(msg) : alert(msg, { title: '技能', kind: 'error' })),
    [onError, alert],
  );
  const [open, setOpen] = useState(false);
  const [segment, setSegment] = useState<Segment | null>(null);
  const [query, setQuery] = useState('');
  const [skills, setSkills] = useState<SkillInfo[] | null>(null);
  const [active, setActive] = useState(0);
  const [mounting, setMounting] = useState(false);
  /** 本次 @ 激活（或点击 chip 指定）的技能：下一条消息注入一次性提示。 */
  const [focus, setFocus] = useState<string | null>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const ensureSkills = useCallback(async () => {
    if (skills !== null) return;
    try {
      setSkills(await skillsList());
    } catch {
      setSkills([]);
    }
  }, [skills]);

  const close = useCallback(() => {
    setOpen(false);
    setSegment(null);
    setQuery('');
    setActive(0);
  }, []);

  const openFromButton = useCallback(() => {
    if (disabled || !sessionId) return;
    void ensureSkills();
    setSegment(null);
    setQuery('');
    setActive(0);
    setOpen((v) => !v);
  }, [disabled, sessionId, ensureSkills]);

  /** 包在 textarea onChange 里（setInput 之后调用）。 */
  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLTextAreaElement>) => {
      if (disabled || !sessionId) return;
      const caret = e.target.selectionStart ?? e.target.value.length;
      const seg = detectSegment(e.target.value, caret);
      if (seg) {
        void ensureSkills();
        setSegment(seg);
        setQuery(seg.query);
        setActive(0);
        setOpen(true);
      } else if (open && segment) {
        // 只有键入触发的面板才随文本失配关闭；按钮打开的不受文本影响。
        close();
      }
    },
    [disabled, sessionId, ensureSkills, open, segment, close],
  );

  const flat = useMemo(() => {
    const filtered = (skills ?? []).filter(
      (s) =>
        !query ||
        s.name.toLowerCase().includes(query.toLowerCase()) ||
        s.description.toLowerCase().includes(query.toLowerCase()),
    );
    const builtin = filtered.filter((s) => s.source === 'builtin');
    const userSkills = filtered.filter((s) => s.source !== 'builtin');
    return { builtin, user: userSkills, flat: [...builtin, ...userSkills] };
  }, [skills, query]);
  const { builtin, user } = flat;
  const flatList = flat.flat;

  const pick = useCallback(
    async (skill: SkillInfo) => {
      if (!sessionId || mounting) return;
      const name = skill.name;
      try {
        if (!selectedSkills.includes(name)) {
          setMounting(true);
          const next = [...selectedSkills, name];
          await agentSetSessionSkills(sessionId, next);
          onSkillsChange(next);
        }
      } catch (err) {
        setMounting(false);
        await reportError(`挂载技能失败: ${err}`);
        return;
      }
      setMounting(false);
      // 键入触发的面板：清掉文本里的 @query 段；chip 承载“已选用”的表达。
      if (segment) {
        setInput(input.slice(0, segment.start) + input.slice(segment.end));
      }
      setFocus(name);
      close();
      setTimeout(() => textareaRef.current?.focus(), 0);
    },
    [sessionId, mounting, selectedSkills, onSkillsChange, reportError, segment, input, setInput, close, textareaRef],
  );

  /** chip 的 ×：从会话卸载（与设置页取消勾选等价）。 */
  const unmount = useCallback(
    async (name: string) => {
      if (!sessionId) return;
      const next = selectedSkills.filter((n) => n !== name);
      try {
        await agentSetSessionSkills(sessionId, next);
        onSkillsChange(next);
        setFocus((f) => (f === name ? null : f));
      } catch (err) {
        await reportError(`卸载技能失败: ${err}`);
      }
    },
    [sessionId, selectedSkills, onSkillsChange, reportError],
  );

  /** 发送时调用：取出一次性“指定使用”提示并清除（无则 null）。 */
  const takeFocusHint = useCallback((): string | null => {
    if (!focus) return null;
    setFocus(null);
    return `（指定使用技能：${focus}）`;
  }, [focus]);

  /** 包在 textarea onKeyDown 最前面；返回 true 表示事件已被面板消费。 */
  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLTextAreaElement>): boolean => {
      if (!open) return false;
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setActive((a) => (flatList.length ? (a + 1) % flatList.length : 0));
        return true;
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault();
        setActive((a) => (flatList.length ? (a - 1 + flatList.length) % flatList.length : 0));
        return true;
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        if (flatList[active]) {
          e.preventDefault();
          void pick(flatList[active]);
        }
        return true;
      }
      if (e.key === 'Escape') {
        e.preventDefault();
        close();
        return true;
      }
      return false;
    },
    [open, flatList, active, pick, close],
  );

  // 选中项滚动到可见区域。
  useEffect(() => {
    if (!open) return;
    listRef.current
      ?.querySelector(`[data-idx="${active}"]`)
      ?.scrollIntoView({ block: 'nearest' });
  }, [active, open]);

  // 点击面板外部关闭。
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (
        popoverRef.current &&
        !popoverRef.current.contains(e.target as Node) &&
        !textareaRef.current?.contains(e.target as Node)
      ) {
        close();
      }
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [open, close, textareaRef]);

  const button = (
    <button
      type="button"
      onClick={openFromButton}
      disabled={disabled || !sessionId}
      className="w-7 h-7 rounded-md border-0 bg-transparent text-text-secondary/70 flex items-center justify-center hover:bg-surface-hover hover:text-text-primary transition-colors disabled:opacity-40"
      title={sessionId ? '技能（也可输入 @ 呼出）' : '开始会话后可使用技能'}
    >
      <Plus size={15} />
    </button>
  );

  let idx = -1;
  const renderRow = (s: SkillInfo) => {
    idx += 1;
    const i = idx;
    const mounted = selectedSkills.includes(s.name);
    return (
      <div
        key={s.name}
        data-idx={i}
        role="button"
        tabIndex={-1}
        onMouseDown={(e) => {
          e.preventDefault(); // 保持 textarea 焦点
          void pick(s);
        }}
        onMouseEnter={() => setActive(i)}
        className={`flex items-center gap-2 px-2.5 py-1.5 rounded-lg cursor-pointer ${
          i === active ? 'bg-primary/10' : ''
        }`}
      >
        <SkillIcon name={s.name} size={16} />
        <span className="text-[12px] font-mono text-text-primary shrink-0">{s.name}</span>
        <span className="text-[11px] text-text-secondary/70 truncate flex-1">{s.description}</span>
        {mounted && (
          <span className="shrink-0 text-[10px] px-1.5 py-px rounded-full border border-primary/30 text-primary bg-primary/10">
            已挂载
          </span>
        )}
      </div>
    );
  };

  const popover = open ? (
    <div
      ref={popoverRef}
      className="absolute bottom-full left-0 right-0 mb-2 z-30 rounded-xl border border-surface-hover bg-surface shadow-2xl overflow-hidden"
    >
      <div className="px-3 pt-2.5 pb-1 text-[11px] text-text-secondary/60">
        选择技能{query && <> · 筛选「{query}」</>}
      </div>
      <div ref={listRef} className="max-h-[240px] overflow-y-auto px-1.5 pb-1.5">
        {skills === null ? (
          <div className="flex items-center justify-center py-6 text-text-secondary/60">
            <Loader2 size={14} className="animate-spin" />
          </div>
        ) : flatList.length === 0 ? (
          <div className="px-2.5 py-4 text-[12px] text-text-secondary/60 text-center">
            {skills.length === 0
              ? '还没有安装技能，可在左侧「插件」中导入'
              : '没有匹配的技能'}
          </div>
        ) : (
          <>
            {builtin.length > 0 && (
              <>
                <div className="px-2.5 pt-1 pb-0.5 text-[10px] text-text-secondary/50">内置</div>
                {builtin.map(renderRow)}
              </>
            )}
            {user.length > 0 && (
              <>
                <div className="px-2.5 pt-1.5 pb-0.5 text-[10px] text-text-secondary/50">我的插件</div>
                {user.map(renderRow)}
              </>
            )}
          </>
        )}
      </div>
      <div className="px-3 py-1.5 border-t border-surface-hover text-[10px] text-text-secondary/50 flex items-center gap-2">
        <span>↑↓ 选择 · Enter 使用 · Esc 关闭</span>
        {mounting && (
          <span className="ml-auto inline-flex items-center gap-1 text-primary">
            <Loader2 size={10} className="animate-spin" /> 挂载中…
          </span>
        )}
        {!mounting && <span className="ml-auto">未挂载的技能会自动挂载到本会话</span>}
      </div>
    </div>
  ) : null;

  // chip 列表 = 会话已挂载技能。打开面板前就渲染，所以这里提前拉取。
  const mountedSkills = useMemo(
    () => (skills ?? []).filter((s) => selectedSkills.includes(s.name)),
    [skills, selectedSkills],
  );
  useEffect(() => {
    if (selectedSkills.length > 0 && skills === null) void ensureSkills();
  }, [selectedSkills.length, skills, ensureSkills]);

  const chips = mountedSkills.length > 0 ? (
    <div className="flex flex-wrap gap-1.5 px-2 pt-2">
      {mountedSkills.map((s) => {
        const focused = focus === s.name;
        return (
          <span
            key={s.name}
            className={`inline-flex items-center gap-1 pl-1.5 pr-1 py-1 rounded-md border text-[11px] transition-colors ${
              focused
                ? 'border-primary/50 bg-primary/10 text-primary'
                : 'border-transparent bg-surface-hover/50 text-text-secondary'
            }`}
            title={focused ? '下一条消息将指定使用该技能（点击取消）' : `${s.description}\n点击：下一条消息指定使用该技能`}
          >
            <button
              type="button"
              onClick={() => setFocus(focused ? null : s.name)}
              className="inline-flex items-center gap-1"
            >
              <SkillIcon name={s.name} size={13} />
              <span className="font-mono">{s.name}</span>
            </button>
            <button
              type="button"
              onClick={() => void unmount(s.name)}
              title="从会话卸载"
              aria-label={`卸载 ${s.name}`}
              className="hover:text-red-400 shrink-0"
            >
              <X size={11} />
            </button>
          </span>
        );
      })}
    </div>
  ) : null;

  return { button, popover, chips, handleChange, handleKeyDown, close, takeFocusHint };
}

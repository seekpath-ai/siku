import { useEffect, useState } from 'react';
import { Loader2, Check } from 'lucide-react';
import { settingsGet, settingsSet, petDomains, petListSkins, petGetSkinSelection, petSetSkinSelection, petGetSize, petSetSize, petGetChatLinked, petSetChatLinked } from '@/lib/tauri';
import type { PetSkinSummary } from '@/lib/tauri';

interface DomainConfig {
  id: string;
  name: string;
  description: string;
}

const DOMAINS: DomainConfig[] = [
  { id: 'note_organizer', name: '笔记整理', description: '在笔记页使用，整理当前笔记内容' },
  { id: 'literature_analyzer', name: '文献分析', description: '在图书馆与阅读器使用，分析当前文献' },
  { id: 'research_tracker', name: '科研追踪', description: '在科研课题页使用，梳理课题进展' },
  { id: 'knowledge_curator', name: '知识库整理', description: '在知识库页使用，整理知识条目' },
  { id: 'chat_summarizer', name: '对话总结', description: '在对话页使用，总结当前对话' },
];

interface DomainState {
  enabled: boolean;
  prompt: string;
  /** Per-round output cap override; '' = built-in domain default. */
  maxTokens: string;
}

/** Settings page section: per-domain pet agent toggles + prompt overrides. */
export function PetSettings() {
  const [states, setStates] = useState<Record<string, DomainState>>({});
  const [loading, setLoading] = useState(true);
  const [savingKey, setSavingKey] = useState<string | null>(null);
  const [savedKey, setSavedKey] = useState<string | null>(null);
  /** Built-in default prompts per domain, shown as placeholders. */
  const [defaultPrompts, setDefaultPrompts] = useState<Record<string, string>>({});
  /** Built-in per-domain output caps, shown as placeholders. */
  const [defaultMaxTokens, setDefaultMaxTokens] = useState<Record<string, number | null>>({});
  /** Installed .codex-pet skins + the selected one ('' = built-in ball). */
  const [skins, setSkins] = useState<PetSkinSummary[]>([]);
  const [skinId, setSkinId] = useState('');
  /** Ball edge length in px. */
  const [ballSize, setBallSize] = useState(48);
  /** Whether plain chat sessions drive the pet ball (default on). */
  const [chatLinked, setChatLinked] = useState(true);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      // Load the built-in default prompts so the textarea placeholder shows
      // exactly what prompt is in use when the override is empty.
      try {
        const domains = await petDomains();
        if (!cancelled) {
          setDefaultPrompts(Object.fromEntries(domains.map((d) => [d.id, d.default_prompt])));
          setDefaultMaxTokens(Object.fromEntries(domains.map((d) => [d.id, d.default_max_tokens])));
        }
      } catch (e) {
        console.error('load pet domains:', e);
      }
      try {
        const [list, selected, size, linked] = await Promise.all([petListSkins(), petGetSkinSelection(), petGetSize(), petGetChatLinked()]);
        if (!cancelled) {
          setSkins(list);
          setSkinId(selected);
          setBallSize(size);
          setChatLinked(linked);
        }
      } catch (e) {
        console.error('load pet skins:', e);
      }
      const result: Record<string, DomainState> = {};
      for (const d of DOMAINS) {
        const [enabled, prompt, maxTokens] = await Promise.all([
          settingsGet(`pet.${d.id}.enabled`),
          settingsGet(`pet.${d.id}.prompt`),
          settingsGet(`pet.${d.id}.max_tokens`),
        ]).catch(() => [null, null, null] as const);
        result[d.id] = {
          enabled: enabled !== '0', // default enabled
          prompt: prompt ?? '',
          maxTokens: maxTokens ?? '',
        };
      }
      if (!cancelled) {
        setStates(result);
        setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, []);

  const setEnabled = async (id: string, enabled: boolean) => {
    setStates((s) => ({ ...s, [id]: { ...s[id], enabled } }));
    await settingsSet(`pet.${id}.enabled`, enabled ? '1' : '0').catch((e) => console.error('save pet enabled:', e));
  };

  const savePrompt = async (id: string) => {
    const st = states[id];
    const prompt = st?.prompt ?? '';
    const maxTokens = st?.maxTokens.trim() ?? '';
    setSavingKey(id);
    try {
      // Empty string = clear the override; the runtime parser treats
      // unset/unparsable values as "use the built-in default".
      await settingsSet(`pet.${id}.prompt`, prompt);
      await settingsSet(`pet.${id}.max_tokens`, maxTokens);
      setSavedKey(id);
      window.setTimeout(() => setSavedKey(null), 2000);
    } catch (e) {
      console.error('save pet prompt:', e);
    } finally {
      setSavingKey(null);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center gap-2 text-sm text-text-secondary/60 py-6">
        <Loader2 size={14} className="animate-spin" />加载中...
      </div>
    );
  }

  return (
    <div className="space-y-6">
      <div>
        <h2 className="text-base font-semibold text-text-primary">宠物智能体</h2>
        <p className="text-xs text-text-secondary/60 mt-1">
          全局宠物球在不同页面唤起对应的内置智能体。可在此控制各智能体是否启用，或自定义其提示词与单轮词元输出上限（留空使用默认）。
        </p>
      </div>

      {/* Skin picker: built-in CSS ball or any installed .codex-pet. */}
      <div className="rounded-lg border border-surface-hover bg-surface px-4 py-3">
        <div className="flex items-center gap-3">
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-text-primary">宠物外观</div>
            <div className="text-xs text-text-secondary/60 mt-0.5">
              兼容 Codex 宠物包：把宠物目录放入应用数据目录 pets/ 或 ~/.codex/pets/ 后重新打开本页即可选择。
            </div>
          </div>
          <select
            value={skinId}
            onChange={async (e) => {
              const id = e.target.value;
              setSkinId(id);
              await petSetSkinSelection(id).catch((err) => console.error('save pet skin:', err));
            }}
            className="h-8 rounded-lg bg-background border border-surface-hover text-sm text-text-primary px-2 focus:outline-none focus:border-primary/50"
          >
            <option value="">内置球（默认）</option>
            {skins.map((s) => (
              <option key={s.id} value={s.id}>
                {s.displayName}{s.source === 'codex' ? '（来自 Codex）' : ''}
              </option>
            ))}
          </select>
        </div>
        {skinId && !skins.some((s) => s.id === skinId) && (
          <div className="text-xs text-amber-500 mt-2">当前选择的皮肤已不存在，已回退为内置球显示；请重新选择。</div>
        )}
        <div className="flex items-center gap-3 mt-3 pt-3 border-t border-surface-hover">
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-text-primary">桌宠大小</div>
          </div>
          <select
            value={ballSize}
            onChange={async (e) => {
              const size = Number(e.target.value);
              setBallSize(size);
              await petSetSize(size).catch((err) => console.error('save pet size:', err));
            }}
            className="h-8 rounded-lg bg-background border border-surface-hover text-sm text-text-primary px-2 focus:outline-none focus:border-primary/50"
          >
            <option value={48}>小</option>
            <option value={72}>中</option>
            <option value={96}>大</option>
            <option value={128}>超大</option>
          </select>
        </div>
        <div className="flex items-center gap-3 mt-3 pt-3 border-t border-surface-hover">
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-text-primary">对话时桌宠联动</div>
            <div className="text-xs text-text-secondary/60 mt-0.5">
              对话生成期间桌宠同步显示工作状态，离开对话页查看笔记、文献时也能感知进度。
            </div>
          </div>
          <button
            role="switch"
            aria-checked={chatLinked}
            onClick={async () => {
              const next = !chatLinked;
              setChatLinked(next);
              await petSetChatLinked(next).catch((err) => console.error('save chat linked:', err));
            }}
            className={`w-9 h-5 rounded-full transition-colors shrink-0 ${
              chatLinked ? 'bg-primary' : 'bg-surface-hover'
            }`}
          >
            <span
              className={`block w-4 h-4 rounded-full bg-white shadow transition-transform ${
                chatLinked ? 'translate-x-[18px]' : 'translate-x-0.5'
              }`}
            />
          </button>
        </div>
      </div>

      {DOMAINS.map((d) => {
        const st = states[d.id];
        if (!st) return null;
        return (
          <div key={d.id} className="rounded-lg border border-surface-hover bg-surface overflow-hidden">
            <div className="flex items-center gap-3 px-4 py-3">
              <div className="flex-1 min-w-0">
                <div className="text-sm font-medium text-text-primary">{d.name}</div>
                <div className="text-xs text-text-secondary/60 mt-0.5">{d.description}</div>
              </div>
              {/* Toggle */}
              <button
                role="switch"
                aria-checked={st.enabled}
                onClick={() => setEnabled(d.id, !st.enabled)}
                className={`w-9 h-5 rounded-full transition-colors shrink-0 ${
                  st.enabled ? 'bg-primary' : 'bg-surface-hover'
                }`}
              >
                <span
                  className={`block w-4 h-4 rounded-full bg-white shadow transition-transform ${
                    st.enabled ? 'translate-x-[18px]' : 'translate-x-0.5'
                  }`}
                />
              </button>
            </div>

            {st.enabled && (
              <div className="px-4 pb-4">
                <div className="flex items-center gap-2 mb-2">
                  <label className="text-xs text-text-secondary/70 shrink-0">单轮词元输出上限</label>
                  <input
                    type="number"
                    value={st.maxTokens}
                    placeholder={defaultMaxTokens[d.id]?.toString() ?? '跟随模型'}
                    onChange={(e) => setStates((s) => ({ ...s, [d.id]: { ...s[d.id], maxTokens: e.target.value } }))}
                    className="w-28 rounded-lg bg-background border border-surface-hover px-2 py-1 text-xs text-text-primary placeholder:text-text-secondary/40 focus:outline-none focus:border-primary/40"
                  />
                  <span className="text-[10px] text-text-secondary/50">留空使用内置默认；作用于每轮 ReAct 输出，立即生效</span>
                </div>
                <textarea
                  value={st.prompt}
                  onChange={(e) => setStates((s) => ({ ...s, [d.id]: { ...s[d.id], prompt: e.target.value } }))}
                  rows={5}
                  placeholder={defaultPrompts[d.id] || '留空使用内置默认提示词'}
                  className="w-full rounded-lg bg-background border border-surface-hover px-3 py-2 text-xs text-text-primary placeholder:text-text-secondary/40 focus:outline-none focus:border-primary/40 resize-y"
                />
                <div className="flex items-center justify-between mt-1.5">
                  {st.prompt.trim() ? (
                    <span className="text-[10px] text-text-secondary/50">已启用自定义提示词，覆盖内置默认</span>
                  ) : (
                    <span className="text-[10px] text-text-secondary/50">
                      当前使用内置默认提示词（见上方占位文本）
                    </span>
                  )}
                </div>
                <div className="flex justify-end mt-2">
                  <button
                    onClick={() => savePrompt(d.id)}
                    disabled={savingKey === d.id}
                    className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg bg-primary/10 text-primary text-xs hover:bg-primary/20 transition-colors disabled:opacity-50"
                  >
                    {savingKey === d.id ? (
                      <><Loader2 size={11} className="animate-spin" />保存中...</>
                    ) : savedKey === d.id ? (
                      <><Check size={11} />已保存</>
                    ) : (
                      '保存'
                    )}
                  </button>
                </div>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

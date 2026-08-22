import { useState, useEffect, useCallback } from 'react';
import { Folder, HardDrive, Loader2, FolderOpen, Cat, Check, Home } from 'lucide-react';
import { listen } from '@tauri-apps/api/event';
import { settingsGetDataDir, settingsAppGet, settingsAppSave } from '@/lib/tauri';
import { useTabStore } from '@/stores/tabStore';
import { pickDirectory } from '@/lib/pickDirectory';

const HOME_OPTIONS = [
  { value: '/library', label: '图书馆' },
  { value: '/chat', label: '对话' },
  { value: '/notes', label: '笔记' },
  { value: '/knowledge', label: '知识库' },
  { value: '/research', label: '科研追踪' },
  { value: '/graph', label: '知识图谱' },
  { value: '/bookmarks', label: '书签' },
  { value: '/timeline', label: '时间轴' },
  { value: '/files', label: '文件列表' },
  { value: '/settings', label: '设置' },
];

export function GeneralSettings() {
  const [currentDir, setCurrentDir] = useState<string>('');
  const [dataDir, setDataDir] = useState<string>('');
  const [dataDirSaving, setDataDirSaving] = useState(false);
  const [dataDirSaved, setDataDirSaved] = useState(false);
  const [showPet, setShowPet] = useState<boolean>(true);
  const [homepage, setHomepage] = useState<string>('/library');
  const [homepageSaving, setHomepageSaving] = useState(false);
  const [homepageSaved, setHomepageSaved] = useState(false);
  const [loading, setLoading] = useState(true);
  const [petSaving, setPetSaving] = useState(false);
  const [petSaved, setPetSaved] = useState(false);

  const loadSettings = useCallback(() => {
    Promise.all([settingsGetDataDir(), settingsAppGet()])
      .then(([actual, settings]) => {
        setCurrentDir(actual);
        setDataDir(settings.data_dir || '');
        setShowPet(settings.show_pet ?? true);
        setHomepage(settings.homepage || '/library');
      })
      .catch((err) => console.error('Failed to load general settings:', err))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    loadSettings();
  }, [loadSettings]);

  // Keep the pet toggle in sync when it is changed from another webview (e.g. the pet ball's context menu).
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    const setup = async () => {
      unlisten = await listen('app:settings_changed', () => {
        settingsAppGet()
          .then((settings) => {
            setShowPet(settings.show_pet ?? true);
            setDataDir(settings.data_dir || '');
            setHomepage(settings.homepage || '/library');
          })
          .catch((err) => console.error('Failed to refresh settings:', err));
      });
    };
    setup();
    return () => unlisten?.();
  }, []);

  const saveDataDir = async (next: string) => {
    setDataDirSaving(true);
    try {
      const current = await settingsAppGet();
      await settingsAppSave({ ...current, data_dir: next || null });
      setDataDirSaved(true);
      window.setTimeout(() => setDataDirSaved(false), 2000);
    } catch (err) {
      console.error('Failed to save data dir:', err);
    } finally {
      setDataDirSaving(false);
    }
  };

  const handlePick = async () => {
    const selected = await pickDirectory(dataDir || currentDir);
    if (!selected) return;
    setDataDir(selected);
    await saveDataDir(selected);
  };

  const savePetVisibility = async (next: boolean) => {
    setPetSaving(true);
    try {
      const current = await settingsAppGet();
      await settingsAppSave({ ...current, show_pet: next });
      setPetSaved(true);
      window.setTimeout(() => setPetSaved(false), 2000);
    } catch (err) {
      console.error('Failed to save pet visibility:', err);
    } finally {
      setPetSaving(false);
    }
  };

  const saveHomepage = async (next: string) => {
    setHomepageSaving(true);
    try {
      const current = await settingsAppGet();
      await settingsAppSave({ ...current, homepage: next });
      useTabStore.getState().setHomeTab(next);
      setHomepageSaved(true);
      window.setTimeout(() => setHomepageSaved(false), 2000);
    } catch (err) {
      console.error('Failed to save homepage:', err);
    } finally {
      setHomepageSaving(false);
    }
  };

  return (
    <div className="space-y-6">
      <h2 className="text-lg font-semibold text-text-primary">通用设置</h2>

      <div className="space-y-3">
        <div className="flex items-center gap-2 text-sm text-text-primary">
          <HardDrive size={16} className="text-primary" />
          <span>数据存储路径</span>
        </div>
        <p className="text-xs text-text-secondary">
          数据库和论文文件存储位置。修改后需重启应用生效。
        </p>

        {loading ? (
          <div className="flex items-center gap-2 text-sm text-text-secondary">
            <Loader2 size={14} className="animate-spin" />加载中...
          </div>
        ) : (
          <div className="flex items-center gap-3 px-4 py-3.5 bg-surface border border-surface-hover rounded-xl">
            <code className="flex-1 min-w-0 px-3 py-2 bg-background border border-surface-hover rounded-lg text-xs text-text-primary font-mono truncate">
              <Folder size={12} className="inline mr-1.5 text-text-secondary" />
              {dataDir || currentDir}
            </code>
            <button
              onClick={handlePick}
              disabled={dataDirSaving}
              className="flex items-center gap-1.5 px-3 py-2 bg-background border border-surface-hover rounded-lg text-xs text-text-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors shrink-0"
            >
              {dataDirSaving ? <Loader2 size={14} className="animate-spin" /> : <FolderOpen size={14} />}
              选择文件夹
            </button>
            {dataDirSaved && !dataDirSaving && (
              <span className="flex items-center gap-1 text-xs text-accent shrink-0">
                <Check size={12} /> 已保存
              </span>
            )}
          </div>
        )}
      </div>

      {/* Homepage */}
      <div className="space-y-3">
        <div className="flex items-center gap-2 text-sm text-text-primary">
          <Home size={16} className="text-primary" />
          <span>启动页</span>
        </div>
        <p className="text-xs text-text-secondary">打开应用或点击首页时默认进入的页面。</p>
        {loading ? (
          <div className="flex items-center gap-2 text-sm text-text-secondary">
            <Loader2 size={14} className="animate-spin" />加载中...
          </div>
        ) : (
          <div className="flex items-center gap-3 px-4 py-3.5 bg-surface border border-surface-hover rounded-xl">
            <select
              value={homepage}
              onChange={(e) => {
                const next = e.target.value;
                setHomepage(next);
                saveHomepage(next);
              }}
              disabled={homepageSaving}
              className="flex-1 min-w-0 px-3 py-2 bg-background border border-surface-hover rounded-lg text-xs text-text-primary focus:outline-none focus:border-primary/50 disabled:opacity-50"
            >
              {HOME_OPTIONS.map((opt) => (
                <option key={opt.value} value={opt.value}>
                  {opt.label}
                </option>
              ))}
            </select>
            {homepageSaved && !homepageSaving && (
              <span className="flex items-center gap-1 text-xs text-accent shrink-0">
                <Check size={12} /> 已保存
              </span>
            )}
          </div>
        )}
      </div>

      {/* Pet visibility */}
      <div className="space-y-3">
        <div className="flex items-center gap-2 text-sm text-text-primary">
          <Cat size={16} className="text-primary" />
          <span>桌面宠物</span>
        </div>
        <div
          className={`flex items-center gap-4 px-4 py-3.5 bg-surface border rounded-xl transition-colors ${
            showPet ? 'border-primary/30' : 'border-surface-hover'
          }`}
        >
          {/* Pet ball preview */}
          <div
            className={`w-11 h-11 rounded-full bg-gradient-to-br from-primary to-amber-700 flex items-center justify-center shadow-lg shrink-0 transition-opacity ${
              showPet ? 'opacity-100' : 'opacity-40'
            }`}
          >
            <div className="relative w-6 h-6">
              <div className="absolute top-1 left-0 w-2 h-2 rounded-full bg-background" />
              <div className="absolute top-1 right-0 w-2 h-2 rounded-full bg-background" />
              <div className="absolute bottom-0.5 left-1/2 -translate-x-1/2 w-3 h-1.5 rounded-b-full bg-background/80" />
            </div>
          </div>

          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-text-primary">显示桌面宠物</div>
            <p className="text-xs text-text-secondary mt-0.5">
              关闭后桌面上的宠物球将被隐藏，可通过重新打开此开关恢复显示。
            </p>
          </div>

          <div className="flex items-center gap-2 shrink-0">
            {petSaving ? (
              <Loader2 size={14} className="animate-spin text-text-secondary" />
            ) : petSaved ? (
              <span className="flex items-center gap-1 text-[10px] text-accent">
                <Check size={12} /> 已保存
              </span>
            ) : null}
            {/* Switch (matches the pet settings page) */}
            <button
              role="switch"
              aria-checked={showPet}
              disabled={petSaving}
              onClick={() => {
                const next = !showPet;
                setShowPet(next);
                savePetVisibility(next);
              }}
              className={`w-9 h-5 rounded-full transition-colors shrink-0 disabled:opacity-50 ${
                showPet ? 'bg-primary' : 'bg-surface-hover'
              }`}
            >
              <span
                className={`block w-4 h-4 rounded-full bg-white shadow transition-transform ${
                  showPet ? 'translate-x-[18px]' : 'translate-x-0.5'
                }`}
              />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

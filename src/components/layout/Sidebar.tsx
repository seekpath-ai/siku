import { useEffect, useRef, useState } from 'react';
import {
  Library,
  Bot,
  GitGraph,
  Settings,
  StickyNote,
  FolderOpen,
  FlaskConical,
  Folder,
  Clock,
  Bookmark,
  Home,
} from 'lucide-react';
import { useLocation, useNavigate } from '@tanstack/react-router';
import { useTabStore } from '@/stores/tabStore';
import { settingsAppGet, settingsAppSave } from '@/lib/tauri';
import {
  DndContext,
  closestCenter,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  arrayMove,
  SortableContext,
  verticalListSortingStrategy,
  useSortable,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { ContextMenu, type ContextMenuItem } from '@/components/ui/ContextMenu';

interface NavItem {
  label: string;
  path: string;
  icon: React.ReactNode;
  tabIcon: string;
}

const defaultPrimaryItems: NavItem[] = [
  { label: '图书馆', path: '/library', icon: <Library size={18} />, tabIcon: 'home' },
  { label: '对话', path: '/chat', icon: <Bot size={18} />, tabIcon: 'chat' },
  { label: '笔记', path: '/notes', icon: <StickyNote size={18} />, tabIcon: 'note' },
  { label: '知识库', path: '/knowledge', icon: <FolderOpen size={18} />, tabIcon: 'knowledge' },
  { label: '科研追踪', path: '/research', icon: <FlaskConical size={18} />, tabIcon: 'research' },
  { label: '知识图谱', path: '/graph', icon: <GitGraph size={18} />, tabIcon: 'graph' },
];

const defaultSecondaryItems: NavItem[] = [
  { label: '书签', path: '/bookmarks', icon: <Bookmark size={18} />, tabIcon: 'bookmark' },
  { label: '时间轴', path: '/timeline', icon: <Clock size={18} />, tabIcon: 'clock' },
  { label: '文件列表', path: '/files', icon: <Folder size={18} />, tabIcon: 'files' },
];

const settingsItem: NavItem = { label: '设置', path: '/settings', icon: <Settings size={18} />, tabIcon: 'settings' };

function applyOrder(items: NavItem[], order: string[] | null | undefined): NavItem[] {
  if (!order || order.length === 0) return items;
  const map = new Map(items.map((i) => [i.path, i]));
  const ordered: NavItem[] = [];
  for (const path of order) {
    const item = map.get(path);
    if (item) {
      ordered.push(item);
      map.delete(path);
    }
  }
  // Append any items not in the saved order (new features) at the end.
  for (const item of items) {
    if (map.has(item.path)) ordered.push(item);
  }
  return ordered;
}

function SortableRailItem({
  item,
  active,
  homepage,
  onClick,
  onSetHome,
}: {
  item: NavItem;
  active: boolean;
  homepage: string | null | undefined;
  onClick: () => void;
  onSetHome: () => void;
}) {
  const {
    attributes,
    listeners,
    setNodeRef,
    transform,
    transition,
    isDragging,
  } = useSortable({ id: item.path });

  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const wasDraggingRef = useRef(false);

  useEffect(() => {
    if (isDragging) wasDraggingRef.current = true;
  }, [isDragging]);

  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
  };

  const isHome = homepage === item.path;

  const menuItems: ContextMenuItem[] = [
    {
      label: isHome ? '取消首页' : '设为首页',
      icon: <Home size={14} />,
      onClick: onSetHome,
    },
  ];

  const handleClick = () => {
    // If this item was just dragged, ignore the trailing click event so we
    // don't accidentally navigate after reordering.
    if (wasDraggingRef.current) {
      wasDraggingRef.current = false;
      return;
    }
    onClick();
  };

  return (
    <div ref={setNodeRef} style={style} className={isDragging ? 'opacity-50' : undefined}>
      <button
        {...attributes}
        {...listeners}
        onClick={handleClick}
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY });
        }}
        title={item.label}
        className={`relative w-[34px] h-[34px] rounded flex items-center justify-center transition-colors mb-0.5 cursor-grab active:cursor-grabbing ${
          active
            ? 'bg-surface-hover text-primary'
            : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover/60'
        }`}
      >
        {item.icon}
        {isHome && (
          <span className="absolute bottom-0.5 right-0.5 w-1.5 h-1.5 rounded-full bg-primary" />
        )}
      </button>
      {menu && <ContextMenu x={menu.x} y={menu.y} items={menuItems} onClose={() => setMenu(null)} />}
    </div>
  );
}

export function Sidebar() {
  const location = useLocation();
  const navigate = useNavigate();
  const { openRoute } = useTabStore();
  const isActive = (path: string) => location.pathname.startsWith(path);

  const [primaryItems, setPrimaryItems] = useState(defaultPrimaryItems);
  const [secondaryItems, setSecondaryItems] = useState(defaultSecondaryItems);
  const [homepage, setHomepage] = useState<string | null | undefined>(null);
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let cancelled = false;
    settingsAppGet()
      .then((settings) => {
        if (cancelled) return;
        setPrimaryItems(applyOrder(defaultPrimaryItems, settings.sidebar_order));
        setSecondaryItems(applyOrder(defaultSecondaryItems, settings.sidebar_order));
        setHomepage(settings.homepage);
      })
      .catch(() => {})
      .finally(() => setLoaded(true));
    return () => {
      cancelled = true;
    };
  }, []);

  const sensors = useSensors(
    useSensor(PointerSensor, {
      activationConstraint: { distance: 5 },
    })
  );

  const handleNav = (item: NavItem) => {
    const tab = openRoute(item.path, { title: item.label, icon: item.tabIcon });
    if (tab.params) {
      navigate({ to: tab.route, params: tab.params });
    } else {
      navigate({ to: tab.route });
    }
  };

  const saveOrder = async (nextPrimary: NavItem[], nextSecondary: NavItem[]) => {
    try {
      const current = await settingsAppGet();
      await settingsAppSave({
        ...current,
        sidebar_order: [...nextPrimary.map((i) => i.path), ...nextSecondary.map((i) => i.path)],
      });
    } catch {
      // best-effort
    }
  };

  const handlePrimaryDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (over && active.id !== over.id) {
      setPrimaryItems((items) => {
        const oldIndex = items.findIndex((i) => i.path === active.id);
        const newIndex = items.findIndex((i) => i.path === over.id);
        const next = arrayMove(items, oldIndex, newIndex);
        saveOrder(next, secondaryItems);
        return next;
      });
    }
  };

  const handleSecondaryDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (over && active.id !== over.id) {
      setSecondaryItems((items) => {
        const oldIndex = items.findIndex((i) => i.path === active.id);
        const newIndex = items.findIndex((i) => i.path === over.id);
        const next = arrayMove(items, oldIndex, newIndex);
        saveOrder(primaryItems, next);
        return next;
      });
    }
  };

  const handleSetHome = async (path: string) => {
    const next = homepage === path ? null : path;
    setHomepage(next);
    useTabStore.getState().setHomeTab(next || '/library');
    try {
      const current = await settingsAppGet();
      await settingsAppSave({ ...current, homepage: next });
    } catch {
      // best-effort
    }
  };

  const renderItem = (item: NavItem) => (
    <SortableRailItem
      key={item.path}
      item={item}
      active={isActive(item.path)}
      homepage={homepage}
      onClick={() => handleNav(item)}
      onSetHome={() => handleSetHome(item.path)}
    />
  );

  // Prevent layout shift before settings load.
  if (!loaded) {
    return (
      <aside className="flex h-full w-11 flex-col items-center bg-background border-r border-surface-hover shrink-0 py-2">
        <nav className="flex flex-col items-center">
          {defaultPrimaryItems.map((item) => (
            <div
              key={item.path}
              className="w-[34px] h-[34px] rounded flex items-center justify-center mb-0.5 text-text-secondary/30"
            >
              {item.icon}
            </div>
          ))}
        </nav>
        <div className="w-5 h-px bg-surface-hover my-2" />
        <nav className="flex flex-col items-center flex-1">
          {defaultSecondaryItems.map((item) => (
            <div
              key={item.path}
              className="w-[34px] h-[34px] rounded flex items-center justify-center mb-0.5 text-text-secondary/30"
            >
              {item.icon}
            </div>
          ))}
        </nav>
        <div className="pb-1">
          <div className="w-[34px] h-[34px] rounded flex items-center justify-center text-text-secondary/30">
            <Settings size={18} />
          </div>
        </div>
      </aside>
    );
  }

  return (
    <aside className="flex h-full w-11 flex-col items-center bg-background border-r border-surface-hover shrink-0 py-2">
      <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={handlePrimaryDragEnd}>
        <SortableContext items={primaryItems.map((i) => i.path)} strategy={verticalListSortingStrategy}>
          <nav className="flex flex-col items-center">{primaryItems.map(renderItem)}</nav>
        </SortableContext>
      </DndContext>

      <div className="w-5 h-px bg-surface-hover my-2" />

      <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={handleSecondaryDragEnd}>
        <SortableContext items={secondaryItems.map((i) => i.path)} strategy={verticalListSortingStrategy}>
          <nav className="flex flex-col items-center flex-1">{secondaryItems.map(renderItem)}</nav>
        </SortableContext>
      </DndContext>

      <div className="pb-1">
        <button
          onClick={() => handleNav(settingsItem)}
          title={settingsItem.label}
          className={`w-[34px] h-[34px] rounded flex items-center justify-center transition-colors ${
            isActive('/settings')
              ? 'bg-surface-hover text-primary'
              : 'text-text-secondary hover:text-text-primary hover:bg-surface-hover/60'
          }`}
        >
          {settingsItem.icon}
        </button>
      </div>
    </aside>
  );
}

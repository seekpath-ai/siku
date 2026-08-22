import { useState, useMemo, useCallback, useRef, useEffect } from 'react';
import {
  Library,
  ChevronRight,
  ChevronDown,
  Folder,
  Tag,
  Plus,
  Trash2,
  Edit3,
  Palette,
  Search,
  X,
  Clock,
} from 'lucide-react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useCollections, useCreateCollection, useUpdateCollection, useDeleteCollection, useAddPapersToCollection, useTags, useCreateTag, useDeleteTag, useUpdateTag } from '@/hooks/useLibrary';
import { savedSearchesList, savedSearchesDelete } from '@/lib/tauri';
import { useLibraryStore } from '@/stores/libraryStore';
import { useDialog } from '@/hooks/useDialog';
import type { Collection, Tag as TagType } from '@/lib/types';
import { ContextMenu, type ContextMenuItem } from '@/components/ui/ContextMenu';
import { ResizeHandle } from './ResizeHandle';

const TAG_SECTION_MIN = 100;
const TAG_SECTION_MAX = 420;
/** Natural height before the user drags the divider. */
const TAG_SECTION_AUTO_MAX = 200;

/** Zotero-style preset tag palette. */
const TAG_COLORS = [
  '#f1c40f', '#f39c12', '#e67e22', '#e74c3c', '#e91e63',
  '#9b59b6', '#3498db', '#2ecc71', '#95a5a6', '#34495e',
];

/** A node in the tag hierarchy, split from tag names by `#` (Zotero style). */
interface TagGroupNode {
  name: string;
  path: string;
  children: TagGroupNode[];
  tags: TagType[];
}

function buildTagTree(tags: TagType[]): TagGroupNode[] {
  const roots: TagGroupNode[] = [];
  const nodeMap = new Map<string, TagGroupNode>();
  for (const tag of tags) {
    const segments = tag.name
      .split('#')
      .map((s) => s.trim())
      .filter(Boolean);
    if (segments.length === 0) continue;
    let path = '';
    let parent: TagGroupNode | null = null;
    for (const seg of segments) {
      path = path ? `${path}#${seg}` : seg;
      if (!nodeMap.has(path)) {
        const node: TagGroupNode = { name: seg, path, children: [], tags: [] };
        nodeMap.set(path, node);
        if (parent) parent.children.push(node);
        else roots.push(node);
      }
      parent = nodeMap.get(path)!;
    }
    parent!.tags.push(tag);
  }
  return roots;
}

interface TreeNode {
  collection: Collection;
  children: TreeNode[];
}

function buildTree(collections: Collection[]): TreeNode[] {
  const map = new Map<string, TreeNode>();
  const roots: TreeNode[] = [];

  for (const collection of collections) {
    map.set(collection.id, { collection, children: [] });
  }

  for (const collection of collections) {
    const node = map.get(collection.id)!;
    if (collection.parent_id && map.has(collection.parent_id)) {
      map.get(collection.parent_id)!.children.push(node);
    } else {
      roots.push(node);
    }
  }

  return roots;
}

function CollectionTreeItem({
  node,
  depth,
  activeCollectionId,
  onSelect,
  expanded,
  onToggleExpand,
  onDropPapers,
}: {
  node: TreeNode;
  depth: number;
  activeCollectionId: string | null;
  onSelect: (id: string | null) => void;
  expanded: Set<string>;
  onToggleExpand: (id: string) => void;
  onDropPapers: (collectionId: string, paperIds: string[]) => void;
}) {
  const [menu, setMenu] = useState<{ x: number; y: number } | null>(null);
  const [editing, setEditing] = useState(false);
  const [editName, setEditName] = useState(node.collection.name);
  const [dragOver, setDragOver] = useState(false);
  const updateMutation = useUpdateCollection();
  const deleteMutation = useDeleteCollection();
  const createMutation = useCreateCollection();
  const { prompt, confirm } = useDialog();
  const isExpanded = expanded.has(node.collection.id);
  const isActive = activeCollectionId === node.collection.id;

  const handleContextMenu = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    setMenu({ x: e.clientX, y: e.clientY });
  }, []);

  const menuItems: ContextMenuItem[] = [
    {
      label: '新建子集合',
      icon: <Plus size={14} />,
      onClick: async () => {
        const name = await prompt('集合名称', { title: '新建子集合' });
        if (name?.trim()) {
          createMutation.mutate({ name: name.trim(), parentId: node.collection.id });
        }
      },
    },
    {
      label: '重命名',
      icon: <Edit3 size={14} />,
      onClick: () => {
        setEditName(node.collection.name);
        setEditing(true);
      },
    },
    {
      label: '删除集合',
      icon: <Trash2 size={14} />,
      destructive: true,
      onClick: async () => {
        const ok = await confirm(`确认删除集合 "${node.collection.name}"？其中的文献不会被删除。`, '删除集合');
        if (ok) {
          deleteMutation.mutate(node.collection.id);
        }
      },
    },
  ];

  const submitRename = () => {
    const name = editName.trim();
    if (name && name !== node.collection.name) {
      updateMutation.mutate({ id: node.collection.id, name });
    }
    setEditing(false);
  };

  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(true);
  };

  const handleDragLeave = () => {
    setDragOver(false);
  };

  const handleDrop = (e: React.DragEvent) => {
    e.preventDefault();
    setDragOver(false);
    const data = e.dataTransfer.getData('application/siku-papers');
    if (data) {
      try {
        const paperIds = JSON.parse(data) as string[];
        if (paperIds.length > 0) {
          onDropPapers(node.collection.id, paperIds);
        }
      } catch {
        // ignore invalid drop data
      }
    }
  };

  const hasChildren = node.children.length > 0;

  return (
    <div
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      <div
        onClick={() => onSelect(node.collection.id)}
        onContextMenu={handleContextMenu}
        className={`flex items-center gap-1.5 px-2 py-1.5 rounded cursor-pointer transition-colors ${
          isActive
            ? 'bg-primary/10 text-primary'
            : dragOver
              ? 'bg-primary/10 text-primary'
              : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
        }`}
        style={{ paddingLeft: `${8 + depth * 14}px` }}
      >
        <button
          onClick={(e) => {
            e.stopPropagation();
            onToggleExpand(node.collection.id);
          }}
          className={`shrink-0 p-0.5 rounded hover:bg-surface-hover transition-colors ${
            hasChildren ? 'opacity-70' : 'opacity-0 pointer-events-none'
          }`}
        >
          {isExpanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        </button>
        <Folder size={14} className="shrink-0" />
        {editing ? (
          <input
            autoFocus
            value={editName}
            onChange={(e) => setEditName(e.target.value)}
            onBlur={submitRename}
            onKeyDown={(e) => {
              if (e.key === 'Enter') submitRename();
              if (e.key === 'Escape') setEditing(false);
            }}
            onClick={(e) => e.stopPropagation()}
            className="flex-1 min-w-0 h-6 px-1.5 text-xs rounded bg-surface border border-surface-hover text-text-primary focus:outline-none focus:border-primary/50"
          />
        ) : (
          <span className="truncate text-xs">{node.collection.name}</span>
        )}
      </div>

      {isExpanded && node.children.length > 0 && (
        <div>
          {node.children.map((child) => (
            <CollectionTreeItem
              key={child.collection.id}
              node={child}
              depth={depth + 1}
              activeCollectionId={activeCollectionId}
              onSelect={onSelect}
              expanded={expanded}
              onToggleExpand={onToggleExpand}
              onDropPapers={onDropPapers}
            />
          ))}
        </div>
      )}

      {menu && <ContextMenu x={menu.x} y={menu.y} items={menuItems} onClose={() => setMenu(null)} />}
    </div>
  );
}

export function CollectionTree() {
  const { data: collections, isLoading: collectionsLoading } = useCollections();
  const { data: tags, isLoading: tagsLoading } = useTags();
  const createCollection = useCreateCollection();
  const createTag = useCreateTag();
  const deleteTag = useDeleteTag();
  const updateTag = useUpdateTag();
  const addPapersToCollection = useAddPapersToCollection();
  const [tagMenu, setTagMenu] = useState<{ x: number; y: number; tag: TagType } | null>(null);
  const [colorPickerTag, setColorPickerTag] = useState<TagType | null>(null);
  const [tagCollapsed, setTagCollapsed] = useState<Set<string>>(new Set());
  const [libraryRootExpanded, setLibraryRootExpanded] = useState(true);
  const { prompt, confirm } = useDialog();

  const activeFilter = useLibraryStore((s) => s.activeFilter);
  const applySavedSearch = useLibraryStore((s) => s.applySavedSearch);
  const queryClient = useQueryClient();
  const { data: savedSearches = [], isLoading: savedSearchesLoading } = useQuery({
    queryKey: ['saved-searches'],
    queryFn: () => savedSearchesList(),
  });
  const deleteSavedSearch = useCallback(
    async (id: string) => {
      await savedSearchesDelete(id).catch(() => {});
      queryClient.invalidateQueries({ queryKey: ['saved-searches'] });
    },
    [queryClient]
  );
  const setActiveCollection = useLibraryStore((s) => s.setActiveCollection);
  const toggleActiveTag = useLibraryStore((s) => s.toggleActiveTag);
  const setTagFilterLogic = useLibraryStore((s) => s.setTagFilterLogic);

  // Tags section height. `null` = auto-sized by content; once the divider is
  // dragged it becomes a fixed pixel height.
  const [tagHeight, setTagHeight] = useState<number | null>(null);
  const tagsSectionRef = useRef<HTMLDivElement>(null);
  const dragStartHeight = useRef(TAG_SECTION_AUTO_MAX);

  // Capture the real section height when the drag starts: while auto-sized it
  // may be much shorter than the fallback, so using the fallback as the base
  // makes the divider jump away from the mouse on the first move.
  const handleTagResizeStart = () => {
    dragStartHeight.current =
      tagHeight ?? tagsSectionRef.current?.offsetHeight ?? TAG_SECTION_AUTO_MAX;
  };

  const handleTagResize = (delta: number) => {
    // The divider sits above the tags section: dragging it up (negative delta)
    // pushes the section's top edge up and grows it, so height moves opposite
    // to the drag direction.
    const next = dragStartHeight.current - delta;
    setTagHeight(Math.min(TAG_SECTION_MAX, Math.max(TAG_SECTION_MIN, next)));
  };

  const tree = useMemo(() => buildTree(collections ?? []), [collections]);
  const tagRoots = useMemo(() => buildTagTree(tags ?? []), [tags]);

  const toggleTagCollapse = (path: string) => {
    setTagCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  const activeCollectionId = activeFilter.type === 'collection' ? activeFilter.id : null;
  const activeTagIds = activeFilter.tagIds;
  const activeTagLogic = activeFilter.tagLogic;
  const expandedIds = useLibraryStore((s) => s.expandedCollectionIds);
  const toggleExpandedCollection = useLibraryStore((s) => s.toggleExpandedCollection);
  const expanded = useMemo(() => new Set(expandedIds), [expandedIds]);

  // Keep the library root expanded when a child scope is active.
  useEffect(() => {
    if (activeFilter.type === 'collection' || activeFilter.type === 'recent' || activeFilter.type === 'trash') {
      setLibraryRootExpanded(true);
    }
  }, [activeFilter.type]);

  const toggleExpand = (id: string) => {
    toggleExpandedCollection(id);
  };

  const handleDropPapers = (collectionId: string, paperIds: string[]) => {
    addPapersToCollection.mutate({ collectionId, paperIds });
  };

  const renderTagNode = (node: TagGroupNode, depth: number): React.ReactNode => {
    const isLeaf = node.children.length === 0;
    const hasSelf = node.tags.length > 0;
    const collapsed = tagCollapsed.has(node.path);
    if (isLeaf && !hasSelf) return null;

    // Leaf tag chip (clickable filter, context menu).
    if (isLeaf) {
      const tag = node.tags[0];
      const active = activeTagIds.includes(tag.id);
      return (
        <button
          key={tag.id}
          onClick={() => toggleActiveTag(tag.id)}
          onContextMenu={(e) => {
            e.preventDefault();
            setTagMenu({ x: e.clientX, y: e.clientY, tag });
          }}
          className={`inline-flex items-center gap-1.5 pl-2 pr-2 py-1 rounded-md text-xs transition-colors ${
            active
              ? 'bg-primary/20 text-primary ring-1 ring-primary/40'
              : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
          }`}
          style={{ marginLeft: depth * 14 }}
          title={tag.name}
        >
          <Tag size={11} style={{ color: tag.color }} />
          <span className="truncate max-w-[110px]">{node.name}</span>
          {tag.paper_count > 0 && (
            <span className="text-[10px] leading-none text-text-secondary/60">
              {tag.paper_count}
            </span>
          )}
        </button>
      );
    }

    // Group header (collapsible; clickable when it has its own real tag).
    return (
      <div key={node.path}>
        <div className="flex items-center gap-1 py-0.5 pr-1" style={{ marginLeft: depth * 14 }}>
          <button
            onClick={() => toggleTagCollapse(node.path)}
            className="shrink-0 p-0.5 rounded hover:bg-surface-hover text-text-secondary/60 hover:text-text-primary transition-colors"
          >
            {collapsed ? <ChevronRight size={12} /> : <ChevronDown size={12} />}
          </button>
          {hasSelf ? (
            <button
              onClick={() => toggleActiveTag(node.tags[0].id)}
              onContextMenu={(e) => {
                e.preventDefault();
                setTagMenu({ x: e.clientX, y: e.clientY, tag: node.tags[0] });
              }}
              className={`inline-flex items-center gap-1.5 px-1.5 py-0.5 rounded-md text-xs transition-colors ${
                activeTagIds.includes(node.tags[0].id)
                  ? 'bg-primary/20 text-primary ring-1 ring-primary/40'
                  : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
              }`}
              title={node.tags[0].name}
            >
              <Tag size={11} style={{ color: node.tags[0].color }} />
              <span className="truncate max-w-[100px] font-medium">{node.name}</span>
              {node.tags[0].paper_count > 0 && (
                <span className="text-[10px] leading-none text-text-secondary/60">
                  {node.tags[0].paper_count}
                </span>
              )}
            </button>
          ) : (
            <span className="text-xs text-text-secondary/70 font-medium truncate max-w-[120px]">
              {node.name}
            </span>
          )}
        </div>
        {!collapsed && node.children.map((child) => renderTagNode(child, depth + 1))}
      </div>
    );
  };

  return (
    <div className="flex flex-col h-full text-sm">
      {/* Header */}
      <div className="flex items-center justify-between px-3 py-2 border-b border-surface-hover">
        <span className="text-xs font-medium text-text-secondary/70 uppercase tracking-wider">图书馆</span>
        <button
          onClick={async () => {
            const name = await prompt('新建集合名称', { title: '新建集合' });
            if (name?.trim()) createCollection.mutate({ name: name.trim() });
          }}
          className="p-1 rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover"
          title="新建集合"
        >
          <Plus size={14} />
        </button>
      </div>

      {/* My Library root (Zotero-style): contains recent reads, trash and user collections. */}
      <div className="flex flex-col min-h-0 px-2 py-1.5 flex-1">
        <div className="flex items-center shrink-0">
          <button
            onClick={() => setLibraryRootExpanded((v) => !v)}
            className="shrink-0 p-0.5 rounded hover:bg-surface-hover transition-colors text-text-secondary/70 hover:text-text-primary"
            title={libraryRootExpanded ? '折叠' : '展开'}
          >
            {libraryRootExpanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
          </button>
          <button
            onClick={() => setActiveCollection(null)}
            className={`flex-1 flex items-center gap-2 px-2 py-1.5 rounded transition-colors ${
              activeFilter.type === 'all'
                ? 'bg-primary/10 text-primary'
                : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
            }`}
          >
            <Library size={15} />
            <span className="truncate">我的图书馆</span>
          </button>
        </div>

        {libraryRootExpanded && (
          <div className="overflow-y-auto mt-0.5">
            <button
              onClick={() => useLibraryStore.getState().openRecentReads()}
              className={`w-full flex items-center gap-1.5 px-2 py-1.5 rounded transition-colors ${
                activeFilter.type === 'recent'
                  ? 'bg-primary/10 text-primary'
                  : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
              }`}
              style={{ paddingLeft: '22px' }}
            >
              <span className="w-[17px] shrink-0" />
              <Clock size={14} className="shrink-0" />
              <span className="truncate text-xs">最近阅读</span>
            </button>
            {collectionsLoading ? (
              <div className="flex items-center gap-1.5 px-2 py-2 text-xs text-text-secondary/60" style={{ paddingLeft: '22px' }}>
                <span className="w-[17px] shrink-0" />
                加载中...
              </div>
            ) : tree.length === 0 ? (
              <div className="flex items-center gap-1.5 px-2 py-2 text-xs text-text-secondary/60" style={{ paddingLeft: '22px' }}>
                <span className="w-[17px] shrink-0" />
                暂无集合
              </div>
            ) : (
              tree.map((node) => (
                <CollectionTreeItem
                  key={node.collection.id}
                  node={node}
                  depth={1}
                  activeCollectionId={activeCollectionId}
                  onSelect={setActiveCollection}
                  expanded={expanded}
                  onToggleExpand={toggleExpand}
                  onDropPapers={handleDropPapers}
                />
              ))
            )}
            <button
              onClick={() => useLibraryStore.getState().openTrash()}
              className={`w-full flex items-center gap-1.5 px-2 py-1.5 rounded transition-colors ${
                activeFilter.type === 'trash'
                  ? 'bg-primary/10 text-primary'
                  : 'text-text-secondary hover:bg-surface-hover hover:text-text-primary'
              }`}
              style={{ paddingLeft: '22px' }}
            >
              <span className="w-[17px] shrink-0" />
              <Trash2 size={14} className="shrink-0" />
              <span className="truncate text-xs">回收站</span>
            </button>
          </div>
        )}
      </div>

      {/* Saved searches */}
      <div className="border-t border-surface-hover px-2 py-2 shrink-0 max-h-40 overflow-y-auto">
        <div className="px-2 pb-1">
          <span className="text-[11px] uppercase tracking-wide text-text-secondary/50">保存的搜索</span>
        </div>
        {savedSearchesLoading ? (
          <div className="px-2 py-1 text-xs text-text-secondary/60">加载中...</div>
        ) : savedSearches.length === 0 ? (
          <div className="px-2 py-1 text-xs text-text-secondary/40">在搜索栏保存常用搜索</div>
        ) : (
          savedSearches.map((s) => (
            <div key={s.id} className="group flex items-center">
              <button
                onClick={() => applySavedSearch(s.params_json)}
                className="flex-1 text-left flex items-center gap-2 px-2 py-1 rounded text-xs text-text-secondary hover:text-text-primary hover:bg-surface-hover transition-colors"
              >
                <Search size={12} className="shrink-0" />
                <span className="truncate">{s.name}</span>
              </button>
              <button
                onClick={() => deleteSavedSearch(s.id)}
                className="opacity-0 group-hover:opacity-100 p-0.5 rounded text-text-secondary/50 hover:text-red-400"
                title="删除保存的搜索"
              >
                <X size={12} />
              </button>
            </div>
          ))
        )}
      </div>

      {/* Draggable divider between the collection tree and the tags section */}
      <ResizeHandle
        orientation="horizontal"
        onResizeStart={handleTagResizeStart}
        onResize={handleTagResize}
        className="bg-surface-hover/50"
      />

      {/* Tags section */}
      <div
        ref={tagsSectionRef}
        className="border-t border-surface-hover flex flex-col shrink-0"
        style={tagHeight === null ? { maxHeight: TAG_SECTION_AUTO_MAX } : { height: tagHeight }}
      >
        <div className="flex items-center justify-between px-3 py-2 shrink-0">
          <span className="text-xs font-medium text-text-secondary/70 uppercase tracking-wider">标签</span>
          <button
            onClick={async () => {
              const name = await prompt('新建标签名称（用 # 分隔可建立层级，如 研究#阅读）', {
                title: '新建标签',
                placeholder: '例如：研究#阅读',
              });
              if (name?.trim()) createTag.mutate({ name: name.trim() });
            }}
            className="p-1 rounded text-text-secondary hover:text-text-primary hover:bg-surface-hover"
            title="新建标签"
          >
            <Plus size={14} />
          </button>
        </div>
        <div className="px-2 pb-2 overflow-y-auto flex-1 min-h-0">
          {activeTagIds.length > 0 && (
            <div className="flex items-center gap-1 px-2 pt-1.5 pb-2">
              <span className="text-[10px] text-text-secondary/60 mr-0.5">过滤</span>
              <div className="flex rounded-md border border-surface-hover overflow-hidden text-[10px]">
                <button
                  onClick={() => setTagFilterLogic('or')}
                  className={`px-1.5 py-0.5 transition-colors ${
                    activeTagLogic === 'or'
                      ? 'bg-primary/15 text-primary'
                      : 'text-text-secondary hover:text-text-primary'
                  }`}
                  title="任一标签命中即显示"
                >
                  任一 ∪
                </button>
                <button
                  onClick={() => setTagFilterLogic('and')}
                  className={`px-1.5 py-0.5 border-l border-surface-hover transition-colors ${
                    activeTagLogic === 'and'
                      ? 'bg-primary/15 text-primary'
                      : 'text-text-secondary hover:text-text-primary'
                  }`}
                  title="同时包含所有选中标签"
                >
                  全部 ∩
                </button>
              </div>
              <button
                onClick={() => {
                  activeTagIds.forEach((id) => toggleActiveTag(id));
                }}
                className="ml-auto px-1.5 py-0.5 rounded text-[10px] text-text-secondary/60 hover:text-text-primary hover:bg-surface-hover transition-colors"
                title="清除标签过滤"
              >
                清除
              </button>
            </div>
          )}
          {tagsLoading ? (
            <div className="px-3 py-2 text-xs text-text-secondary/60">加载中...</div>
          ) : tags?.length === 0 ? (
            <div className="px-3 py-2 text-xs text-text-secondary/60">暂无标签</div>
          ) : (
            <div className="flex flex-col gap-0.5 pt-1 px-1">
              {tagRoots.map((node) => renderTagNode(node, 0))}
            </div>
          )}
        </div>
      </div>

      {tagMenu && (
        <ContextMenu
          x={tagMenu.x}
          y={tagMenu.y}
          items={[
            {
              label: '重命名',
              icon: <Edit3 size={14} />,
              onClick: async () => {
                const name = await prompt('重命名标签（用 # 分隔可调整层级）', {
                  title: '重命名标签',
                  defaultValue: tagMenu.tag.name,
                });
                if (name?.trim() && name.trim() !== tagMenu.tag.name) {
                  updateTag.mutate({ id: tagMenu.tag.id, name: name.trim() });
                }
              },
            },
            {
              label: '修改颜色',
              icon: <Palette size={14} />,
              onClick: () => {
                setColorPickerTag(tagMenu.tag);
                setTagMenu(null);
              },
            },
            {
              label: '删除标签',
              icon: <Trash2 size={14} />,
              destructive: true,
              onClick: async () => {
                const ok = await confirm(`确认删除标签 "${tagMenu.tag.name}"？`, '删除标签');
                if (ok) {
                  deleteTag.mutate(tagMenu.tag.id);
                }
              },
            },
          ]}
          onClose={() => setTagMenu(null)}
        />
      )}

      {colorPickerTag && (
        <div
          className="fixed inset-0 z-[200] flex items-center justify-center"
          onClick={() => setColorPickerTag(null)}
        >
          <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" />
          <div
            className="relative bg-surface border border-surface-hover rounded-xl shadow-2xl p-4"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="text-sm font-medium text-text-primary mb-3">标签颜色</div>
            <div className="flex flex-wrap gap-2 max-w-[192px]">
              {TAG_COLORS.map((c) => (
                <button
                  key={c}
                  onClick={() => {
                    updateTag.mutate({ id: colorPickerTag.id, color: c });
                    setColorPickerTag(null);
                  }}
                  className={`w-7 h-7 rounded-full transition-transform hover:scale-110 ${
                    colorPickerTag.color === c
                      ? 'ring-2 ring-primary ring-offset-2 ring-offset-surface'
                      : ''
                  }`}
                  style={{ backgroundColor: c }}
                  title={c}
                />
              ))}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

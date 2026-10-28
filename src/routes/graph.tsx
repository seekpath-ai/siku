import { useState, useEffect, useRef, useMemo } from 'react';
import { createRoute, useNavigate } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { Loader2, Search, CircleDashed, X, ExternalLink, Crosshair, Undo2 } from 'lucide-react';
import ForceGraph2D from 'react-force-graph-2d';
import { graphGet, graphGetLocal, notesListAll } from '@/lib/tauri';
import { openNoteTab } from '@/lib/openNote';
import { useAllUnlockedSet } from '@/stores/noteLockStore';
import { computeHiddenSet } from '@/lib/noteLock';
import {
  GRAPH_NODE_TYPES,
  GRAPH_TYPE_LABELS,
  graphLinkAlpha,
  graphNodeColor,
  graphTextAlpha,
  useThemeVersion,
  type GraphNodeType,
} from '@/lib/graphColors';
import type { Note } from '@/lib/types';

interface GraphNode {
  id: string; label: string; node_type: string; color: string;
  x?: number; y?: number;
}
interface GraphEdge { source: string; target: string; edge_type: string; }
interface GraphData { nodes: GraphNode[]; links: GraphEdge[]; }

const LARGE_GRAPH_NODES = 2000;

function linkEndId(end: unknown): string {
  return typeof end === 'string' ? end : ((end as { id?: string })?.id ?? '');
}

function GraphPage() {
  const [data, setData] = useState<GraphData | null>(null);
  const [loading, setLoading] = useState(true);
  const containerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  const navigate = useNavigate();
  const themeVersion = useThemeVersion();

  const [query, setQuery] = useState('');
  const [hiddenTypes, setHiddenTypes] = useState<Set<string>>(new Set());
  // null = auto (hidden when the graph is large)
  const [showOrphans, setShowOrphans] = useState<boolean | null>(null);
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  // Single click selects a node → detail sidebar + persistent focus.
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // Double click drills into a local graph centered on that node.
  const [drill, setDrill] = useState<{ centerId: string; label: string; depth: number; data: GraphData | null } | null>(null);

  // Size the canvas from the real container (re-layouts on resize), which also
  // guarantees a non-zero canvas on first paint.
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const update = () => {
      const rect = el.getBoundingClientRect();
      setSize({ width: rect.width, height: rect.height });
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Reset any stray body cursor when leaving the graph page (onNodeHover sets
  // it on the container, but past versions leaked it to document.body).
  useEffect(() => {
    return () => {
      document.body.style.cursor = '';
      if (containerRef.current) {
        containerRef.current.style.cursor = '';
      }
    };
  }, []);

  useEffect(() => {
    (async () => {
      try {
        const raw = await graphGet() as { nodes: GraphNode[]; edges: GraphEdge[] };
        setData({ nodes: raw.nodes, links: raw.edges });
      } catch (err) { console.error(err); }
      finally { setLoading(false); }
    })();
  }, []);

  // Gated notes (subtrees of not-yet-unlocked lock roots) are dropped from
  // the graph together with their edges — node labels would leak their titles.
  const [allNotes, setAllNotes] = useState<Note[]>([]);
  useEffect(() => {
    notesListAll().then(setAllNotes).catch(() => {});
  }, []);
  const unlockedSet = useAllUnlockedSet();
  const hiddenIds = useMemo(() => computeHiddenSet(allNotes, unlockedSet), [allNotes, unlockedSet]);

  // Pipeline: locked-note filter → type filter → orphan filter. Drilled local
  // graphs flow through the same pipeline (locked notes stay hidden there too).
  const baseData = drill?.data ?? data;
  const visibleData = useMemo(() => {
    if (!baseData) return null;
    let nodes = baseData.nodes.filter((n) => !(n.node_type === 'note' && hiddenIds.has(n.id)));
    nodes = nodes.filter((n) => !hiddenTypes.has(n.node_type));
    let ids = new Set(nodes.map((n) => n.id));
    let links = baseData.links.filter((l) => ids.has(linkEndId(l.source)) && ids.has(linkEndId(l.target)));

    const large = nodes.length > LARGE_GRAPH_NODES;
    const orphansVisible = showOrphans ?? !large;
    if (!orphansVisible) {
      const degree = new Set<string>();
      for (const l of links) {
        degree.add(linkEndId(l.source));
        degree.add(linkEndId(l.target));
      }
      nodes = nodes.filter((n) => degree.has(n.id));
      ids = new Set(nodes.map((n) => n.id));
      links = links.filter((l) => ids.has(linkEndId(l.source)) && ids.has(linkEndId(l.target)));
    }
    return { nodes, links, large, orphansVisible };
  }, [baseData, hiddenIds, hiddenTypes, showOrphans]);

  // Canvas colors resolve CSS variables — re-resolved when the theme changes.
  const palette = useMemo(() => {
    const byType: Record<string, string> = {};
    for (const t of GRAPH_NODE_TYPES) byType[t] = graphNodeColor(t);
    return {
      byType,
      fallback: graphNodeColor('__other__'),
      label: graphTextAlpha(0.9),
      labelDim: graphTextAlpha(0.2),
      link: graphLinkAlpha(0.25),
      linkDim: graphLinkAlpha(0.08),
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [themeVersion]);

  const openNode = (n: GraphNode) => {
    if (n.node_type === 'note') {
      openNoteTab(navigate, { id: n.id, title: n.label });
    } else if (n.node_type === 'paper') {
      navigate({ to: '/reader/$paperId', params: { paperId: n.id } });
    } else if (n.node_type === 'knowledge_item') {
      navigate({ to: '/knowledge' });
    }
  };

  const drillInto = (n: GraphNode, depth = 1) => {
    setSelectedId(n.id);
    setDrill({ centerId: n.id, label: n.label, depth, data: null });
    graphGetLocal(n.id, depth)
      .then((raw) => setDrill({ centerId: n.id, label: n.label, depth, data: { nodes: raw.nodes, links: raw.edges } }))
      .catch((err) => {
        console.error(err);
        setDrill(null);
      });
  };

  const exitDrill = () => setDrill(null);

  // react-force-graph-2d has no onNodeDblClick — detect the second click of a
  // double-click on the same node ourselves.
  const lastClick = useRef<{ id: string; time: number }>({ id: '', time: 0 });
  const handleNodeClick = (node: unknown) => {
    const n = node as GraphNode;
    if (!n?.id) return;
    const now = Date.now();
    if (lastClick.current.id === n.id && now - lastClick.current.time < 350) {
      lastClick.current = { id: '', time: 0 };
      drillInto(n, drill?.depth ?? 1);
      return;
    }
    lastClick.current = { id: n.id, time: now };
    setSelectedId((prev) => (prev === n.id ? null : n.id));
  };

  // Give nodes deterministic initial positions (circular layout) so the first
  // paint has valid coordinates even before the force engine lays them out.
  const graphData = useMemo(() => {
    if (!visibleData) return null;
    const n = visibleData.nodes.length || 1;
    return {
      nodes: visibleData.nodes.map((node, i) => {
        if (node.x !== undefined && node.y !== undefined) return node;
        const angle = (i / n) * Math.PI * 2;
        const radius = 120 + (i % 5) * 40;
        return { ...node, x: Math.cos(angle) * radius, y: Math.sin(angle) * radius };
      }),
      links: visibleData.links,
    };
  }, [visibleData]);

  // Focus set: hovered node + its neighbors win; then the selected node; then
  // search matches. Everything outside the focus set fades — Obsidian-style.
  const neighborSet = (centerId: string | null): Set<string> | null => {
    if (!centerId || !visibleData) return null;
    const set = new Set<string>([centerId]);
    for (const l of visibleData.links) {
      const s = linkEndId(l.source);
      const t = linkEndId(l.target);
      if (s === centerId) set.add(t);
      if (t === centerId) set.add(s);
    }
    return set;
  };
  const hoverFocus = useMemo(() => neighborSet(hoveredId), [hoveredId, visibleData]); // eslint-disable-line react-hooks/exhaustive-deps
  const selectedFocus = useMemo(() => neighborSet(selectedId), [selectedId, visibleData]); // eslint-disable-line react-hooks/exhaustive-deps
  const selectedNode = useMemo(
    () => visibleData?.nodes.find((n) => n.id === selectedId) ?? null,
    [visibleData, selectedId]
  );
  const selectedNeighbors = useMemo(() => {
    if (!selectedId || !visibleData) return [];
    const byId = new Map(visibleData.nodes.map((n) => [n.id, n]));
    const out: GraphNode[] = [];
    for (const l of visibleData.links) {
      const s = linkEndId(l.source);
      const t = linkEndId(l.target);
      if (s === selectedId && byId.has(t)) out.push(byId.get(t)!);
      else if (t === selectedId && byId.has(s)) out.push(byId.get(s)!);
    }
    return out.sort((a, b) => a.label.localeCompare(b.label));
  }, [selectedId, visibleData]);

  const searchFocus = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q || !visibleData) return null;
    const set = new Set<string>();
    for (const n of visibleData.nodes) {
      if (n.label.toLowerCase().includes(q)) set.add(n.id);
    }
    return set;
  }, [query, visibleData]);

  const focusSet = hoverFocus ?? selectedFocus ?? searchFocus;
  const isFocused = (id: string) => !focusSet || focusSet.has(id);

  const toggleType = (t: GraphNodeType) => {
    setHiddenTypes((prev) => {
      const next = new Set(prev);
      if (next.has(t)) next.delete(t);
      else next.add(t);
      return next;
    });
  };

  return (
    <div className="h-full flex flex-col bg-background text-text-primary">
      {/* Toolbar */}
      <div className="h-11 flex items-center gap-2 px-3 border-b border-surface-hover shrink-0 flex-wrap">
        <div className="relative">
          <Search size={13} className="absolute left-2 top-1/2 -translate-y-1/2 text-text-secondary" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="搜索节点…"
            className="w-44 h-7 pl-7 pr-2 rounded-md bg-surface border border-surface-hover text-xs outline-none focus:border-primary placeholder:text-text-secondary"
          />
        </div>
        <div className="flex items-center gap-1">
          {GRAPH_NODE_TYPES.map((t) => {
            const off = hiddenTypes.has(t);
            return (
              <button
                key={t}
                onClick={() => toggleType(t)}
                className={`flex items-center gap-1.5 h-7 px-2 rounded-md text-xs transition-colors ${
                  off ? 'text-text-secondary opacity-50' : 'text-text-primary hover:bg-surface-hover'
                }`}
                title={off ? `显示${GRAPH_TYPE_LABELS[t]}` : `隐藏${GRAPH_TYPE_LABELS[t]}`}
              >
                <span
                  className="w-2 h-2 rounded-full"
                  style={{ backgroundColor: palette.byType[t] }}
                />
                {GRAPH_TYPE_LABELS[t]}
              </button>
            );
          })}
        </div>
        <button
          onClick={() => setShowOrphans((v) => !(v ?? !(visibleData?.large ?? false)))}
          className={`flex items-center gap-1.5 h-7 px-2 rounded-md text-xs transition-colors ${
            visibleData?.orphansVisible ? 'text-text-primary hover:bg-surface-hover' : 'text-text-secondary opacity-50'
          }`}
          title="显示/隐藏没有任何连线的节点"
        >
          <CircleDashed size={13} />
          孤立节点
        </button>
        {drill && (
          <div className="flex items-center gap-1 h-7 px-2 rounded-md text-xs bg-surface border border-primary/50 text-text-primary">
            <button onClick={exitDrill} className="flex items-center gap-1 hover:text-primary" title="返回全局图谱">
              <Undo2 size={12} />
              局部：{drill.label}
            </button>
            <span className="text-text-secondary mx-1">|</span>
            {[1, 2].map((d) => (
              <button
                key={d}
                onClick={() => drill.depth !== d && drillInto({ id: drill.centerId, label: drill.label } as GraphNode, d)}
                className={`px-1 rounded ${drill.depth === d ? 'text-primary font-medium' : 'text-text-secondary hover:text-text-primary'}`}
                title={`${d} 跳深度`}
              >
                {d}跳
              </button>
            ))}
          </div>
        )}
        {visibleData?.large && showOrphans === null && (
          <span className="text-xs text-text-secondary">
            节点较多，已自动隐藏孤立节点
          </span>
        )}
        {visibleData && (
          <span className="ml-auto text-xs text-text-secondary">
            {visibleData.nodes.length} 节点 · {visibleData.links.length} 连线
          </span>
        )}
      </div>

      {/* Canvas */}
      <div ref={containerRef} className="flex-1 min-h-0 relative">
        {loading || (drill && !drill.data) ? (
          <div className="flex items-center justify-center h-full">
            <Loader2 size={32} className="animate-spin text-text-secondary" />
          </div>
        ) : !visibleData?.nodes.length ? (
          <div className="flex flex-col items-center justify-center h-full text-text-secondary">
            <p className="text-lg">暂无图谱数据</p>
            <p className="text-sm mt-2">导入论文、创建笔记后会自动生成关联图谱</p>
          </div>
        ) : size.width > 0 && size.height > 0 && graphData ? (
          <>
            <ForceGraph2D
              width={size.width}
              height={size.height}
              graphData={graphData}
              nodeId="id"
              nodeLabel="label"
              nodeColor={(n) => palette.byType[(n as GraphNode).node_type] ?? palette.fallback}
              nodeVal={(n) => ((n as GraphNode).node_type === 'paper' ? 4 : 3) * (focusSet && !isFocused((n as GraphNode).id) ? 0.5 : 1)}
              linkDirectionalArrowLength={4}
              linkDirectionalArrowRelPos={1}
              linkColor={(l) => {
                const s = linkEndId(l.source);
                const t = linkEndId(l.target);
                if (focusSet && (focusSet.has(s) || focusSet.has(t))) return palette.link;
                return focusSet ? palette.linkDim : palette.link;
              }}
              backgroundColor="rgb(var(--background))"
              onNodeClick={handleNodeClick}
              onBackgroundClick={() => setSelectedId(null)}
              onNodeHover={(node) => {
                setHoveredId(((node as GraphNode)?.id) ?? null);
                const el = containerRef.current;
                if (el) {
                  el.style.cursor = node ? 'pointer' : '';
                }
              }}
              warmupTicks={10}
              cooldownTicks={30}
              d3AlphaDecay={0.02}
              d3VelocityDecay={0.3}
              nodeCanvasObjectMode={() => 'after'}
              nodeCanvasObject={(node, ctx, globalScale) => {
                const n = node as unknown as GraphNode;
                const label = n.label || n.id;
                const fontSize = 11 / globalScale;
                ctx.font = `${fontSize}px Inter, sans-serif`;
                ctx.textAlign = 'center';
                ctx.textBaseline = 'top';
                ctx.fillStyle = isFocused(n.id) ? palette.label : palette.labelDim;
                ctx.fillText(label, node.x!, node.y! + 6 / globalScale);
              }}
            />
            {/* Legend */}
            <div className="absolute left-3 bottom-3 rounded-lg bg-surface/80 backdrop-blur px-3 py-2 text-xs text-text-secondary space-y-1 pointer-events-none">
              {GRAPH_NODE_TYPES.map((t) => (
                <div key={t} className="flex items-center gap-2">
                  <span className="w-2 h-2 rounded-full" style={{ backgroundColor: palette.byType[t] }} />
                  {GRAPH_TYPE_LABELS[t]}
                </div>
              ))}
            </div>
            {/* Node detail sidebar */}
            {selectedNode && (
              <div className="absolute right-3 top-3 bottom-3 w-64 rounded-lg bg-surface/95 backdrop-blur border border-surface-hover flex flex-col overflow-hidden">
                <div className="flex items-start justify-between gap-2 px-3 py-2.5 border-b border-surface-hover">
                  <div className="min-w-0">
                    <div className="flex items-center gap-1.5 text-[11px] text-text-secondary">
                      <span
                        className="w-2 h-2 rounded-full shrink-0"
                        style={{ backgroundColor: palette.byType[selectedNode.node_type] ?? palette.fallback }}
                      />
                      {GRAPH_TYPE_LABELS[selectedNode.node_type as GraphNodeType] ?? selectedNode.node_type}
                    </div>
                    <div className="text-sm text-text-primary mt-0.5 break-words">{selectedNode.label}</div>
                  </div>
                  <button
                    onClick={() => setSelectedId(null)}
                    className="p-1 rounded text-text-secondary hover:bg-surface-hover hover:text-text-primary shrink-0"
                    title="取消选中"
                  >
                    <X size={13} />
                  </button>
                </div>
                <div className="flex items-center gap-1 px-3 py-2 border-b border-surface-hover">
                  {selectedNode.node_type !== 'tag' && (
                    <button
                      onClick={() => openNode(selectedNode)}
                      className="flex items-center gap-1 h-6 px-2 rounded-md text-xs bg-primary/15 text-primary hover:bg-primary/25"
                    >
                      <ExternalLink size={11} />
                      打开
                    </button>
                  )}
                  <button
                    onClick={() => drillInto(selectedNode, drill?.depth ?? 1)}
                    className="flex items-center gap-1 h-6 px-2 rounded-md text-xs bg-surface-hover text-text-primary hover:brightness-110"
                  >
                    <Crosshair size={11} />
                    以此为局部中心
                  </button>
                </div>
                <div className="flex-1 min-h-0 overflow-y-auto px-2 py-2">
                  <div className="text-[11px] text-text-secondary px-1 mb-1">
                    邻居（{selectedNeighbors.length}）
                  </div>
                  {selectedNeighbors.map((n) => (
                    <button
                      key={n.id}
                      onClick={() => setSelectedId(n.id)}
                      className="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-xs text-left text-text-primary hover:bg-surface-hover"
                      title={`${GRAPH_TYPE_LABELS[n.node_type as GraphNodeType] ?? n.node_type}：${n.label}`}
                    >
                      <span
                        className="w-2 h-2 rounded-full shrink-0"
                        style={{ backgroundColor: palette.byType[n.node_type] ?? palette.fallback }}
                      />
                      <span className="truncate">{n.label}</span>
                    </button>
                  ))}
                  {selectedNeighbors.length === 0 && (
                    <div className="text-xs text-text-secondary px-2 py-1">无关联节点</div>
                  )}
                </div>
              </div>
            )}
          </>
        ) : null}
      </div>
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/graph',
  component: GraphPage,
});

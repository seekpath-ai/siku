import { useState, useEffect, useRef, useMemo } from 'react';
import { createRoute, useNavigate } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { Loader2, Search, CircleDashed } from 'lucide-react';
import ForceGraph2D from 'react-force-graph-2d';
import { graphGet, notesListAll } from '@/lib/tauri';
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

  // Pipeline: locked-note filter → type filter → orphan filter.
  const visibleData = useMemo(() => {
    if (!data) return null;
    let nodes = data.nodes.filter((n) => !(n.node_type === 'note' && hiddenIds.has(n.id)));
    nodes = nodes.filter((n) => !hiddenTypes.has(n.node_type));
    let ids = new Set(nodes.map((n) => n.id));
    let links = data.links.filter((l) => ids.has(linkEndId(l.source)) && ids.has(linkEndId(l.target)));

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
  }, [data, hiddenIds, hiddenTypes, showOrphans]);

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

  const handleNodeClick = (node: unknown) => {
    const n = node as GraphNode;
    if (!n?.id) return;
    if (n.node_type === 'note') {
      openNoteTab(navigate, { id: n.id, title: n.label });
    } else if (n.node_type === 'paper' || n.node_type === 'tag') {
      navigate({ to: '/library' });
    } else if (n.node_type === 'knowledge_item') {
      navigate({ to: '/knowledge' });
    }
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

  // Focus set: hovered node + its neighbors win; otherwise search matches.
  // Everything outside the focus set fades — Obsidian-style behavior.
  const hoverFocus = useMemo(() => {
    if (!hoveredId || !visibleData) return null;
    const set = new Set<string>([hoveredId]);
    for (const l of visibleData.links) {
      const s = linkEndId(l.source);
      const t = linkEndId(l.target);
      if (s === hoveredId) set.add(t);
      if (t === hoveredId) set.add(s);
    }
    return set;
  }, [hoveredId, visibleData]);

  const searchFocus = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q || !visibleData) return null;
    const set = new Set<string>();
    for (const n of visibleData.nodes) {
      if (n.label.toLowerCase().includes(q)) set.add(n.id);
    }
    return set;
  }, [query, visibleData]);

  const focusSet = hoverFocus ?? searchFocus;
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
        {loading ? (
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

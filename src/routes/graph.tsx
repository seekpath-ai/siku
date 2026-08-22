import { useState, useEffect, useRef, useMemo } from 'react';
import { createRoute, useNavigate } from '@tanstack/react-router';
import { Route as RootRoute } from './__root';
import { Loader2 } from 'lucide-react';
import ForceGraph2D from 'react-force-graph-2d';
import { graphGet } from '@/lib/tauri';

interface GraphNode {
  id: string; label: string; node_type: string; color: string;
  x?: number; y?: number;
}
interface GraphEdge { source: string; target: string; edge_type: string; }
interface GraphData { nodes: GraphNode[]; links: GraphEdge[]; }

function GraphPage() {
  const [data, setData] = useState<GraphData | null>(null);
  const [loading, setLoading] = useState(true);
  const containerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });
  const navigate = useNavigate();

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

  const handleNodeClick = (node: unknown) => {
    const n = node as GraphNode;
    if (!n?.id) return;
    if (n.node_type === 'note') {
      navigate({ to: '/notes', search: { note: n.id } });
    } else if (n.node_type === 'paper' || n.node_type === 'tag') {
      navigate({ to: '/library' });
    } else if (n.node_type === 'knowledge_item') {
      navigate({ to: '/knowledge' });
    }
  };

  // Give nodes deterministic initial positions (circular layout) so the first
  // paint has valid coordinates even before the force engine lays them out.
  const graphData = useMemo(() => {
    if (!data) return null;
    const n = data.nodes.length || 1;
    return {
      nodes: data.nodes.map((node, i) => {
        if (node.x !== undefined && node.y !== undefined) return node;
        const angle = (i / n) * Math.PI * 2;
        const radius = 120 + (i % 5) * 40;
        return { ...node, x: Math.cos(angle) * radius, y: Math.sin(angle) * radius };
      }),
      links: data.links,
    };
  }, [data]);

  // Hover focus: the hovered node + its neighbors stay prominent, everything
  // else fades — Obsidian-style knowledge graph behavior.
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const neighbors = useMemo(() => {
    if (!hoveredId || !data) return new Set<string>();
    const set = new Set<string>();
    for (const l of data.links) {
      const link = l as { source: unknown; target: unknown };
      const s = typeof link.source === 'string'
        ? link.source
        : (link.source as { id?: string })?.id ?? '';
      const t = typeof link.target === 'string'
        ? link.target
        : (link.target as { id?: string })?.id ?? '';
      if (s === hoveredId) set.add(t);
      if (t === hoveredId) set.add(s);
    }
    return set;
  }, [hoveredId, data]);

  const nodeIdOf = (n: unknown) => ((n as GraphNode)?.id) ?? null;
  const isFocused = (id: string) => hoveredId === null || hoveredId === id || neighbors.has(id);

  return (
    <div ref={containerRef} className="h-full w-full">
      {loading ? (
        <div className="flex items-center justify-center h-full">
          <Loader2 size={32} className="animate-spin text-text-secondary" />
        </div>
      ) : !data?.nodes.length ? (
        <div className="flex flex-col items-center justify-center h-full text-text-secondary">
          <p className="text-lg">暂无图谱数据</p>
          <p className="text-sm mt-2">导入论文、创建笔记后会自动生成关联图谱</p>
        </div>
      ) : size.width > 0 && size.height > 0 && graphData ? (
        <ForceGraph2D
          width={size.width}
          height={size.height}
          graphData={graphData}
          nodeId="id"
          nodeLabel="label"
          nodeColor="color"
          nodeVal={(n) => (n as GraphNode).node_type === 'paper' ? 4 : 3}
          linkDirectionalArrowLength={4}
          linkDirectionalArrowRelPos={1}
          linkColor={(l) => {
            const link = l as { source: unknown; target: unknown };
            const s = typeof link.source === 'string'
              ? link.source
              : (link.source as { id?: string })?.id ?? '';
            const t = typeof link.target === 'string'
              ? link.target
              : (link.target as { id?: string })?.id ?? '';
            if (hoveredId && (s === hoveredId || t === hoveredId)) return 'rgba(230,126,34,0.6)';
            return hoveredId ? 'rgba(158,163,172,0.08)' : 'rgba(158,163,172,0.25)';
          }}
          backgroundColor="#1A1A1E"
          onNodeClick={handleNodeClick}
          onNodeHover={(node) => {
            setHoveredId(nodeIdOf(node));
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
            ctx.fillStyle = isFocused(n.id)
              ? 'rgba(236,237,240,0.85)'
              : 'rgba(236,237,240,0.2)';
            ctx.fillText(label, node.x!, node.y! + 6 / globalScale);
          }}
        />
      ) : null}
    </div>
  );
}

export const Route = createRoute({
  getParentRoute: () => RootRoute,
  path: '/graph',
  component: GraphPage,
});

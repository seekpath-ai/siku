import { useRef, useEffect, useState, useMemo } from 'react';
import ForceGraph2D from 'react-force-graph-2d';
import { X } from 'lucide-react';

interface GraphNode {
  id: string;
  label: string;
  node_type: string;
  color: string;
}

interface GraphEdge {
  source: string;
  target: string;
  edge_type: string;
}

interface ForceNode extends GraphNode {
  val: number;
}

interface Props {
  activeNoteId: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
  onNodeClick: (id: string) => void;
  onClose?: () => void;
}

export function GraphPanel({ activeNoteId, nodes, edges, onNodeClick, onClose }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ width: 0, height: 0 });

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

  const data = useMemo(() => {
    const nodeMap = new Map<string, GraphNode>();
    nodes.forEach((n) => nodeMap.set(n.id, n));
    return {
      nodes: nodes.map((n) => ({
        ...n,
        val: n.id === activeNoteId ? 8 : 5,
      })),
      links: edges
        .filter((e) => nodeMap.has(e.source) && nodeMap.has(e.target))
        .map((e) => ({
          source: e.source,
          target: e.target,
        })),
    };
  }, [nodes, edges, activeNoteId]);

  return (
    <div ref={containerRef} className="w-64 border-l border-surface-hover flex flex-col bg-background">
      <div className="flex items-center justify-between px-3 py-2 border-b border-surface-hover">
        <span className="text-xs font-medium text-text-secondary">局部关系图谱</span>
        {onClose && (
          <button
            onClick={onClose}
            className="p-1 rounded text-text-secondary hover:bg-surface-hover hover:text-text-primary"
            title="关闭"
          >
            <X size={12} />
          </button>
        )}
      </div>
      <div className="flex-1 min-h-0 relative">
        {size.width > 0 && size.height > 0 && (
          <ForceGraph2D
            width={size.width}
            height={size.height}
            graphData={data}
            nodeId="id"
            nodeLabel="label"
            nodeColor={(n: unknown) => ((n as ForceNode).color) || '#9CA3AF'}
            nodeVal={(n: unknown) => ((n as ForceNode).val) || 5}
            linkColor={() => 'rgba(158, 163, 172, 0.25)'}
            linkDirectionalArrowLength={4}
            linkDirectionalArrowRelPos={1}
            backgroundColor="transparent"
            onNodeClick={(node: unknown) => {
              const id = (node as ForceNode).id;
              if (id) onNodeClick(id);
            }}
            warmupTicks={10}
            cooldownTicks={30}
            d3AlphaDecay={0.02}
            d3VelocityDecay={0.3}
          />
        )}
      </div>
    </div>
  );
}

/**
 * Knowledge-graph canvas colors. Canvas fillStyle/strokeStyle cannot resolve
 * CSS var() references, so the theme triplets are read from getComputedStyle
 * and composed into rgb()/rgba() strings. useThemeVersion() re-renders the
 * consumer after a theme switch so these get re-resolved.
 */
import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';

export function useThemeVersion(): number {
  const [version, setVersion] = useState(0);
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    listen('app:settings_changed', () => setVersion((v) => v + 1)).then((f) => {
      if (disposed) f();
      else unlisten = f;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);
  return version;
}

function triplet(varName: string, fallback: string): string {
  if (typeof document === 'undefined') return fallback;
  const v = getComputedStyle(document.documentElement).getPropertyValue(varName).trim();
  return v || fallback;
}

function commas(rgbTriplet: string): string {
  return rgbTriplet.trim().split(/\s+/).join(',');
}

const NODE_TYPE_VARS: Record<string, string> = {
  paper: '--graph-paper',
  note: '--graph-note',
  tag: '--graph-tag',
  knowledge_item: '--graph-knowledge',
};

export function graphNodeColor(nodeType: string): string {
  const varName = NODE_TYPE_VARS[nodeType] ?? '--text-secondary';
  return `rgb(${commas(triplet(varName, '158 163 172'))})`;
}

export function graphCssColor(varName: string, fallback: string): string {
  return `rgb(${commas(triplet(varName, fallback))})`;
}

export function graphTextAlpha(alpha: number): string {
  return `rgba(${commas(triplet('--text-primary', '245 245 245'))},${alpha})`;
}

export function graphLinkAlpha(alpha: number): string {
  return `rgba(${commas(triplet('--text-secondary', '158 163 172'))},${alpha})`;
}

export const GRAPH_NODE_TYPES = ['paper', 'note', 'tag', 'knowledge_item'] as const;
export type GraphNodeType = (typeof GRAPH_NODE_TYPES)[number];

export const GRAPH_TYPE_LABELS: Record<GraphNodeType, string> = {
  paper: '论文',
  note: '笔记',
  tag: '标签',
  knowledge_item: '知识条目',
};

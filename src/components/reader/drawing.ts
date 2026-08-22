/**
 * Lightweight drawing annotation model for PDF pages.
 *
 * Coordinates are stored as ratios (0-1) relative to the page wrapper so that
 * strokes scale correctly with zoom and resize.
 */

export type DrawingTool = 'pen' | 'highlighter' | 'eraser';

export interface Point {
  xRatio: number;
  yRatio: number;
}

export interface Stroke {
  id: string;
  pageIndex: number;
  tool: 'pen' | 'highlighter';
  color: string;
  width: number;
  points: Point[];
}

export const DEFAULT_PEN_COLOR = '#ef4444';
export const DEFAULT_HIGHLIGHTER_COLOR = '#facc15';
export const DEFAULT_PEN_WIDTH = 2;
export const DEFAULT_HIGHLIGHTER_WIDTH = 12;

export const DRAWING_COLORS = [
  '#ef4444', // red
  '#f97316', // orange
  '#facc15', // yellow
  '#22c55e', // green
  '#3b82f6', // blue
  '#a855f7', // purple
  '#ec4899', // pink
  '#1f2937', // dark gray
];

/** Build an SVG path `d` attribute from normalized points and wrapper size. */
export function buildPathD(points: Point[], width: number, height: number): string {
  if (points.length === 0) return '';
  const toX = (p: Point) => p.xRatio * width;
  const toY = (p: Point) => p.yRatio * height;
  if (points.length === 1) {
    return `M ${toX(points[0])} ${toY(points[0])}`;
  }
  let d = `M ${toX(points[0])} ${toY(points[0])}`;
  for (let i = 1; i < points.length; i++) {
    d += ` L ${toX(points[i])} ${toY(points[i])}`;
  }
  return d;
}

/** Compute the distance from a point to a line segment, in ratio space. */
function distToSegment(px: number, py: number, a: Point, b: Point): number {
  const dx = b.xRatio - a.xRatio;
  const dy = b.yRatio - a.yRatio;
  if (dx === 0 && dy === 0) return Math.hypot(px - a.xRatio, py - a.yRatio);
  let t = ((px - a.xRatio) * dx + (py - a.yRatio) * dy) / (dx * dx + dy * dy);
  t = Math.max(0, Math.min(1, t));
  return Math.hypot(px - (a.xRatio + t * dx), py - (a.yRatio + t * dy));
}

/** Check whether an eraser path hits a stroke. Simple whole-stroke hit test. */
export function strokeHitByEraser(stroke: Stroke, eraserPoints: Point[], thresholdRatio = 0.015): boolean {
  if (eraserPoints.length === 0 || stroke.points.length === 0) return false;
  for (const ep of eraserPoints) {
    for (let i = 0; i < stroke.points.length - 1; i++) {
      if (distToSegment(ep.xRatio, ep.yRatio, stroke.points[i], stroke.points[i + 1]) < thresholdRatio) {
        return true;
      }
    }
  }
  return false;
}

/** Generate a short unique id. */
export function generateStrokeId(): string {
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
}

/** Eraser-shaped custom cursor (SVG data URI), hotspot at the eraser head. */
const ERASER_SVG = `<svg xmlns="http://www.w3.org/2000/svg" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="#374151" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="m7 21-4.3-4.3c-1-1-1-2.5 0-3.4l9.6-9.6c1-1 2.5-1 3.4 0l5.6 5.6c1 1 1 2.5 0 3.4L13 21" fill="#f9a8d4"/><path d="M22 21H7"/><path d="m5 11 9 9"/></svg>`;

export const ERASER_CURSOR = `url("data:image/svg+xml;utf8,${encodeURIComponent(ERASER_SVG)}") 6 20, cell`;

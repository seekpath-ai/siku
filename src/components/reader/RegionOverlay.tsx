import type { DetectedRegion } from './regions';
import { REGION_COLORS } from './regions';

/**
 * Create region overlay DOM elements inside a page wrapper.
 * Renders colored semi-transparent boxes with type labels.
 * Returns a cleanup function that removes the overlay elements.
 */
export function createRegionOverlays(
  wrapper: HTMLDivElement,
  regions: DetectedRegion[],
): () => void {
  cleanupRegionOverlays(wrapper);

  const container = document.createElement('div');
  container.className = 'region-overlays-container';
  container.style.cssText = 'position:absolute;inset:0;pointer-events:none;z-index:0;';

  const wRect = wrapper.getBoundingClientRect();

  for (const region of regions) {
    const colors = REGION_COLORS[region.type] ?? REGION_COLORS.unknown;
    const dashedBorder = region.type === 'figure' || region.type === 'table';
    const confidence = region.confidence ?? 0.5;

    // Main region box
    const box = document.createElement('div');
    box.className = 'region-overlay-box';
    box.style.cssText = `
      position: absolute;
      left: ${region.xRatio * wRect.width}px;
      top: ${region.yRatio * wRect.height}px;
      width: ${Math.max(region.widthRatio * wRect.width, 20)}px;
      height: ${Math.max(region.heightRatio * wRect.height, 12)}px;
      background: ${colors.bg};
      border: 1.5px ${dashedBorder ? 'dashed' : 'solid'} ${colors.border};
      border-radius: 3px;
      pointer-events: none;
      opacity: ${0.5 + confidence * 0.5};
    `;

    // Label badge
    const badge = document.createElement('span');
    badge.style.cssText = `
      position: absolute;
      top: -1px;
      left: 4px;
      transform: translateY(-100%);
      font-size: 10px;
      line-height: 1;
      padding: 1px 5px;
      border-radius: 3px 3px 0 0;
      color: ${colors.border};
      background: ${adjustAlpha(colors.border, 0.15)};
      white-space: nowrap;
      opacity: 0.85;
    `;
    badge.textContent = colors.label;
    box.appendChild(badge);

    container.appendChild(box);
  }

  wrapper.appendChild(container);

  return () => cleanupRegionOverlays(wrapper);
}

/** Remove all region overlay elements from a wrapper. */
export function cleanupRegionOverlays(wrapper: HTMLDivElement): void {
  const existing = wrapper.querySelector('.region-overlays-container');
  if (existing) existing.remove();
}

function adjustAlpha(rgba: string, alpha: number): string {
  return rgba.replace(/[\d.]+\)$/, `${alpha})`);
}

// frontend/src/lib/graph/geometry.ts

export interface FitTransform { tx: number; ty: number; scale: number; }

/** A node's center plus the half-extents of its rendered rectangle. */
export interface FitRect { x: number; y: number; halfW: number; halfH: number; }

/**
 * Compute a zoom transform that fits the nodes' rendered rectangles into a
 * width×height viewport, centered, clamped to maxScale.
 *
 * Matches the legacy MicroGraphCanvas autoFit()/autoFitSections() behavior
 * exactly. Legacy folds the per-node padding into the bounding box:
 *   minX = min(nx - nodeWidth/2 - 40);  maxX = max(nx + nodeWidth/2 + 40);
 *   minY = min(ny - nodeHeight/2 - 40); maxY = max(ny + nodeHeight/2 + 40);
 *   bw = maxX - minX; bh = maxY - minY;
 *   scale = Math.max(Math.min(sw/bw, sh/bh, 1.5) * 0.85, 0.45);
 *   cx = (minX+maxX)/2; cy = (minY+maxY)/2;
 *   tx = sw/2 - cx*scale; ty = sh/2 - cy*scale;
 * (call graph uses nodeWidth/nodeHeight; sections use sectionPillWidth/PILL_HEIGHT.)
 *
 * Here that is expressed as: bbox over the rectangle EXTENTS
 * (x ± halfW, y ± halfH), then the padded span adds 2*padding (the legacy
 * `40` on each side). Padding is symmetric, so the rect-bbox center equals
 * the legacy padded-bbox center.
 *
 * NOTE: This matches the LEGACY, not the plan's draft (which had no 0.85
 * factor, no 0.45 min-clamp, and treated nodes as points). Unit B computes
 * each FitRect's halfW/halfH from pillWidth(microNodeLabel(node)) /
 * PILL_HEIGHT|CLASS_CARD_HEIGHT|ghost dims — exactly as legacy
 * nodeWidth/nodeHeight did — keeping geometry type-agnostic.
 */
export function computeFitTransform(
  rects: FitRect[],
  width: number,
  height: number,
  padding = 40,
  maxScale = 1.5,
): FitTransform {
  if (rects.length === 0 || width <= 0 || height <= 0) {
    return { tx: 0, ty: 0, scale: 1 };
  }
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (const r of rects) {
    if (r.x - r.halfW < minX) minX = r.x - r.halfW;
    if (r.y - r.halfH < minY) minY = r.y - r.halfH;
    if (r.x + r.halfW > maxX) maxX = r.x + r.halfW;
    if (r.y + r.halfH > maxY) maxY = r.y + r.halfH;
  }
  // Legacy adds `padding` on each side → padded span = extent + 2*padding.
  const bw = Math.max(maxX - minX + padding * 2, 1);
  const bh = Math.max(maxY - minY + padding * 2, 1);
  const scale = Math.max(Math.min(width / bw, height / bh, maxScale) * 0.85, 0.45);
  const cx = (minX + maxX) / 2;
  const cy = (minY + maxY) / 2;
  const tx = width / 2 - cx * scale;
  const ty = height / 2 - cy * scale;
  return { tx, ty, scale };
}

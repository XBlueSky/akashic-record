import { describe, it, expect } from 'vitest';
import { computeFitTransform } from '../geometry';
import type { FitRect } from '../geometry';

const rect = (x: number, y: number, halfW = 50, halfH = 14): FitRect => ({ x, y, halfW, halfH });

describe('computeFitTransform', () => {
  it('empty rects → identity-ish', () => {
    expect(computeFitTransform([], 800, 600)).toEqual({ tx: 0, ty: 0, scale: 1 });
  });

  it('centers the bounding box in the viewport', () => {
    // symmetric box around origin → centered at viewport center
    const t = computeFitTransform([rect(-100, -100), rect(100, 100)], 800, 600, 40, 1.5);
    // center of box is (0,0) → tx = width/2, ty = height/2
    expect(t.tx).toBeCloseTo(400);
    expect(t.ty).toBeCloseTo(300);
    expect(t.scale).toBeGreaterThan(0);
    expect(t.scale).toBeLessThanOrEqual(1.5);
  });

  it('fits all node RECTANGLE EDGES inside the viewport after transform', () => {
    const rects = [rect(0, 0), rect(500, 300), rect(250, 800)];
    const w = 800, h = 600, pad = 40;
    const t = computeFitTransform(rects, w, h, pad, 1.5);
    for (const r of rects) {
      const left = (r.x - r.halfW) * t.scale + t.tx;
      const right = (r.x + r.halfW) * t.scale + t.tx;
      const top = (r.y - r.halfH) * t.scale + t.ty;
      const bottom = (r.y + r.halfH) * t.scale + t.ty;
      expect(left).toBeGreaterThanOrEqual(pad - 1);
      expect(right).toBeLessThanOrEqual(w - pad + 1);
      expect(top).toBeGreaterThanOrEqual(pad - 1);
      expect(bottom).toBeLessThanOrEqual(h - pad + 1);
    }
  });

  it('clamps to maxScale * 0.85 for a tiny box (legacy breathing-room factor)', () => {
    // Legacy: scale = Math.max(Math.min(..., maxScale) * 0.85, 0.45)
    // For a tiny box, min(...) hits maxScale (1.5), so scale = 1.5 * 0.85 = 1.275
    const t = computeFitTransform([rect(0, 0, 0.5, 0.5), rect(1, 1, 0.5, 0.5)], 800, 600, 40, 1.5);
    expect(t.scale).toBeCloseTo(1.275);
  });
});

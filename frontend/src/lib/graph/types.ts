// frontend/src/lib/graph/types.ts

// ── Node interfaces (SVG-based, no force simulation fields) ──

export interface MicroNode {
  id: string;
  name: string;
  chunk_type: string;
  signature?: string;
  line_count: number;
  x: number;
  y: number;
  vx?: number;
  vy?: number;
  fx?: number | null;
  fy?: number | null;
  inDegree: number;
  outDegree: number;
  isGhost?: boolean;
  ghostModuleId?: string;
  ghostModulePath?: string;
  direction?: 'caller' | 'callee';
}

export interface MicroLink {
  source: string;
  target: string;
  confidence: number;
  method: string;
  isExternal: boolean;
  curvature: number;
  isReversed?: boolean;
}

// ── Color system: "Astral Depths" ──

export interface ChunkColors {
  stroke: string;
  fill: string;
  textFill: string;
}

type ColorBucket = 'function' | 'class' | 'enum' | 'ghost' | 'section';

const CHUNK_TYPE_TO_BUCKET: Record<string, ColorBucket> = {
  function: 'function', method: 'function', macro: 'function',
  class: 'class', struct: 'class', type: 'class', trait: 'class', interface: 'class', component: 'class',
  enum: 'enum', constant: 'enum', config: 'enum',
  section: 'section',
};

export const ASTRAL_COLORS: Record<ColorBucket, ChunkColors> = {
  function: { stroke: '#2DD4BF', fill: '#0D3331', textFill: '#99F6E4' },
  class:    { stroke: '#FB7185', fill: '#2D0F1E', textFill: '#FECDD3' },
  enum:     { stroke: '#FBBF24', fill: '#2A1F05', textFill: '#FDE68A' },
  ghost:    { stroke: '#64748B', fill: '#1E293B', textFill: '#64748B' },
  section:  { stroke: '#67E8F9', fill: '#0E4155', textFill: '#CFFAFE' },
};

export const HOTSPOT_EDGE_COLOR = '#5EEAD4';

// ── Section tree depth colors (warm → cool) ──

export interface DepthColors {
  stroke: string;
  fill: string;
  textFill: string;
}

export const SECTION_DEPTH_COLORS: DepthColors[] = [
  { stroke: '#F472B6', fill: '#3D1428', textFill: '#FECDD3' },
  { stroke: '#2DD4BF', fill: '#0D3331', textFill: '#99F6E4' },
  { stroke: '#60A5FA', fill: '#172554', textFill: '#BFDBFE' },
  { stroke: '#A78BFA', fill: '#1E1B4B', textFill: '#DDD6FE' },
];

export const EXPLAINS_COLORS: ChunkColors = {
  stroke: '#FBBF24', fill: '#2A1F05', textFill: '#FDE68A',
};

// ── Helpers ──

export const CLASS_TYPES = new Set(['class', 'struct', 'type', 'trait', 'interface', 'component']);
export const ENUM_TYPES = new Set(['enum', 'constant', 'config']);

export function isClassType(chunkType: string): boolean {
  return CLASS_TYPES.has(chunkType);
}

export function isEnumType(chunkType: string): boolean {
  return ENUM_TYPES.has(chunkType);
}

/** Map a bare chunk_type string → ColorBucket (no ghost check; use getColorBucket for MicroNode). */
export function chunkTypeBucket(chunkType: string): ColorBucket {
  return CHUNK_TYPE_TO_BUCKET[chunkType] ?? 'function';
}

export function getColorBucket(node: MicroNode): ColorBucket {
  if (node.isGhost) return 'ghost';
  return chunkTypeBucket(node.chunk_type);
}

export function getChunkColors(node: MicroNode): ChunkColors {
  return ASTRAL_COLORS[getColorBucket(node)];
}

export function getSectionColors(depth: number, _hasExplains: boolean): ChunkColors {
  // Always use depth color — explains is indicated by badge + glow, not node color
  return SECTION_DEPTH_COLORS[Math.min(depth, SECTION_DEPTH_COLORS.length - 1)];
}

export function microNodeLabel(node: MicroNode, limit = 36): string {
  if (node.isGhost) {
    const modName = node.ghostModulePath?.split('/').pop() || '?';
    const full = `${modName}/${node.name}`;
    return full.length > limit ? full.substring(0, limit) + '…' : full;
  }
  return node.name.length > limit ? node.name.substring(0, limit) + '…' : node.name;
}

export function pillWidth(label: string, minWidth = 56): number {
  return Math.max(label.length * 7 + 20, minWidth);
}

export const PILL_HEIGHT = 28;
export const CLASS_CARD_HEIGHT = 46;
export const GHOST_PILL_WIDTH = 60;
export const GHOST_PILL_HEIGHT = 24;

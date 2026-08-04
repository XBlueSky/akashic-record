/**
 * constellation.ts — pure, framework-agnostic geometry for the landing scene.
 *
 * All math is ported verbatim from
 *   .legacy/src/components/views/LandingView.svelte
 * with no Three.js / Threlte / Svelte imports so the functions are unit-testable.
 *
 * Coordinate system: right-handed, Y-up, same as Three.js.
 */

// ---------------------------------------------------------------------------
// Constants (matches legacy defaults)
// ---------------------------------------------------------------------------

export const DEFAULT_PARTICLE_COUNT = 120;
export const DEFAULT_SPHERE_RADIUS = 14;
export const DEFAULT_DRIFT_SPEED = 0.004;
export const DEFAULT_CONNECTION_DIST = 7;
export const DEFAULT_MAX_CONNECTIONS = 50;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface Vec3 {
  x: number;
  y: number;
  z: number;
}

export interface ParticleData {
  x: number;
  y: number;
  z: number;
  vx: number;
  vy: number;
  vz: number;
  /** Visual radius (world units). */
  radius: number;
  colorIdx: number;
  /** Phase offset for pulsing animation (radians). */
  phase: number;
}

export interface EdgeCandidate {
  a: number;
  b: number;
  dist: number;
}

export interface Connection {
  a: number;
  b: number;
  phase: number;
  age: number;
  lifespan: number;
}

export interface ConstellationOpts {
  particleCount?: number;
  sphereRadius?: number;
  driftSpeed?: number;
  connectionDist?: number;
  maxConnections?: number;
  colorCount?: number;
}

export interface Constellation {
  particles: ParticleData[];
  connections: Connection[];
}

// ---------------------------------------------------------------------------
// Particle generation (contains Math.random — not deterministic)
// ---------------------------------------------------------------------------

/**
 * Generates `count` particles randomly distributed inside a sphere of
 * `sphereRadius`.  Position and velocity logic ported verbatim from legacy.
 */
export function buildParticles(
  count: number = DEFAULT_PARTICLE_COUNT,
  sphereRadius: number = DEFAULT_SPHERE_RADIUS,
  driftSpeed: number = DEFAULT_DRIFT_SPEED,
  colorCount: number = 5,
): ParticleData[] {
  const particles: ParticleData[] = [];

  for (let i = 0; i < count; i++) {
    // Rejection-sample a point inside the unit sphere, then scale.
    let x: number, y: number, z: number;
    do {
      x = (Math.random() - 0.5) * 2;
      y = (Math.random() - 0.5) * 2;
      z = (Math.random() - 0.5) * 2;
    } while (x * x + y * y + z * z > 1);

    // Size distribution: mostly small, some medium, a few large — verbatim.
    const sizeRoll = Math.random();
    let radius: number;
    if (sizeRoll < 0.6) {
      radius = 0.06 + Math.random() * 0.06;       // small
    } else if (sizeRoll < 0.9) {
      radius = 0.12 + Math.random() * 0.1;        // medium
    } else {
      radius = 0.22 + Math.random() * 0.12;       // large "stars"
    }

    particles.push({
      x: x * sphereRadius,
      y: y * sphereRadius,
      z: z * sphereRadius,
      vx: (Math.random() - 0.5) * driftSpeed * 2,
      vy: (Math.random() - 0.5) * driftSpeed * 2,
      vz: (Math.random() - 0.5) * driftSpeed * 2,
      radius,
      colorIdx: Math.floor(Math.random() * colorCount),
      phase: Math.random() * Math.PI * 2,
    });
  }

  return particles;
}

// ---------------------------------------------------------------------------
// Edge / connection computation (DETERMINISTIC given positions)
// ---------------------------------------------------------------------------

/**
 * Returns Euclidean distance between two Vec3 points.
 */
export function euclideanDist(a: Vec3, b: Vec3): number {
  const dx = a.x - b.x;
  const dy = a.y - b.y;
  const dz = a.z - b.z;
  return Math.sqrt(dx * dx + dy * dy + dz * dz);
}

/**
 * Finds all pairs of positions within `connectionDist` of each other,
 * excluding pairs already in `existingPairs` (formatted as "a-b" with a < b).
 * Returned candidates are sorted by distance (nearest first).
 *
 * This is the deterministic half of `buildNewConnections` from the legacy
 * LandingView.  Extracted so it can be unit-tested with injected positions.
 *
 * @param positions  Array of 3-D positions (any object with x,y,z).
 * @param connectionDist  Distance threshold; pairs beyond this are ignored.
 * @param maxSlots  Maximum number of candidates to return.
 *   IMPORTANT: callers MUST pass the number of *available* slots, not the
 *   full capacity.  In the animation loop this is `MAX_CONNECTIONS -
 *   connections.length`.  Passing the full cap can cause `connections` to
 *   exceed the Float32Array size allocated for `MAX_CONNECTIONS * 6` vertices
 *   and overflow the buffer.
 * @param existingPairs  Set of "a-b" strings (a < b) to skip.
 */
export function computeEdges(
  positions: Vec3[],
  connectionDist: number = DEFAULT_CONNECTION_DIST,
  maxSlots: number = DEFAULT_MAX_CONNECTIONS,
  existingPairs: Set<string> = new Set(),
): EdgeCandidate[] {
  const n = positions.length;
  const candidates: EdgeCandidate[] = [];

  for (let i = 0; i < n; i++) {
    for (let j = i + 1; j < n; j++) {
      if (existingPairs.has(`${i}-${j}`)) continue;
      const dist = euclideanDist(positions[i], positions[j]);
      if (dist < connectionDist) {
        candidates.push({ a: i, b: j, dist });
      }
    }
  }

  candidates.sort((a, b) => a.dist - b.dist);
  return candidates.slice(0, maxSlots);
}

/**
 * Converts `EdgeCandidate[]` into full `Connection[]` objects with random
 * phase / lifespan (same logic as the legacy `buildNewConnections` return).
 * The random fields are intentionally non-deterministic (animation flavour).
 */
export function edgesToConnections(candidates: EdgeCandidate[]): Connection[] {
  return candidates.map((c) => ({
    a: c.a,
    b: c.b,
    phase: Math.random() * Math.PI * 2,
    age: 0,
    lifespan: 3 + Math.random() * 5,
  }));
}

// ---------------------------------------------------------------------------
// Top-level convenience builder
// ---------------------------------------------------------------------------

/**
 * Builds a full constellation: randomised particles + initial connections.
 * Use `computeEdges` + `edgesToConnections` directly in tests where you need
 * injected positions.
 */
export function buildConstellation(opts: ConstellationOpts = {}): Constellation {
  const {
    particleCount = DEFAULT_PARTICLE_COUNT,
    sphereRadius = DEFAULT_SPHERE_RADIUS,
    driftSpeed = DEFAULT_DRIFT_SPEED,
    connectionDist = DEFAULT_CONNECTION_DIST,
    maxConnections = DEFAULT_MAX_CONNECTIONS,
    colorCount = 5,
  } = opts;

  const particles = buildParticles(particleCount, sphereRadius, driftSpeed, colorCount);
  const candidates = computeEdges(particles, connectionDist, maxConnections);
  const connections = edgesToConnections(candidates);

  return { particles, connections };
}

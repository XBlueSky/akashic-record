<script lang="ts">
	import { onMount, onDestroy } from "svelte";
	import { fly } from "svelte/transition";
	import { Canvas, T } from "@threlte/core";
	import * as THREE from "three";
	import { fetchRepos, isAuthError } from "$lib/api";
	import type { Repository } from "$lib/types";
	import { buildParticles, computeEdges, edgesToConnections } from "$lib/landing/constellation";
	import { t } from "svelte-i18n";

	// ---------------------------------------------------------------------------
	// Props
	// ---------------------------------------------------------------------------

	let { onselect }: { onselect?: (name: string) => void } = $props();

	// ---------------------------------------------------------------------------
	// UI state
	// ---------------------------------------------------------------------------

	let repos: Repository[] = $state([]);
	let loading = $state(true);
	let ready = $state(false); // controls overlay entrance

	// ---------------------------------------------------------------------------
	// Scene constants (verbatim from legacy)
	// ---------------------------------------------------------------------------

	const PARTICLE_COUNT = 120;
	const SPHERE_RADIUS = 14;
	const DRIFT_SPEED = 0.004;
	const MAX_CONNECTIONS = 50;
	const CONNECTION_DIST = 7;

	const COLORS = [
		new THREE.Color("#3b82f6"), // blue
		new THREE.Color("#6366f1"), // indigo
		new THREE.Color("#8b5cf6"), // violet
		new THREE.Color("#06b6d4"), // cyan
		new THREE.Color("#a78bfa"), // light purple
	];

	// ---------------------------------------------------------------------------
	// Particles — InstancedMesh (actual 3D spheres with emissive glow)
	// Uses buildParticles() from constellation lib instead of inline generation.
	// ---------------------------------------------------------------------------

	const particles = buildParticles(PARTICLE_COUNT, SPHERE_RADIUS, DRIFT_SPEED, COLORS.length);

	const particleGeometry = new THREE.SphereGeometry(1, 12, 12);
	const particleMaterial = new THREE.MeshStandardMaterial({
		emissive: "#6366f1",
		emissiveIntensity: 0.6,
		color: "#1e1b4b",
		transparent: true,
		opacity: 0.9,
		roughness: 0.3,
		metalness: 0.1,
	});

	const instancedMesh = new THREE.InstancedMesh(particleGeometry, particleMaterial, PARTICLE_COUNT);
	instancedMesh.instanceMatrix.setUsage(THREE.DynamicDrawUsage);

	// Set initial transforms and per-instance colors
	const tmpMatrix = new THREE.Matrix4();
	for (let i = 0; i < PARTICLE_COUNT; i++) {
		const p = particles[i];
		tmpMatrix.makeScale(p.radius, p.radius, p.radius);
		tmpMatrix.setPosition(p.x, p.y, p.z);
		instancedMesh.setMatrixAt(i, tmpMatrix);
		instancedMesh.setColorAt(i, COLORS[p.colorIdx]);
	}
	instancedMesh.instanceMatrix.needsUpdate = true;
	if (instancedMesh.instanceColor) instancedMesh.instanceColor.needsUpdate = true;

	// ---------------------------------------------------------------------------
	// Central core orb — pulsing energy sphere
	// ---------------------------------------------------------------------------

	let coreRef = $state<THREE.Mesh | undefined>(undefined);
	const coreMaterial = new THREE.MeshStandardMaterial({
		color: "#1e3a5f",
		emissive: "#3b82f6",
		emissiveIntensity: 1.2,
		transparent: true,
		opacity: 0.4,
		roughness: 0.1,
		metalness: 0.5,
	});

	let coreInnerRef = $state<THREE.Mesh | undefined>(undefined);
	const coreInnerMaterial = new THREE.MeshBasicMaterial({
		color: "#93c5fd",
		transparent: true,
		opacity: 0.7,
	});

	// ---------------------------------------------------------------------------
	// Orbital rings — wireframe torus outlines
	// ---------------------------------------------------------------------------

	const ring1Geometry = new THREE.TorusGeometry(8, 0.02, 8, 100);
	const ring2Geometry = new THREE.TorusGeometry(11, 0.015, 8, 120);
	const ring3Geometry = new THREE.TorusGeometry(6, 0.015, 8, 80);

	const ringMaterial = new THREE.MeshBasicMaterial({
		color: "#3b82f6",
		transparent: true,
		opacity: 0.15,
	});
	const ringMaterial2 = new THREE.MeshBasicMaterial({
		color: "#8b5cf6",
		transparent: true,
		opacity: 0.1,
	});

	let ring1Ref = $state<THREE.Mesh | undefined>(undefined);
	let ring2Ref = $state<THREE.Mesh | undefined>(undefined);
	let ring3Ref = $state<THREE.Mesh | undefined>(undefined);

	// ---------------------------------------------------------------------------
	// Connections
	// Uses computeEdges() + edgesToConnections() from constellation lib.
	// F-1: animation loop passes MAX_CONNECTIONS - connections.length (available
	//      slots) — not the full MAX_CONNECTIONS cap.
	// ---------------------------------------------------------------------------

	type Connection = {
		a: number;
		b: number;
		phase: number;
		age: number;
		lifespan: number;
	};

	let connections: Connection[] = edgesToConnections(
		computeEdges(particles, CONNECTION_DIST, MAX_CONNECTIONS, new Set()),
	);

	// Line geometry for connections
	const linePositions = new Float32Array(MAX_CONNECTIONS * 6);
	const lineColors = new Float32Array(MAX_CONNECTIONS * 6);
	const lineGeometry = new THREE.BufferGeometry();
	lineGeometry.setAttribute("position", new THREE.BufferAttribute(linePositions, 3));
	lineGeometry.setAttribute("color", new THREE.BufferAttribute(lineColors, 3));
	lineGeometry.setDrawRange(0, 0);

	const lineMaterial = new THREE.LineBasicMaterial({
		vertexColors: true,
		transparent: true,
		opacity: 1.0,
		depthWrite: false,
	});

	const lineBaseColor = new THREE.Color("#6366f1");

	// ---------------------------------------------------------------------------
	// Scene pivot (rotate whole scene instead of orbiting camera)
	// ---------------------------------------------------------------------------

	let pivotRef = $state<THREE.Group | undefined>(undefined);

	// ---------------------------------------------------------------------------
	// Camera + mouse parallax
	// ---------------------------------------------------------------------------

	let cameraRef = $state<THREE.PerspectiveCamera | undefined>(undefined);

	let mouseTarget = { x: 0, y: 0 };
	let mouseCurrent = { x: 0, y: 0 };
	const MOUSE_INFLUENCE = 3;
	const MOUSE_SMOOTH = 0.04;

	function onMouseMove(e: MouseEvent) {
		mouseTarget.x = (e.clientX / window.innerWidth) * 2 - 1;
		mouseTarget.y = (e.clientY / window.innerHeight) * 2 - 1;
	}

	// ---------------------------------------------------------------------------
	// WebGL availability guard
	// ---------------------------------------------------------------------------

	let webglAvailable = $state(true);

	function detectWebGL(): boolean {
		try {
			const canvas = document.createElement("canvas");
			const ctx =
				canvas.getContext("webgl2") ??
				canvas.getContext("webgl") ??
				canvas.getContext("experimental-webgl");
			return ctx !== null;
		} catch {
			return false;
		}
	}

	// ---------------------------------------------------------------------------
	// Animation loop
	// ---------------------------------------------------------------------------

	let rafId: number;
	let lastTime = 0;
	let elapsed = 0;

	function animate(now: number) {
		if (lastTime === 0) lastTime = now;
		const delta = Math.min((now - lastTime) / 1000, 0.1);
		lastTime = now;
		elapsed += delta;

		// --- Drift particles & update InstancedMesh ---
		for (let i = 0; i < PARTICLE_COUNT; i++) {
			const p = particles[i];
			p.x += p.vx;
			p.y += p.vy;
			p.z += p.vz;

			// Bounce off sphere boundary
			const dist = Math.sqrt(p.x * p.x + p.y * p.y + p.z * p.z);
			if (dist > SPHERE_RADIUS) {
				const nx = p.x / dist,
					ny = p.y / dist,
					nz = p.z / dist;
				const dot = p.vx * nx + p.vy * ny + p.vz * nz;
				p.vx -= 2 * dot * nx;
				p.vy -= 2 * dot * ny;
				p.vz -= 2 * dot * nz;
				const s = SPHERE_RADIUS / dist;
				p.x *= s;
				p.y *= s;
				p.z *= s;
			}

			// Pulsing scale for larger particles
			const pulse = p.radius > 0.15 ? 1 + 0.15 * Math.sin(elapsed * 1.5 + p.phase) : 1;
			const r = p.radius * pulse;

			tmpMatrix.makeScale(r, r, r);
			tmpMatrix.setPosition(p.x, p.y, p.z);
			instancedMesh.setMatrixAt(i, tmpMatrix);
		}
		instancedMesh.instanceMatrix.needsUpdate = true;

		// --- Pulse central core ---
		if (coreRef) {
			const coreScale = 1.8 + 0.3 * Math.sin(elapsed * 0.8);
			coreRef.scale.setScalar(coreScale);
			(coreRef.material as THREE.MeshStandardMaterial).emissiveIntensity =
				1.0 + 0.4 * Math.sin(elapsed * 1.2);
		}
		if (coreInnerRef) {
			const innerScale = 0.8 + 0.15 * Math.sin(elapsed * 1.5 + 1);
			coreInnerRef.scale.setScalar(innerScale);
		}

		// --- Rotate orbital rings ---
		if (ring1Ref) {
			ring1Ref.rotation.x = Math.PI * 0.5 + elapsed * 0.05;
			ring1Ref.rotation.y = elapsed * 0.03;
		}
		if (ring2Ref) {
			ring2Ref.rotation.x = Math.PI * 0.35;
			ring2Ref.rotation.z = elapsed * 0.04;
		}
		if (ring3Ref) {
			ring3Ref.rotation.x = Math.PI * 0.7;
			ring3Ref.rotation.y = -elapsed * 0.06;
		}

		// --- Update connections (age + expire + refill) ---
		for (let i = connections.length - 1; i >= 0; i--) {
			connections[i].age += delta;
			if (connections[i].age >= connections[i].lifespan) {
				connections.splice(i, 1);
			}
		}
		if (connections.length < MAX_CONNECTIONS * 0.6) {
			// F-1: pass AVAILABLE slots (MAX_CONNECTIONS - connections.length), not
			//      the full MAX_CONNECTIONS cap, to prevent buffer overflow.
			const available = MAX_CONNECTIONS - connections.length;
			const existingPairs = new Set(connections.map((c) => `${c.a}-${c.b}`));
			const newEdges = computeEdges(particles, CONNECTION_DIST, available, existingPairs);
			connections.push(...edgesToConnections(newEdges));
		}

		// Update line geometry
		const linePosAttr = lineGeometry.getAttribute("position") as THREE.BufferAttribute;
		const lineColAttr = lineGeometry.getAttribute("color") as THREE.BufferAttribute;
		let vi = 0;
		for (const conn of connections) {
			const pa = particles[conn.a];
			const pb = particles[conn.b];

			const pulse = 0.2 + 0.4 * (0.5 + 0.5 * Math.sin(elapsed * 2 + conn.phase));
			const fadeIn = Math.min(conn.age / 0.8, 1.0);
			const fadeOut = Math.min((conn.lifespan - conn.age) / 0.8, 1.0);
			const alpha = pulse * fadeIn * fadeOut;

			const cr = lineBaseColor.r * alpha;
			const cg = lineBaseColor.g * alpha;
			const cb = lineBaseColor.b * alpha;

			linePosAttr.array[vi * 3] = pa.x;
			linePosAttr.array[vi * 3 + 1] = pa.y;
			linePosAttr.array[vi * 3 + 2] = pa.z;
			lineColAttr.array[vi * 3] = cr;
			lineColAttr.array[vi * 3 + 1] = cg;
			lineColAttr.array[vi * 3 + 2] = cb;
			vi++;
			linePosAttr.array[vi * 3] = pb.x;
			linePosAttr.array[vi * 3 + 1] = pb.y;
			linePosAttr.array[vi * 3 + 2] = pb.z;
			lineColAttr.array[vi * 3] = cr;
			lineColAttr.array[vi * 3 + 1] = cg;
			lineColAttr.array[vi * 3 + 2] = cb;
			vi++;
		}
		linePosAttr.needsUpdate = true;
		lineColAttr.needsUpdate = true;
		lineGeometry.setDrawRange(0, vi);

		// --- Rotate scene pivot ---
		if (pivotRef) {
			pivotRef.rotation.y += 0.06 * delta;
			pivotRef.rotation.x = Math.sin(elapsed * 0.15) * 0.08;
		}

		// --- Mouse parallax on camera ---
		mouseCurrent.x += (mouseTarget.x - mouseCurrent.x) * MOUSE_SMOOTH;
		mouseCurrent.y += (mouseTarget.y - mouseCurrent.y) * MOUSE_SMOOTH;
		if (cameraRef) {
			cameraRef.position.x = -mouseCurrent.x * MOUSE_INFLUENCE;
			cameraRef.position.y = mouseCurrent.y * MOUSE_INFLUENCE;
			cameraRef.lookAt(0, 0, 0);
		}

		rafId = requestAnimationFrame(animate);
	}

	onMount(async () => {
		webglAvailable = detectWebGL();
		if (webglAvailable) {
			rafId = requestAnimationFrame(animate);
		}

		try {
			repos = await fetchRepos();
		} catch (err) {
			if (!isAuthError(err)) {
				// Landing is anonymous-browsable; auth errors are silent (repos = []).
				// Unexpected errors also silently degrade to empty list.
			}
			repos = [];
		} finally {
			loading = false;
		}

		// Entrance animation: show overlay after brief delay.
		// GSAP evaluation: legacy used gsap.fromTo(panelEl, {opacity,y,scale})
		// — a pure CSS tween, no 3D/camera interaction. Replaced with Svelte
		// `fly` transition. No gsap dependency needed.
		setTimeout(() => {
			ready = true;
		}, 400);
	});

	onDestroy(() => {
		if (rafId) cancelAnimationFrame(rafId);
		particleGeometry.dispose();
		particleMaterial.dispose();
		lineGeometry.dispose();
		lineMaterial.dispose();
		ring1Geometry.dispose();
		ring2Geometry.dispose();
		ring3Geometry.dispose();
		ringMaterial.dispose();
		ringMaterial2.dispose();
		coreMaterial.dispose();
		coreInnerMaterial.dispose();
	});
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="relative w-full h-full overflow-hidden" onmousemove={onMouseMove}>
	<!-- 3D Canvas (guarded: if WebGL unavailable the 2D overlay still renders) -->
	{#if webglAvailable}
		<div class="absolute inset-0 w-full h-full">
			<Canvas renderMode="always">
				<T.PerspectiveCamera makeDefault position={[0, 0, 32]} fov={55} bind:ref={cameraRef} />

				<!-- Lighting (outside pivot so it stays fixed) -->
				<T.AmbientLight intensity={0.15} />
				<T.PointLight position={[15, 15, 15]} intensity={0.8} color="#3b82f6" />
				<T.PointLight position={[-10, -8, -12]} intensity={0.3} color="#8b5cf6" />

				<!-- Fog for depth -->
				<T.Fog color="#040608" near={20} far={55} attach="fog" />

				<!-- Rotating scene pivot -->
				<T.Group bind:ref={pivotRef}>
					<!-- Central core orb -->
					<T.Mesh bind:ref={coreRef} material={coreMaterial}>
						<T.SphereGeometry args={[1, 32, 32]} />
					</T.Mesh>
					<T.Mesh bind:ref={coreInnerRef} material={coreInnerMaterial}>
						<T.SphereGeometry args={[1, 24, 24]} />
					</T.Mesh>

					<!-- Orbital rings -->
					<T.Mesh bind:ref={ring1Ref} geometry={ring1Geometry} material={ringMaterial} />
					<T.Mesh bind:ref={ring2Ref} geometry={ring2Geometry} material={ringMaterial2} />
					<T.Mesh bind:ref={ring3Ref} geometry={ring3Geometry} material={ringMaterial2} />

					<!-- Particles (InstancedMesh — real 3D spheres) -->
					<T is={instancedMesh} />

					<!-- Connections -->
					<T.LineSegments geometry={lineGeometry} material={lineMaterial} />
				</T.Group>
			</Canvas>
		</div>
	{/if}

	<!-- Glass overlay (always rendered; sits above the 3D canvas or alone) -->
	<div class="absolute inset-0 flex items-center justify-center pointer-events-none z-[1]">
		{#if ready}
			<div class="overlay-content" in:fly={{ y: 30, duration: 800, opacity: 0 }}>
				<!-- Corner decorations -->
				<div class="corner-tl"></div>
				<div class="corner-br"></div>

				<!-- Decorative micro text -->
				<span class="micro-text top-left">{$t("landing.sysLabel")}</span>
				<span class="micro-text top-right">{$t("landing.version")}</span>

				<h1 class="landing-title">{$t("landing.title")}</h1>
				<p class="font-mono text-xs tracking-wide text-muted-foreground mb-9">
					{$t("landing.subtitle")}
				</p>

				{#if loading}
					<p class="text-muted-foreground font-mono text-xs tracking-wide">
						{$t("landing.scanning")}
					</p>
				{:else if repos.length === 0}
					<p class="text-muted-foreground font-mono text-xs tracking-wide">
						{$t("landing.noRepos")}
					</p>
				{:else}
					<div class="grid grid-cols-[repeat(auto-fill,minmax(200px,1fr))] gap-2.5">
						{#each repos as repo, i (repo.id)}
							<button
								class="repo-card"
								onclick={() => onselect?.(repo.name)}
								in:fly={{ y: 12, delay: 100 + i * 80, duration: 400 }}
							>
								<span class="repo-bracket left">[</span>
								<span
									class="w-1.5 h-1.5 rounded-full bg-primary shrink-0 shadow-[0_0_8px_var(--color-primary)]"
								></span>
								<span class="font-mono text-sm tracking-tight">{repo.name}</span>
								<span class="repo-bracket right">]</span>
							</button>
						{/each}
					</div>
				{/if}

				<span class="micro-text bottom-center">{$t("landing.footer")}</span>
			</div>
		{/if}
	</div>
</div>

<style>
	/* Canvas brightness/contrast filter */
	.absolute :global(canvas) {
		filter: brightness(1.15) contrast(1.05);
	}

	/* ---------- Glass panel ---------- */
	.overlay-content {
		position: relative;
		text-align: center;
		max-width: 560px;
		width: 90%;
		padding: 52px 44px 40px;
		background: linear-gradient(165deg, rgba(15, 23, 42, 0.55) 0%, rgba(8, 12, 24, 0.5) 100%);
		background-image: var(--glass-highlight);
		backdrop-filter: blur(40px) saturate(1.3);
		-webkit-backdrop-filter: blur(40px) saturate(1.3);
		border: 1px solid rgba(59, 130, 246, 0.15);
		border-radius: var(--radius-xl);
		box-shadow:
			0 0 0 1px rgba(255, 255, 255, 0.03),
			0 8px 40px rgba(0, 0, 0, 0.5),
			0 0 80px rgba(59, 130, 246, 0.06);
		pointer-events: auto;
	}

	/* Corner accent marks */
	.corner-tl,
	.corner-br {
		position: absolute;
		width: 20px;
		height: 20px;
		pointer-events: none;
	}
	.corner-tl {
		top: -1px;
		left: -1px;
		border-top: 2px solid rgba(59, 130, 246, 0.4);
		border-left: 2px solid rgba(59, 130, 246, 0.4);
		border-radius: var(--radius-xl) 0 0 0;
	}
	.corner-br {
		bottom: -1px;
		right: -1px;
		border-bottom: 2px solid rgba(59, 130, 246, 0.4);
		border-right: 2px solid rgba(59, 130, 246, 0.4);
		border-radius: 0 0 var(--radius-xl) 0;
	}

	/* ---------- Micro decorative text ---------- */
	.micro-text {
		position: absolute;
		font-family: var(--font-mono);
		font-size: 9px;
		font-weight: 500;
		letter-spacing: 0.12em;
		color: rgba(59, 130, 246, 0.35);
		text-transform: uppercase;
		pointer-events: none;
		user-select: none;
	}
	.top-left {
		top: 14px;
		left: 18px;
	}
	.top-right {
		top: 14px;
		right: 18px;
	}
	.bottom-center {
		bottom: 14px;
		left: 50%;
		transform: translateX(-50%);
		white-space: nowrap;
	}

	/* ---------- Title ---------- */
	.landing-title {
		font-size: 36px;
		font-weight: 700;
		letter-spacing: 0.25em;
		color: var(--color-primary);
		margin: 0 0 10px;
		text-shadow:
			0 0 20px rgba(59, 130, 246, 0.5),
			0 0 60px rgba(59, 130, 246, 0.15);
	}

	/* ---------- Repo card ---------- */
	.repo-card {
		position: relative;
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 12px 16px;
		border-radius: var(--radius);
		font-size: 14px;
		color: var(--color-muted-foreground);
		cursor: pointer;
		background: rgba(10, 15, 28, 0.4);
		border: 1px solid rgba(255, 255, 255, 0.04);
		transition:
			color 0.25s ease-out,
			border-color 0.25s ease-out,
			transform 0.25s ease-out,
			box-shadow 0.25s ease-out,
			background 0.25s ease-out;
	}

	.repo-card:hover {
		color: var(--color-primary);
		border-color: rgba(59, 130, 246, 0.25);
		transform: translateX(4px);
		background: rgba(59, 130, 246, 0.06);
		box-shadow: 0 0 20px rgba(59, 130, 246, 0.1);
	}

	/* Sci-fi brackets on hover */
	.repo-bracket {
		font-family: var(--font-mono);
		font-size: 14px;
		font-weight: 300;
		color: transparent;
		transition: color 0.2s ease-out;
	}
	.repo-bracket.left {
		margin-right: -4px;
	}
	.repo-bracket.right {
		margin-left: auto;
	}
	.repo-card:hover .repo-bracket {
		color: rgba(59, 130, 246, 0.5);
	}

	/* Left indicator line on hover */
	.repo-card::before {
		content: "";
		position: absolute;
		left: 0;
		top: 25%;
		bottom: 25%;
		width: 2px;
		background: var(--color-primary);
		border-radius: 1px;
		opacity: 0;
		transform: scaleY(0);
		transition:
			opacity 0.2s,
			transform 0.25s ease-out;
	}
	.repo-card:hover::before {
		opacity: 1;
		transform: scaleY(1);
	}

	.repo-card:hover .w-1\.5 {
		box-shadow:
			0 0 12px var(--color-primary),
			0 0 24px rgba(59, 130, 246, 0.3);
	}
</style>

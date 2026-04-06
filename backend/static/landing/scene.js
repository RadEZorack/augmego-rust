import * as THREE from "three";
import { GLTFLoader } from "/landing/vendor/GLTFLoader.js";

const SCENE_ENDPOINT = "/api/v1/landing/scene";
const MAX_PIXEL_RATIO = 1.5;
const ARENA_HALF_WIDTH = 8.6;
const ARENA_HALF_DEPTH = 6.4;
const PET_TARGET_HEIGHT = 2.2;
const WEAPON_TARGET_SIZE = 0.95;
const EFFECT_COLORS = {
  laser: 0x84d8ff,
  gun: 0xffd76b,
  flamethrower: 0xff9d7a,
  sword: 0xcfc8ff,
};

export async function bootLandingScene({ container, onLoaded, onUnavailable }) {
  const response = await fetch(SCENE_ENDPOINT, {
    headers: { accept: "application/json" },
  });

  if (!response.ok) {
    throw new Error(`Landing scene request failed: ${response.status}`);
  }

  const payload = await response.json();
  const actors = buildActorSpecs(payload);
  if (actors.length < 3) {
    onUnavailable?.();
    return { available: false };
  }

  const controller = new LandingSceneController(container, actors);
  const ready = await controller.init();
  if (!ready) {
    onUnavailable?.();
    controller.dispose();
    return { available: false };
  }

  onLoaded?.();
  return { available: true, controller };
}

function buildActorSpecs(payload) {
  if (!payload || !Array.isArray(payload.pets) || !Array.isArray(payload.weapons)) {
    return [];
  }

  const weaponById = new Map(
    payload.weapons
      .filter((weapon) => weapon?.id && weapon?.modelUrl)
      .map((weapon) => [weapon.id, weapon]),
  );
  const pairingByPetId = new Map(
    (payload.pairings || [])
      .filter((pairing) => pairing?.petId && pairing?.weaponId)
      .map((pairing) => [pairing.petId, pairing.weaponId]),
  );

  return payload.pets
    .filter((pet) => pet?.id && pet?.modelUrl)
    .map((pet) => {
      const weapon = weaponById.get(pairingByPetId.get(pet.id));
      if (!weapon) {
        return null;
      }

      return { pet, weapon };
    })
    .filter(Boolean)
    .slice(0, 6);
}

class LandingSceneController {
  constructor(container, actorSpecs) {
    this.container = container;
    this.actorSpecs = actorSpecs;
    this.scene = null;
    this.camera = null;
    this.renderer = null;
    this.clock = new THREE.Clock();
    this.loader = new GLTFLoader();
    this.mixers = [];
    this.actors = [];
    this.effects = [];
    this.frameHandle = 0;
    this.resizeObserver = null;
    this.onResize = this.onResize.bind(this);
    this.tick = this.tick.bind(this);
  }

  async init() {
    this.scene = new THREE.Scene();
    this.scene.fog = new THREE.FogExp2(0x08101a, 0.03);

    this.camera = new THREE.PerspectiveCamera(38, 1, 0.1, 100);
    this.camera.position.set(0, 6.7, 15.8);
    this.camera.lookAt(0, 1.4, 0);

    this.renderer = new THREE.WebGLRenderer({
      alpha: true,
      antialias: true,
      powerPreference: "high-performance",
    });
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, MAX_PIXEL_RATIO));
    this.renderer.setClearColor(0x000000, 0);
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.shadowMap.enabled = true;
    this.renderer.shadowMap.type = THREE.PCFSoftShadowMap;
    this.container.replaceChildren(this.renderer.domElement);

    this.buildEnvironment();
    await this.loadActors();
    if (this.actors.length < 3) {
      return false;
    }

    this.onResize();
    window.addEventListener("resize", this.onResize, { passive: true });
    if ("ResizeObserver" in window) {
      this.resizeObserver = new ResizeObserver(() => this.onResize());
      this.resizeObserver.observe(this.container);
    }

    this.frameHandle = window.requestAnimationFrame(this.tick);
    return true;
  }

  dispose() {
    window.cancelAnimationFrame(this.frameHandle);
    window.removeEventListener("resize", this.onResize);
    this.resizeObserver?.disconnect();

    for (const effect of this.effects) {
      disposeObject(effect.object);
    }
    for (const actor of this.actors) {
      disposeObject(actor.group);
    }

    this.renderer?.dispose();
  }

  buildEnvironment() {
    const hemisphere = new THREE.HemisphereLight(0x8cd8ff, 0x0b1018, 1.8);
    this.scene.add(hemisphere);

    const keyLight = new THREE.DirectionalLight(0xfff3d8, 1.7);
    keyLight.position.set(8, 12, 5);
    keyLight.castShadow = true;
    keyLight.shadow.mapSize.set(1024, 1024);
    keyLight.shadow.camera.left = -14;
    keyLight.shadow.camera.right = 14;
    keyLight.shadow.camera.top = 14;
    keyLight.shadow.camera.bottom = -14;
    keyLight.shadow.bias = -0.0002;
    this.scene.add(keyLight);

    const rimLight = new THREE.DirectionalLight(0x84d8ff, 0.55);
    rimLight.position.set(-7, 6, -8);
    this.scene.add(rimLight);

    const arenaFloor = new THREE.Mesh(
      new THREE.CircleGeometry(12.4, 80),
      new THREE.MeshStandardMaterial({
        color: 0x101924,
        roughness: 0.92,
        metalness: 0.06,
      }),
    );
    arenaFloor.rotation.x = -Math.PI / 2;
    arenaFloor.position.y = -0.02;
    arenaFloor.receiveShadow = true;
    this.scene.add(arenaFloor);

    const arenaGrid = new THREE.GridHelper(22, 22, 0x1d4a66, 0x102739);
    arenaGrid.position.y = 0.01;
    arenaGrid.material.opacity = 0.34;
    arenaGrid.material.transparent = true;
    this.scene.add(arenaGrid);

    const ring = new THREE.Mesh(
      new THREE.RingGeometry(9.3, 9.75, 64),
      new THREE.MeshBasicMaterial({
        color: 0x84d8ff,
        transparent: true,
        opacity: 0.18,
        side: THREE.DoubleSide,
      }),
    );
    ring.rotation.x = -Math.PI / 2;
    ring.position.y = 0.03;
    this.scene.add(ring);
  }

  async loadActors() {
    const loadedActors = await Promise.all(
      this.actorSpecs.map((spec, index) => this.loadActor(spec, index)),
    );
    this.actors = loadedActors.filter(Boolean);
  }

  async loadActor(spec, index) {
    try {
      const [petAsset, weaponAsset] = await Promise.all([
        this.loader.loadAsync(spec.pet.modelUrl),
        this.loader.loadAsync(spec.weapon.modelUrl),
      ]);

      const pet = normalizePetModel(petAsset.scene);
      const weapon = normalizeWeaponModel(weaponAsset.scene);
      const actor = this.createActorState(spec, index, pet, weapon, petAsset.animations);
      this.scene.add(actor.group);
      return actor;
    } catch (error) {
      console.warn("landing actor unavailable", spec.pet.displayName, error);
      return null;
    }
  }

  createActorState(spec, index, pet, weapon, animations) {
    const angle = (index / this.actorSpecs.length) * Math.PI * 2;
    const group = new THREE.Group();
    group.position.set(
      Math.cos(angle) * (ARENA_HALF_WIDTH * 0.68),
      0,
      Math.sin(angle) * (ARENA_HALF_DEPTH * 0.68),
    );
    group.rotation.y = angle + Math.PI;

    const shadow = new THREE.Mesh(
      new THREE.CircleGeometry(0.72, 20),
      new THREE.MeshBasicMaterial({
        color: 0x000000,
        transparent: true,
        opacity: 0.22,
        depthWrite: false,
      }),
    );
    shadow.rotation.x = -Math.PI / 2;
    shadow.position.y = 0.01;
    group.add(shadow);

    group.add(pet.root);

    const weaponSocket = new THREE.Group();
    weaponSocket.position.set(0.52, pet.height * 0.55, 0.34);
    weaponSocket.rotation.set(-0.18, Math.PI * 0.52, -0.18);
    weaponSocket.add(weapon.root);
    group.add(weaponSocket);

    const materialStates = captureMaterialStates(group);
    let mixer = null;
    if (animations?.length) {
      mixer = new THREE.AnimationMixer(pet.root);
      mixer.clipAction(animations[0]).play();
      this.mixers.push(mixer);
    }

    return {
      index,
      kind: spec.weapon.kind,
      group,
      petRoot: pet.root,
      weaponSocket,
      shadow,
      materialStates,
      mixer,
      petHeight: pet.height,
      moveTarget: randomArenaTarget(),
      idleUntilMs: performance.now() + randomBetween(250, 1500),
      nextFireAtMs: performance.now() + randomBetween(1000, 3000),
      movementSpeed: randomBetween(1.05, 1.45),
      hitFlash: 0,
      recoil: 0,
      stagger: 0,
      hoverSeed: Math.random() * Math.PI * 2,
    };
  }

  onResize() {
    const width = Math.max(1, this.container.clientWidth);
    const height = Math.max(1, this.container.clientHeight);
    this.camera.aspect = width / height;
    this.camera.updateProjectionMatrix();
    this.renderer.setSize(width, height, false);
  }

  tick() {
    const dt = Math.min(this.clock.getDelta(), 1 / 24);
    const nowMs = performance.now();
    const nowSeconds = nowMs / 1000;

    for (const mixer of this.mixers) {
      mixer.update(dt);
    }

    for (const actor of this.actors) {
      this.updateActor(actor, dt, nowMs, nowSeconds);
      this.tryFire(actor, nowMs);
    }

    this.updateEffects(nowMs);
    this.renderer.render(this.scene, this.camera);
    this.frameHandle = window.requestAnimationFrame(this.tick);
  }

  updateActor(actor, dt, nowMs, nowSeconds) {
    actor.hitFlash = Math.max(0, actor.hitFlash - dt * 2.8);
    actor.recoil = Math.max(0, actor.recoil - dt * 4.5);
    actor.stagger = Math.max(0, actor.stagger - dt * 3.3);

    const flatPosition = new THREE.Vector3(
      actor.group.position.x,
      0,
      actor.group.position.z,
    );
    const toTarget = actor.moveTarget.clone().sub(flatPosition);
    const distance = toTarget.length();

    if (distance < 0.45) {
      if (nowMs > actor.idleUntilMs) {
        actor.moveTarget = randomArenaTarget();
        actor.idleUntilMs = nowMs + randomBetween(420, 1400);
      }
    } else if (nowMs > actor.idleUntilMs) {
      const direction = toTarget.normalize();
      actor.group.position.addScaledVector(direction, actor.movementSpeed * dt);
      actor.group.position.x = THREE.MathUtils.clamp(
        actor.group.position.x,
        -ARENA_HALF_WIDTH,
        ARENA_HALF_WIDTH,
      );
      actor.group.position.z = THREE.MathUtils.clamp(
        actor.group.position.z,
        -ARENA_HALF_DEPTH,
        ARENA_HALF_DEPTH,
      );
      const desiredYaw = Math.atan2(direction.x, direction.z);
      actor.group.rotation.y = dampAngle(actor.group.rotation.y, desiredYaw, dt * 4.2);
    }

    const hover =
      Math.sin(nowSeconds * 2.4 + actor.hoverSeed) * 0.04 +
      Math.sin(nowSeconds * 4.6 + actor.hoverSeed) * 0.015;
    actor.group.position.y = hover;
    actor.petRoot.rotation.z =
      Math.sin(nowSeconds * 5.4 + actor.hoverSeed) * 0.025 + actor.stagger * 0.09;
    actor.petRoot.position.z = -actor.recoil * 0.12;
    actor.weaponSocket.rotation.x = -0.18 - actor.recoil * 0.6;
    actor.weaponSocket.rotation.z = -0.18 + actor.stagger * 0.08;
    actor.shadow.scale.setScalar(1 - actor.hitFlash * 0.08);

    updateActorMaterials(actor);
  }

  tryFire(actor, nowMs) {
    if (nowMs < actor.nextFireAtMs || this.actors.length < 2) {
      return;
    }

    const target = chooseNearestOpponent(actor, this.actors);
    if (!target) {
      actor.nextFireAtMs = nowMs + randomBetween(1800, 2800);
      return;
    }

    actor.nextFireAtMs = nowMs + randomBetween(2500, 4500);
    actor.idleUntilMs = nowMs + randomBetween(180, 420);
    actor.recoil = 1;
    target.hitFlash = Math.max(target.hitFlash, 0.95);
    target.stagger = Math.max(target.stagger, 0.7);
    target.moveTarget = randomArenaTarget();
    target.idleUntilMs = nowMs + randomBetween(240, 560);

    const muzzleOffset = new THREE.Vector3(0.56, actor.petHeight * 0.56, 0.34).applyAxisAngle(
      new THREE.Vector3(0, 1, 0),
      actor.group.rotation.y,
    );
    const origin = actor.group.position.clone().add(muzzleOffset);
    const targetPoint = target.group.position
      .clone()
      .add(new THREE.Vector3(0, target.petHeight * 0.48, 0));

    const effect = createEffect(actor.kind, origin, targetPoint, nowMs);
    this.effects.push(effect);
    this.scene.add(effect.object);
  }

  updateEffects(nowMs) {
    this.effects = this.effects.filter((effect) => {
      const progress = Math.min(1, (nowMs - effect.startedAtMs) / effect.durationMs);
      effect.update(progress);

      if (progress >= 1) {
        this.scene.remove(effect.object);
        disposeObject(effect.object);
        return false;
      }

      return true;
    });
  }
}

function normalizePetModel(scene) {
  const object = prepareModel(scene.clone(true));
  const root = new THREE.Group();
  root.add(object);

  const box = new THREE.Box3().setFromObject(object);
  const size = box.getSize(new THREE.Vector3());
  const scale = PET_TARGET_HEIGHT / Math.max(size.y, 0.001);
  object.scale.setScalar(scale);

  const scaledBox = new THREE.Box3().setFromObject(object);
  const center = scaledBox.getCenter(new THREE.Vector3());
  object.position.x -= center.x;
  object.position.z -= center.z;
  object.position.y -= scaledBox.min.y;

  const groundedBox = new THREE.Box3().setFromObject(root);
  return {
    root,
    height: groundedBox.getSize(new THREE.Vector3()).y,
  };
}

function normalizeWeaponModel(scene) {
  const object = prepareModel(scene.clone(true));
  const root = new THREE.Group();
  root.add(object);

  const box = new THREE.Box3().setFromObject(object);
  const size = box.getSize(new THREE.Vector3());
  const scale = WEAPON_TARGET_SIZE / Math.max(size.length(), 0.001);
  object.scale.setScalar(scale);

  const scaledBox = new THREE.Box3().setFromObject(object);
  const center = scaledBox.getCenter(new THREE.Vector3());
  object.position.sub(center);

  return { root };
}

function prepareModel(root) {
  root.traverse((child) => {
    child.castShadow = true;
    child.receiveShadow = true;

    if (!child.material) {
      return;
    }

    if (Array.isArray(child.material)) {
      child.material = child.material.map((material) => material.clone());
      return;
    }

    child.material = child.material.clone();
  });

  return root;
}

function captureMaterialStates(root) {
  const states = [];

  root.traverse((child) => {
    if (!child.material) {
      return;
    }

    const materials = Array.isArray(child.material)
      ? child.material
      : [child.material];
    for (const material of materials) {
      states.push({
        material,
        color: material.color?.clone() || null,
        emissive: material.emissive?.clone() || null,
        emissiveIntensity: material.emissiveIntensity || 0,
      });
    }
  });

  return states;
}

function updateActorMaterials(actor) {
  const flashColor = new THREE.Color(EFFECT_COLORS[actor.kind] || 0xffffff);

  for (const state of actor.materialStates) {
    if (state.color) {
      state.material.color.copy(state.color).lerp(flashColor, actor.hitFlash * 0.18);
    }
    if (state.emissive) {
      state.material.emissive.copy(state.emissive).lerp(flashColor, actor.hitFlash * 0.85);
      state.material.emissiveIntensity = state.emissiveIntensity + actor.hitFlash * 0.85;
    }
  }
}

function chooseNearestOpponent(actor, actors) {
  let bestTarget = null;
  let bestDistanceSq = Number.POSITIVE_INFINITY;

  for (const candidate of actors) {
    if (candidate === actor) {
      continue;
    }

    const distanceSq = actor.group.position.distanceToSquared(candidate.group.position);
    if (distanceSq < bestDistanceSq) {
      bestDistanceSq = distanceSq;
      bestTarget = candidate;
    }
  }

  return bestTarget;
}

function randomArenaTarget() {
  return new THREE.Vector3(
    randomBetween(-ARENA_HALF_WIDTH, ARENA_HALF_WIDTH),
    0,
    randomBetween(-ARENA_HALF_DEPTH, ARENA_HALF_DEPTH),
  );
}

function randomBetween(min, max) {
  return min + Math.random() * (max - min);
}

function dampAngle(current, target, amount) {
  const delta = normalizeAngle(target - current);
  return current + delta * Math.min(1, amount);
}

function normalizeAngle(value) {
  let angle = value;
  while (angle > Math.PI) {
    angle -= Math.PI * 2;
  }
  while (angle < -Math.PI) {
    angle += Math.PI * 2;
  }
  return angle;
}

function createEffect(kind, origin, target, startedAtMs) {
  switch (kind) {
    case "gun":
      return createGunEffect(origin, target, startedAtMs);
    case "flamethrower":
      return createFlameEffect(origin, target, startedAtMs);
    case "sword":
      return createSwordEffect(origin, target, startedAtMs);
    case "laser":
    default:
      return createLaserEffect(origin, target, startedAtMs);
  }
}

function createLaserEffect(origin, target, startedAtMs) {
  const geometry = new THREE.BufferGeometry().setFromPoints([origin, target]);
  const material = new THREE.LineBasicMaterial({
    color: EFFECT_COLORS.laser,
    transparent: true,
    opacity: 0.94,
    depthWrite: false,
  });
  const line = new THREE.Line(geometry, material);

  return {
    object: line,
    startedAtMs,
    durationMs: 170,
    update(progress) {
      material.opacity = 0.94 * (1 - progress);
      line.scale.setScalar(1 + progress * 0.03);
    },
  };
}

function createGunEffect(origin, target, startedAtMs) {
  const material = new THREE.MeshStandardMaterial({
    color: EFFECT_COLORS.gun,
    emissive: EFFECT_COLORS.gun,
    emissiveIntensity: 0.9,
    transparent: true,
    opacity: 0.94,
    depthWrite: false,
  });
  const projectile = new THREE.Mesh(new THREE.SphereGeometry(0.1, 12, 12), material);
  projectile.position.copy(origin);

  return {
    object: projectile,
    startedAtMs,
    durationMs: 280,
    update(progress) {
      projectile.position.lerpVectors(origin, target, progress);
      projectile.scale.setScalar(1 - progress * 0.3);
      material.opacity = 0.94 * (1 - progress * 0.7);
    },
  };
}

function createFlameEffect(origin, target, startedAtMs) {
  const group = new THREE.Group();
  const embers = [];

  for (let index = 0; index < 6; index += 1) {
    const material = new THREE.MeshBasicMaterial({
      color: index % 2 === 0 ? EFFECT_COLORS.flamethrower : EFFECT_COLORS.gun,
      transparent: true,
      opacity: 0.88,
      depthWrite: false,
    });
    const ember = new THREE.Mesh(new THREE.SphereGeometry(0.12, 10, 10), material);
    group.add(ember);
    embers.push({ ember, material, offset: Math.random() * 0.18 });
  }

  return {
    object: group,
    startedAtMs,
    durationMs: 360,
    update(progress) {
      const direction = new THREE.Vector3().subVectors(target, origin);
      const side = new THREE.Vector3(direction.z, 0, -direction.x)
        .normalize()
        .multiplyScalar(0.22);

      embers.forEach((entry, index) => {
        const emberProgress = Math.min(1, progress * (0.72 + index * 0.08));
        entry.ember.position.lerpVectors(origin, target, emberProgress);
        entry.ember.position.addScaledVector(side, Math.sin(progress * 8 + index) * entry.offset);
        const scale = 1 - emberProgress * 0.5;
        entry.ember.scale.setScalar(scale);
        entry.material.opacity = 0.82 * (1 - emberProgress);
      });
    },
  };
}

function createSwordEffect(origin, target, startedAtMs) {
  const material = new THREE.MeshStandardMaterial({
    color: EFFECT_COLORS.sword,
    emissive: EFFECT_COLORS.sword,
    emissiveIntensity: 0.45,
    transparent: true,
    opacity: 0.92,
    depthWrite: false,
  });
  const blade = new THREE.Mesh(new THREE.BoxGeometry(0.14, 0.04, 0.92), material);
  blade.position.copy(origin);

  return {
    object: blade,
    startedAtMs,
    durationMs: 540,
    update(progress) {
      const swingProgress = progress < 0.55 ? progress / 0.55 : 1 - (progress - 0.55) / 0.45;
      blade.position.lerpVectors(origin, target, Math.max(0.08, swingProgress * 0.9));
      blade.rotation.x = progress * Math.PI * 5;
      blade.rotation.y = progress * Math.PI * 3.2;
      blade.rotation.z = progress * Math.PI * 4.5;
      material.opacity = 0.92 * (1 - progress * 0.8);
    },
  };
}

function disposeObject(root) {
  root.traverse((child) => {
    child.geometry?.dispose?.();

    if (!child.material) {
      return;
    }

    if (Array.isArray(child.material)) {
      child.material.forEach((material) => material.dispose?.());
      return;
    }

    child.material.dispose?.();
  });
}

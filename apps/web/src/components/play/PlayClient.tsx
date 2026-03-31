"use client";

import { Suspense, useEffect, useMemo, useRef, useState } from "react";
import { Canvas, useFrame, useLoader, useThree } from "@react-three/fiber";
import * as THREE from "three";
import { GLTF, GLTFLoader } from "three/examples/jsm/loaders/GLTFLoader.js";
import { BlockId, blockDefinition, currentAvatarUrl } from "@/src/game/shared/content";
import { CHUNK_DEPTH, CHUNK_HEIGHT, CHUNK_WIDTH, ChunkPos, LocalVoxelPos, minWorldBlock, toChunkLocal } from "@/src/game/shared/math";
import { ChunkData, chunkVoxel, iterateChunkBlocks } from "@/src/game/shared/world";
import { GameAuthUser, GameEngine, GameSnapshot, RemotePlayer } from "@/src/game/client/engine";

type PlayClientProps = {
  initialUser: GameAuthUser;
};

export default function PlayClient({ initialUser }: PlayClientProps) {
  const [user, setUser] = useState<GameAuthUser>(initialUser);
  const [showAvatarModal, setShowAvatarModal] = useState(false);
  const [snapshot, setSnapshot] = useState<GameSnapshot | null>(null);
  const engineRef = useRef<GameEngine | null>(null);
  const wrapperRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const engine = new GameEngine({ authUser: user });
    engineRef.current = engine;
    setSnapshot(engine.getSnapshot());
    return engine.subscribe(() => {
      setSnapshot(engine.getSnapshot());
    });
  }, []);

  useEffect(() => {
    if (!engineRef.current) {
      return;
    }
    engineRef.current.setAuthUser(user);
  }, [user]);

  useEffect(() => {
    return () => {
      engineRef.current?.dispose();
      engineRef.current = null;
    };
  }, []);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (!engineRef.current) {
        return;
      }
      if (event.code.startsWith("Digit")) {
        const slot = Number.parseInt(event.code.replace("Digit", ""), 10) - 1;
        if (Number.isFinite(slot)) {
          engineRef.current.setSelectedSlot(slot);
        }
      }
      if (event.code === "KeyT") {
        event.preventDefault();
        const input = document.getElementById("play-chat-input") as HTMLInputElement | null;
        input?.focus();
      }
      engineRef.current.setKey(event.code, true);
    };

    const handleKeyUp = (event: KeyboardEvent) => {
      engineRef.current?.setKey(event.code, false);
    };

    const handleMouseMove = (event: MouseEvent) => {
      if (document.pointerLockElement !== wrapperRef.current) {
        return;
      }
      engineRef.current?.look(event.movementX, event.movementY);
    };

    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("keyup", handleKeyUp);
    window.addEventListener("mousemove", handleMouseMove);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("keyup", handleKeyUp);
      window.removeEventListener("mousemove", handleMouseMove);
    };
  }, []);

  const renderChunks = useMemo(() => {
    if (!engineRef.current || !snapshot) {
      return [] as ChunkData[];
    }
    return engineRef.current.getRenderableChunks();
  }, [snapshot?.worldRevision, snapshot?.localPosition?.[0], snapshot?.localPosition?.[1], snapshot?.localPosition?.[2], snapshot?.viewRadius]);
  const renderChunkLookup = useMemo(
    () => new Map(renderChunks.map((chunk) => [chunkKey(chunk.position), chunk])),
    [renderChunks],
  );

  if (!snapshot) {
    return <div className="play-loading">Loading world...</div>;
  }

  return (
    <div
      className="play-root"
      ref={wrapperRef}
      onContextMenu={(event) => event.preventDefault()}
      onMouseDown={(event) => {
        if (document.pointerLockElement !== wrapperRef.current) {
          wrapperRef.current?.requestPointerLock();
          return;
        }

        if (event.button === 0) {
          engineRef.current?.primaryAction();
        }
        if (event.button === 2) {
          engineRef.current?.secondaryAction();
        }
      }}
    >
      <Canvas className="play-canvas" camera={{ fov: 75, near: 0.1, far: 400 }}>
        <color attach="background" args={["#b8d8ef"]} />
        <fog attach="fog" args={["#b8d8ef", 30, 220]} />
        <ambientLight intensity={1.8} />
        <directionalLight intensity={1.4} position={[40, 90, 20]} />
        <SceneBridge engine={engineRef.current} snapshot={snapshot} />
        {renderChunks.map((chunk) => (
          <ChunkMesh
            key={`${chunk.position.x},${chunk.position.z},${chunk.revision}`}
            chunk={chunk}
            chunkLookup={renderChunkLookup}
          />
        ))}
        {snapshot.target ? <TargetOutline position={snapshot.target.breakPosition} /> : null}
        <Suspense fallback={null}>
          {snapshot.remotePlayers.map((player) => (
            <RemotePlayerMesh key={player.playerId} player={player} />
          ))}
        </Suspense>
      </Canvas>

      <div className="play-gradient" />
      <div className="crosshair" />

      <div className="play-overlay top-left">
        <div className="hud-card status-card">
          <p className="hud-label">World</p>
          <h1>Augmego Frontier</h1>
          <p className="hud-copy">
            {snapshot.status === "ready"
              ? "Connected to the unified Next.js realtime world."
              : "Connecting to the unified Next.js realtime world..."}
          </p>
          <div className="hud-pills">
            <span>{snapshot.status}</span>
            <span>seed {snapshot.worldSeed.toString(16)}</span>
            <span>/play + /ws</span>
          </div>
          <div className="hud-actions">
            {!user ? (
              <a className="hud-button primary" href="/api/v1/auth/google?returnTo=/play">
                Sign In
              </a>
            ) : null}
            <button className="hud-button" onClick={() => void engineRef.current?.requestMedia()}>
              Enable Webcam
            </button>
            <button className="hud-button" onClick={() => setShowAvatarModal(true)}>
              Avatar Setup
            </button>
          </div>
          {snapshot.error ? <p className="hud-error">{snapshot.error}</p> : null}
          {snapshot.mediaError ? <p className="hud-error">{snapshot.mediaError}</p> : null}
        </div>

        <div className="hud-card controls-card">
          <p className="hud-label">Controls</p>
          <p className="hud-copy">WASD move, Shift sprint, Space jump, click to lock mouse, left/right click break and place, T chat.</p>
        </div>
      </div>

      <div className="play-overlay top-right">
        <div className="hud-card players-card">
          <p className="hud-label">Players</p>
          <ul className="players-list">
            <li className="player-row self">
              <span>{user?.name ?? user?.email ?? "Guest"}</span>
              <span>you</span>
            </li>
            {snapshot.remotePlayers.map((player) => (
              <li key={player.playerId} className="player-row">
                <span>{player.displayName}</span>
                <span>{Math.round(Math.hypot(player.velocity[0], player.velocity[2]) * 10) / 10} m/s</span>
              </li>
            ))}
          </ul>
        </div>

        <div className="hud-card video-card">
          <p className="hud-label">Media</p>
          <div className="video-grid">
            {snapshot.localStream ? <VideoTile stream={snapshot.localStream} label="You" muted /> : null}
            {snapshot.remoteStreams.map((entry) => (
              <VideoTile
                key={entry.playerId}
                stream={entry.stream}
                label={snapshot.remotePlayers.find((player) => player.playerId === entry.playerId)?.displayName ?? `Player ${entry.playerId}`}
              />
            ))}
            {!snapshot.localStream && snapshot.remoteStreams.length === 0 ? (
              <div className="video-empty">Enable your webcam to start peer media.</div>
            ) : null}
          </div>
        </div>
      </div>

      <div className="play-overlay bottom-left">
        <div className="hud-card chat-card">
          <p className="hud-label">Chat</p>
          <div className="chat-log">
            {snapshot.chatMessages.length === 0 ? (
              <p className="chat-empty">No chat yet. Press T or type below.</p>
            ) : (
              snapshot.chatMessages.map((message) => (
                <p key={message.id} className="chat-line">
                  <strong>{message.from}</strong> {message.body}
                </p>
              ))
            )}
          </div>
          <form
            className="chat-form"
            onSubmit={(event) => {
              event.preventDefault();
              const form = event.currentTarget;
              const input = form.elements.namedItem("message") as HTMLInputElement;
              engineRef.current?.sendChat(input.value);
              input.value = "";
            }}
          >
            <input id="play-chat-input" name="message" placeholder="Say something to the room" />
            <button type="submit">Send</button>
          </form>
        </div>
      </div>

      <div className="play-overlay bottom-center">
        <div className="hotbar">
          {snapshot.inventory.map((slot, index) => (
            <button
              key={`${slot.block}-${index}`}
              className={index === snapshot.selectedSlot ? "hotbar-slot active" : "hotbar-slot"}
              onClick={() => engineRef.current?.setSelectedSlot(index)}
            >
              <span>{index + 1}</span>
              <strong>{blockDefinition(slot.block).name}</strong>
              <small>{slot.count}</small>
            </button>
          ))}
        </div>
      </div>

      {showAvatarModal ? (
        <AvatarModal
          user={user}
          onClose={() => setShowAvatarModal(false)}
          onSaved={(nextUser) => {
            setUser(nextUser);
            setShowAvatarModal(false);
          }}
        />
      ) : null}
    </div>
  );
}

function SceneBridge({ engine, snapshot }: { engine: GameEngine | null; snapshot: GameSnapshot }) {
  const { camera } = useThree();

  useFrame((_, delta) => {
    engine?.update(delta);
    camera.position.set(snapshot.localPosition[0], snapshot.localPosition[1], snapshot.localPosition[2]);
    camera.rotation.order = "YXZ";
    camera.rotation.y = snapshot.yaw;
    camera.rotation.x = snapshot.pitch;
  });

  return null;
}

function ChunkMesh({
  chunk,
  chunkLookup,
}: {
  chunk: ChunkData;
  chunkLookup: Map<string, ChunkData>;
}) {
  const groups = useMemo(() => buildChunkInstances(chunk, chunkLookup), [chunk, chunkLookup]);

  return (
    <group>
      {groups.map((group) => {
        const definition = blockDefinition(group.block);
        return (
          <ChunkInstances
            key={`${chunk.position.x},${chunk.position.z},${group.block}`}
            positions={group.positions}
            color={definition.color}
            transparent={definition.transparent}
          />
        );
      })}
    </group>
  );
}

function ChunkInstances({
  positions,
  color,
  transparent,
}: {
  positions: Array<[number, number, number]>;
  color: string;
  transparent: boolean;
}) {
  const ref = useRef<THREE.InstancedMesh>(null);
  const matrix = useMemo(() => new THREE.Matrix4(), []);

  useEffect(() => {
    const mesh = ref.current;
    if (!mesh) {
      return;
    }
    mesh.count = positions.length;
    positions.forEach((position, index) => {
      matrix.makeTranslation(position[0], position[1], position[2]);
      mesh.setMatrixAt(index, matrix);
    });
    mesh.instanceMatrix.needsUpdate = true;
    mesh.computeBoundingBox();
    mesh.computeBoundingSphere();
  }, [matrix, positions]);

  return (
    <instancedMesh ref={ref} args={[undefined, undefined, positions.length]}>
      <boxGeometry args={[1, 1, 1]} />
      <meshStandardMaterial color={color} transparent={transparent} opacity={transparent ? 0.72 : 1} />
    </instancedMesh>
  );
}

function TargetOutline({ position }: { position: { x: number; y: number; z: number } }) {
  return (
    <mesh position={[position.x + 0.5, position.y + 0.5, position.z + 0.5]}>
      <boxGeometry args={[1.03, 1.03, 1.03]} />
      <meshBasicMaterial color="#ffffff" wireframe transparent opacity={0.9} />
    </mesh>
  );
}

function RemotePlayerMesh({ player }: { player: RemotePlayer }) {
  const speed = Math.hypot(player.velocity[0], player.velocity[2]);
  const idleSeconds = Math.max(0, (performance.now() - player.updatedAt) / 1000);
  const avatarUrl = currentAvatarUrl(player.avatarSelection, speed, idleSeconds);

  if (!avatarUrl) {
    return (
      <mesh position={[player.position[0], player.position[1] - 1, player.position[2]]} rotation={[0, player.yaw, 0]}>
        <capsuleGeometry args={[0.35, 1.2, 6, 10]} />
        <meshStandardMaterial color="#ef8b58" />
      </mesh>
    );
  }

  return <AvatarModel player={player} url={avatarUrl} />;
}

function AvatarModel({ player, url }: { player: RemotePlayer; url: string }) {
  const gltf = useLoader(GLTFLoader, url) as GLTF;
  const scene = useMemo(() => gltf.scene.clone(true), [gltf.scene]);

  return (
    <primitive
      object={scene}
      position={[player.position[0], player.position[1] - 1.62, player.position[2]]}
      rotation={[0, player.yaw + Math.PI, 0]}
      scale={[0.8, 0.8, 0.8]}
    />
  );
}

function VideoTile({
  stream,
  label,
  muted = false,
}: {
  stream: MediaStream;
  label: string;
  muted?: boolean;
}) {
  const ref = useRef<HTMLVideoElement | null>(null);

  useEffect(() => {
    if (ref.current) {
      ref.current.srcObject = stream;
    }
  }, [stream]);

  return (
    <figure className="video-tile">
      <video ref={ref} autoPlay playsInline muted={muted} />
      <figcaption>{label}</figcaption>
    </figure>
  );
}

function AvatarModal({
  user,
  onClose,
  onSaved,
}: {
  user: GameAuthUser;
  onClose: () => void;
  onSaved: (user: GameAuthUser) => void;
}) {
  const [stationaryModelUrl, setStationaryModelUrl] = useState(user?.avatarSelection.stationaryModelUrl ?? "");
  const [moveModelUrl, setMoveModelUrl] = useState(user?.avatarSelection.moveModelUrl ?? "");
  const [specialModelUrl, setSpecialModelUrl] = useState(user?.avatarSelection.specialModelUrl ?? "");
  const [status, setStatus] = useState<string | null>(null);
  const formRef = useRef<HTMLFormElement | null>(null);

  const refreshUser = async () => {
    const response = await fetch("/api/v1/auth/me");
    const body = (await response.json()) as { user: GameAuthUser };
    onSaved(body.user);
  };

  return (
    <div className="avatar-modal-backdrop" onClick={onClose}>
      <div className="avatar-modal" onClick={(event) => event.stopPropagation()}>
        <div className="avatar-modal-header">
          <div>
            <p className="hud-label">Avatar Setup</p>
            <h2>Remote avatar models</h2>
          </div>
          <button className="hud-button" onClick={onClose}>
            Close
          </button>
        </div>

        {!user ? <p className="hud-error">Sign in first to save avatar settings.</p> : null}

        <form
          ref={formRef}
          className="avatar-form"
          onSubmit={async (event) => {
            event.preventDefault();
            if (!user) {
              return;
            }

            const payload = {
              stationaryModelUrl,
              moveModelUrl,
              specialModelUrl,
            };
            const response = await fetch("/api/v1/auth/player-avatar", {
              method: "PATCH",
              headers: {
                "Content-Type": "application/json",
              },
              body: JSON.stringify(payload),
            });

            if (!response.ok) {
              setStatus("Saving avatar URLs failed.");
              return;
            }

            const formData = new FormData();
            const files = [
              ["idleFile", (formRef.current?.elements.namedItem("idleFile") as HTMLInputElement | null)?.files?.[0]],
              ["runFile", (formRef.current?.elements.namedItem("runFile") as HTMLInputElement | null)?.files?.[0]],
              ["danceFile", (formRef.current?.elements.namedItem("danceFile") as HTMLInputElement | null)?.files?.[0]],
            ] as const;
            for (const [key, file] of files) {
              if (file) {
                formData.set(key, file);
              }
            }

            if ([...formData.keys()].length > 0) {
              const uploadResponse = await fetch("/api/v1/auth/player-avatar/upload", {
                method: "POST",
                body: formData,
              });
              if (!uploadResponse.ok) {
                setStatus("Avatar upload failed.");
                return;
              }
            }

            await refreshUser();
            setStatus("Avatar settings saved.");
          }}
        >
          <label>
            Idle / stationary URL
            <input value={stationaryModelUrl} onChange={(event) => setStationaryModelUrl(event.target.value)} placeholder="https://..." />
          </label>
          <label>
            Move URL
            <input value={moveModelUrl} onChange={(event) => setMoveModelUrl(event.target.value)} placeholder="https://..." />
          </label>
          <label>
            Special / dance URL
            <input value={specialModelUrl} onChange={(event) => setSpecialModelUrl(event.target.value)} placeholder="https://..." />
          </label>
          <label>
            Upload idle GLB
            <input name="idleFile" type="file" accept=".glb,model/gltf-binary" />
          </label>
          <label>
            Upload move GLB
            <input name="runFile" type="file" accept=".glb,model/gltf-binary" />
          </label>
          <label>
            Upload special GLB
            <input name="danceFile" type="file" accept=".glb,model/gltf-binary" />
          </label>

          <div className="hud-actions">
            <button className="hud-button primary" type="submit" disabled={!user}>
              Save
            </button>
          </div>
          {status ? <p className="hud-copy">{status}</p> : null}
        </form>
      </div>
    </div>
  );
}

function buildChunkInstances(
  chunk: ChunkData,
  chunkLookup: Map<string, ChunkData>,
) {
  const groups = new Map<BlockId, Array<[number, number, number]>>();
  const base = minWorldBlock(chunk.position);

  iterateChunkBlocks(chunk, (block, local) => {
    if (block === BlockId.Air) {
      return;
    }
    if (!isBlockVisible(chunk, local, chunkLookup)) {
      return;
    }

    const positions = groups.get(block) ?? [];
    positions.push([
      base.x + local.x + 0.5,
      local.y + 0.5,
      base.z + local.z + 0.5,
    ]);
    groups.set(block, positions);
  });

  return [...groups.entries()].map(([block, positions]) => ({ block, positions }));
}

function isBlockVisible(
  chunk: ChunkData,
  local: LocalVoxelPos,
  chunkLookup: Map<string, ChunkData>,
) {
  const neighbors: Array<[number, number, number]> = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
  ];
  const base = minWorldBlock(chunk.position);

  for (const [dx, dy, dz] of neighbors) {
    const worldPosition = {
      x: base.x + local.x + dx,
      y: local.y + dy,
      z: base.z + local.z + dz,
    };
    if (worldPosition.y < 0 || worldPosition.y >= CHUNK_HEIGHT) {
      return true;
    }
    const neighbor = blockAtRenderedWorldPosition(chunkLookup, worldPosition);
    if ([BlockId.Air, BlockId.Water, BlockId.Leaves, BlockId.Glass].includes(neighbor)) {
      return true;
    }
  }

  return false;
}

function chunkKey(position: ChunkPos) {
  return `${position.x},${position.z}`;
}

function blockAtRenderedWorldPosition(
  chunkLookup: Map<string, ChunkData>,
  worldPosition: { x: number; y: number; z: number },
) {
  const { chunk, local } = toChunkLocal(worldPosition);
  const targetChunk = chunkLookup.get(chunkKey(chunk));
  return targetChunk ? chunkVoxel(targetChunk, local).block : BlockId.Air;
}

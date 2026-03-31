export enum BlockId {
  Air = 0,
  Grass = 1,
  Dirt = 2,
  Stone = 3,
  Sand = 4,
  Water = 5,
  Log = 6,
  Leaves = 7,
  Planks = 8,
  Glass = 9,
  Lantern = 10,
  Storage = 11,
  GoldOre = 12,
}

export type AvatarSelection = {
  stationaryModelUrl: string | null;
  moveModelUrl: string | null;
  specialModelUrl: string | null;
};

export type BlockDefinition = {
  id: BlockId;
  name: string;
  solid: boolean;
  transparent: boolean;
  hardness: number;
  color: string;
};

export const BLOCK_DEFINITIONS: BlockDefinition[] = [
  { id: BlockId.Air, name: "Air", solid: false, transparent: true, hardness: 0, color: "#000000" },
  { id: BlockId.Grass, name: "Grass", solid: true, transparent: false, hardness: 0.8, color: "#5f9d44" },
  { id: BlockId.Dirt, name: "Dirt", solid: true, transparent: false, hardness: 0.7, color: "#7e5938" },
  { id: BlockId.Stone, name: "Stone", solid: true, transparent: false, hardness: 1.6, color: "#7a7f87" },
  { id: BlockId.Sand, name: "Sand", solid: true, transparent: false, hardness: 0.6, color: "#d8c27b" },
  { id: BlockId.Water, name: "Water", solid: false, transparent: true, hardness: 0, color: "#5b8fd4" },
  { id: BlockId.Log, name: "Log", solid: true, transparent: false, hardness: 1.2, color: "#8a5a33" },
  { id: BlockId.Leaves, name: "Leaves", solid: true, transparent: true, hardness: 0.2, color: "#4d7f43" },
  { id: BlockId.Planks, name: "Planks", solid: true, transparent: false, hardness: 1.1, color: "#bf8d53" },
  { id: BlockId.Glass, name: "Glass", solid: true, transparent: true, hardness: 0.3, color: "#d0edf5" },
  { id: BlockId.Lantern, name: "Lantern", solid: true, transparent: true, hardness: 0.3, color: "#ffd96b" },
  { id: BlockId.Storage, name: "Storage Crate", solid: true, transparent: false, hardness: 1.5, color: "#9a6f3c" },
  { id: BlockId.GoldOre, name: "Gold Ore", solid: true, transparent: false, hardness: 1.9, color: "#d1a431" },
];

const blockDefinitionMap = new Map(BLOCK_DEFINITIONS.map((definition) => [definition.id, definition]));

export function blockDefinition(id: BlockId) {
  return blockDefinitionMap.get(id) ?? blockDefinitionMap.get(BlockId.Air)!;
}

export function blockIsTransparent(id: BlockId) {
  return blockDefinition(id).transparent;
}

export function blockIsSolid(id: BlockId) {
  return blockDefinition(id).solid && !blockDefinition(id).transparent;
}

export function normalizeAvatarSelection(selection: Partial<AvatarSelection> | null | undefined): AvatarSelection {
  return {
    stationaryModelUrl: sanitizeAvatarUrl(selection?.stationaryModelUrl),
    moveModelUrl: sanitizeAvatarUrl(selection?.moveModelUrl),
    specialModelUrl: sanitizeAvatarUrl(selection?.specialModelUrl),
  };
}

function sanitizeAvatarUrl(value: string | null | undefined) {
  if (!value) {
    return null;
  }

  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

export function currentAvatarUrl(selection: AvatarSelection, speed: number, idleSeconds: number) {
  if (speed > 0.15) {
    return selection.moveModelUrl ?? selection.stationaryModelUrl ?? selection.specialModelUrl;
  }

  if (idleSeconds >= 5) {
    return selection.specialModelUrl ?? selection.stationaryModelUrl ?? selection.moveModelUrl;
  }

  return selection.stationaryModelUrl ?? selection.moveModelUrl ?? selection.specialModelUrl;
}

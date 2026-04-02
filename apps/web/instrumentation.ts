export async function register() {
  if (process.env.NEXT_RUNTIME !== "nodejs") {
    return;
  }

  const { startPetGenerationWorker } = await import("@/src/lib/pets");
  startPetGenerationWorker();
}

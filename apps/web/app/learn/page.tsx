export default function LearnPage() {
  return (
    <main className="shell">
      <section className="hero" style={{ paddingBottom: 12 }}>
        <p className="eyebrow">Learning Material</p>
        <h1>Teach the world you are building.</h1>
        <p>
          This route is the first placeholder for the Next.js side of the
          product. It gives you a clean home for onboarding, tutorials, and any
          non-game content that should live beside the shared world.
        </p>
      </section>

      <section className="cards">
        <article className="card">
          <h3>Player Guides</h3>
          <p>
            Explain controls, world rules, and progression without mixing those
            concerns into the Rust/WASM client bundle.
          </p>
        </article>
        <article className="card">
          <h3>Creator Docs</h3>
          <p>
            Add publishing flows, documentation, and roadmap content using the
            normal React and Next.js ergonomics you wanted for the web layer.
          </p>
        </article>
      </section>
    </main>
  );
}

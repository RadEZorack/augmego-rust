import Link from "next/link";
import { auth } from "@/src/auth";

export default async function HomePage() {
  const session = await auth();

  return (
    <main className="shell">
      <section className="hero">
        <p className="eyebrow">Augmego Web</p>
        <h1>One web shell. One Rust world.</h1>
        <p>
          Next.js now owns the browser-facing app surface for sign-in, content,
          and learning material. The Rust voxel server still handles the
          authoritative shared world and realtime transport.
        </p>
        <div className="actions">
          <Link className="button primary" href="/play">
            Enter The World
          </Link>
          <Link className="button" href="/learn">
            Open Learning Hub
          </Link>
          {!session?.user ? (
            <Link className="button" href="/api/v1/auth/google">
              Sign In With Google
            </Link>
          ) : null}
        </div>
      </section>

      <section className="cards">
        <article className="card">
          <h2>Game Route</h2>
          <p>
            The Rust/WASM client is built by Trunk into <code>/play</code> and
            served through the same Next.js origin as the rest of the product.
          </p>
          <span className="meta">/play</span>
        </article>
        <article className="card">
          <h2>Auth Compatibility</h2>
          <p>
            The legacy Rust client keeps calling the same{" "}
            <code>/api/v1/auth/*</code> endpoints while the implementation now
            lives in Next.js and Prisma.
          </p>
          <span className="meta">/api/v1/auth</span>
        </article>
        <article className="card">
          <h2>Unified Origin</h2>
          <p>
            Browser pages, API routes, and the game shell now share the same
            host. Only the authoritative voxel socket stays separate at{" "}
            <code>/ws</code>.
          </p>
          <span className="meta">/ws</span>
        </article>
      </section>
    </main>
  );
}

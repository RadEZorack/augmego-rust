import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Augmego",
  description: "Next.js shell for the Augmego Rust voxel world.",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}

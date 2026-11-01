import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Mugshot Support",
  description: "Criogen demo: a support agent with threads, approvals and crash recovery",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}

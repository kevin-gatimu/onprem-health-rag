/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Vitest configuration — runs in jsdom so browser APIs are available.
// Keeps the Vite + React transform pipeline so TSX and CSS modules work.
export default defineConfig({
  plugins: [react()],
  test: {
    // jsdom gives us window, document, and navigator — necessary for Tauri's
    // mock layer (mockIPC) and @testing-library/react.
    environment: "jsdom",
    // Import the jest-dom matchers (toBeInTheDocument, etc.) before every test.
    setupFiles: ["./src/test/setup.ts"],
    globals: true,
    // Exclude the Tauri src-tauri Rust workspace from the scan.
    exclude: ["**/node_modules/**", "**/src-tauri/**", "**/dist/**"],
  },
});

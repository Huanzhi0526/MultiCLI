import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));
const appRoot = path.resolve(root, "../..");

export default {
  root,
  clearScreen: false,
  esbuild: { jsx: "automatic" },
  server: {
    host: "127.0.0.1",
    port: 1437,
    strictPort: true,
    watch: { ignored: ["**/webview/**", "**/profile/**", "**/cases/**", "**/probe/**", "**/*.log"] },
    fs: { allow: [root, appRoot] },
    hmr: { host: "127.0.0.1", clientPort: 1437 },
  },
  resolve: {
    alias: [
      { find: /^@\//, replacement: `${path.join(appRoot, "src")}/` },
      { find: /^react-dom\/client$/, replacement: path.join(appRoot, "node_modules/react-dom/client.js") },
      { find: /^react-dom(?:\/|$)/, replacement: `${path.join(appRoot, "node_modules/react-dom")}/` },
      { find: /^react(?:\/|$)/, replacement: `${path.join(appRoot, "node_modules/react")}/` },
      { find: /^@xterm\/xterm(?:\/|$)/, replacement: `${path.join(appRoot, "node_modules/@xterm/xterm")}/` },
      { find: /^@tauri-apps\/api(?:\/|$)/, replacement: `${path.join(appRoot, "node_modules/@tauri-apps/api")}/` },
    ],
    dedupe: ["react", "react-dom", "@xterm/xterm"],
  },
};

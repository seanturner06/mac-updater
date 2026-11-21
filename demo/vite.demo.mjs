// Serves the demo film and its app frames: `pnpm vite --config demo/vite.demo.mjs`, then open
// http://localhost:1499/demo/film.html (render.sh does this for you).
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

export default {
  root: fileURLToPath(new URL("..", import.meta.url)),
  plugins: [react()],
  server: { port: 1499, strictPort: true },
};

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
	plugins: [react()],
	clearScreen: false,
	server: {
		port: 5173,
		strictPort: true,
		host: true,
		watch: {
			ignored: ["**/src/**"],
		},
	},
	build: {
		target: "esnext",
		minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
		sourcemap: !!process.env.TAURI_DEBUG,
	},
});

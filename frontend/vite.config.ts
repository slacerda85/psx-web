import { defineConfig } from 'vite';

/**
 * Cross-origin isolation (COOP/COEP) ja fica ligada: o core ainda e
 * single-thread, mas habilitar depois exigiria mudar o deploy inteiro. Com
 * esses headers o `SharedArrayBuffer` fica disponivel para a Fase 6 (threads).
 *
 * Em producao os mesmos headers precisam vir do host — ver `docs/architecture.md`.
 */
const crossOriginIsolation = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};

export default defineConfig({
  // Base relativa para o build servir tanto na raiz quanto em subdiretorio
  // (GitHub Pages publica em /<repo>/).
  base: './',
  server: { port: 5173, headers: crossOriginIsolation },
  preview: { port: 4173, headers: crossOriginIsolation },
  build: {
    target: 'es2022',
    outDir: 'dist',
    sourcemap: true,
    // O .wasm tem ~60 KB e e carregado por fetch pelo glue do wasm-bindgen:
    // nunca deve virar data URI inline.
    assetsInlineLimit: 0,
  },
});

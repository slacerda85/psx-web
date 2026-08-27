import { defineConfig, devices } from '@playwright/test';

/**
 * O E2E roda contra o build de producao servido por `vite preview`, e nao
 * contra o dev server: e o artefato que vai para o ar que precisa funcionar,
 * incluindo o carregamento do `.wasm` como asset com hash.
 */
export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env['CI'],
  retries: process.env['CI'] ? 1 : 0,
  reporter: process.env['CI'] ? [['html', { open: 'never' }], ['list']] : 'list',
  use: {
    baseURL: 'http://127.0.0.1:4173',
    trace: 'on-first-retry',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    // Vite escuta so em 'localhost' por padrao, que no Windows resolve para
    // ::1 e deixa o 127.0.0.1 do baseURL sem resposta. O --host fixa o bind.
    command: 'npm run build && npx vite preview --port 4173 --strictPort --host 127.0.0.1',
    url: 'http://127.0.0.1:4173',
    reuseExistingServer: !process.env['CI'],
    timeout: 300_000,
  },
});

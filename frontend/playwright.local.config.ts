import { defineConfig, devices } from '@playwright/test';

/**
 * Config da verificação visual local (`e2e-local/`).
 *
 * Separada da suíte principal porque depende de uma BIOS real, que o projeto
 * não distribui e o CI não tem. Os testes se pulam sozinhos quando o arquivo
 * não está lá, então rodar isto nunca quebra — só deixa de verificar.
 */
export default defineConfig({
  testDir: './e2e-local',
  reporter: 'list',
  // Um frame real de emulação é mais lento que qualquer asserção de UI.
  timeout: 120_000,
  use: {
    baseURL: 'http://127.0.0.1:4173',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: 'npm run build && npx vite preview --port 4173 --strictPort --host 127.0.0.1',
    url: 'http://127.0.0.1:4173',
    reuseExistingServer: true,
    timeout: 300_000,
  },
});

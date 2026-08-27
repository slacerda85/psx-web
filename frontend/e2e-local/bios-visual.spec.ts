import { test, expect } from '@playwright/test';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * Verificação do caminho visual completo **dentro do navegador**, com uma BIOS
 * real.
 *
 * Vive fora da suíte principal e é **pulado** quando não há BIOS em `bios/`:
 * o projeto não distribui BIOS e o CI não terá nenhuma. Rodar isto localmente
 * é o que prova que WASM, WebGL e o laço de emulação funcionam juntos — a
 * suíte de `e2e/` cobre a UI, mas nunca executa um frame de verdade.
 *
 * ```sh
 * npm run test:visual
 * ```
 */

// O projeto é ESM: `__dirname` não existe, então vem da URL do módulo.
const here = dirname(fileURLToPath(import.meta.url));
const BIOS = resolve(here, '../../bios/SCPH1001.BIN');

/** Segundos de emulação antes de olhar a tela: o logo da Sony leva ~5 s. */
const WARMUP_MS = 9000;

test('a BIOS real desenha na tela e o laço mantém a taxa de frames', async ({ page }) => {
  test.skip(!existsSync(BIOS), 'sem bios/SCPH1001.BIN — nada a verificar');

  const errors: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error') errors.push(message.text());
  });
  page.on('pageerror', (error) => errors.push(error.message));

  await page.goto('/');
  await expect(page.locator('#version')).toHaveText(/psx-core/);

  const [chooser] = await Promise.all([
    page.waitForEvent('filechooser'),
    page.locator('#pick-bios').click(),
  ]);
  await chooser.setFiles({
    name: 'SCPH1001.BIN',
    mimeType: 'application/octet-stream',
    buffer: readFileSync(BIOS),
  });

  await expect(page.locator('#chip-bios')).toHaveAttribute('data-state', 'on');
  await page.locator('#btn-run').click();
  await expect(page.locator('#btn-run')).toHaveText('Pausar');

  await page.waitForTimeout(WARMUP_MS);

  // Ler os pixels de volta do WebGL prova que a imagem chegou ao canvas, e não
  // apenas que o core produziu um framebuffer.
  const painted = await page.evaluate(() => {
    const canvas = document.getElementById('screen') as HTMLCanvasElement;
    const gl = canvas.getContext('webgl2');
    if (!gl) return { total: 0, nonBlack: 0 };
    const pixels = new Uint8Array(canvas.width * canvas.height * 4);
    gl.readPixels(0, 0, canvas.width, canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
    let nonBlack = 0;
    for (let index = 0; index < pixels.length; index += 4) {
      if (pixels[index] || pixels[index + 1] || pixels[index + 2]) nonBlack++;
    }
    return { total: canvas.width * canvas.height, nonBlack };
  });

  const fps = await page.locator('#chip-fps').textContent();
  console.log(`FPS: ${fps} · pixels desenhados: ${painted.nonBlack} de ${painted.total}`);

  await page.screenshot({ path: 'test-results/bios-na-tela.png' });

  expect(errors).toEqual([]);
  expect(painted.nonBlack).toBeGreaterThan(1000);
  // O laço tem que estar de fato girando, não travado no primeiro frame.
  expect(fps).toMatch(/[1-9]\d* fps/);
});

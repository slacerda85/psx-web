import { test, expect, type Page } from '@playwright/test';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/**
 * O teclado chega ao console emulado?
 *
 * O core já responde a botões (há teste headless para isso), então este
 * verifica a outra metade do caminho: a tecla sai do navegador, vira máscara e
 * chega ao `setButtons`. Pulado sem BIOS, como o resto de `e2e-local/`.
 */

const here = dirname(fileURLToPath(import.meta.url));
const BIOS = resolve(here, '../../bios/SCPH1001.BIN');

/** Tempo até o menu da BIOS aparecer. */
const WARMUP_MS = 9000;

async function boot(page: Page): Promise<void> {
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
}

/** Assinatura do que está na tela, para comparar antes e depois. */
async function screenSignature(page: Page): Promise<string> {
  return page.evaluate(() => {
    const canvas = document.getElementById('screen') as HTMLCanvasElement;
    const gl = canvas.getContext('webgl2')!;
    const pixels = new Uint8Array(canvas.width * canvas.height * 4);
    gl.readPixels(0, 0, canvas.width, canvas.height, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
    // Soma por canal: barata e sensível o bastante para detectar o cursor
    // mudando de item no menu.
    let sum = 0;
    for (let index = 0; index < pixels.length; index += 4) {
      sum = (sum + pixels[index]! * 3 + pixels[index + 1]! * 5 + pixels[index + 2]! * 7) >>> 0;
    }
    return String(sum);
  });
}

test('uma seta do teclado muda o que a BIOS desenha', async ({ page }) => {
  test.skip(!existsSync(BIOS), 'sem bios/SCPH1001.BIN — nada a verificar');

  await boot(page);
  const before = await screenSignature(page);

  // Segura a seta por tempo suficiente para o menu reagir.
  await page.keyboard.down('ArrowDown');
  await page.waitForTimeout(1500);
  await page.keyboard.up('ArrowDown');
  await page.waitForTimeout(1500);

  const after = await screenSignature(page);
  await page.screenshot({ path: 'test-results/apos-seta.png' });

  expect(after).not.toBe(before);
});

test('as teclas continuam chegando depois de clicar num botão da UI', async ({ page }) => {
  test.skip(!existsSync(BIOS), 'sem bios/SCPH1001.BIN — nada a verificar');

  await boot(page);
  // Clicar num controle deixa o foco nele; a partir daí o teclado do jogo
  // concorre com a ativação do botão focado — é o caso que o usuário vive.
  await page.locator('#btn-keys').click();
  await page.keyboard.press('Escape');

  const before = await screenSignature(page);
  await page.keyboard.down('ArrowDown');
  await page.waitForTimeout(1500);
  await page.keyboard.up('ArrowDown');
  await page.waitForTimeout(1500);

  expect(await screenSignature(page)).not.toBe(before);
});

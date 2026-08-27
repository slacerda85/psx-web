import { test, expect, type Page } from '@playwright/test';

/**
 * Fumaca do frontend: garante que o bundle carrega, o WASM inicializa e o
 * caminho de entrada da BIOS se comporta.
 *
 * Nenhum teste aqui executa frames: sem uma BIOS real nao ha o que emular, e
 * o projeto nao distribui BIOS. A cobertura de emulacao vive nos testes
 * unitarios de `psx-core`.
 */

const BIOS_SIZE = 512 * 1024;

/** Coleta erros de console e excecoes nao tratadas durante o teste. */
function collectErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on('console', (message) => {
    if (message.type() === 'error') errors.push(message.text());
  });
  page.on('pageerror', (error) => errors.push(error.message));
  return errors;
}

/**
 * Carrega um arquivo pelo fluxo real da UI: o botao arma `pickFile` e abre o
 * seletor. Escrever direto no `<input>` escondido nao serve — sem a promessa
 * armada, o handler de `change` descarta o arquivo de proposito.
 */
async function upload(page: Page, trigger: string, name: string, size: number): Promise<void> {
  const [chooser] = await Promise.all([
    page.waitForEvent('filechooser'),
    page.locator(trigger).click(),
  ]);
  await chooser.setFiles({
    name,
    mimeType: 'application/octet-stream',
    buffer: Buffer.alloc(size),
  });
}

test('a pagina carrega e inicializa o WASM sem erros', async ({ page }) => {
  const errors = collectErrors(page);
  await page.goto('/');

  // `version()` so responde depois de `init()` resolver: e a prova de que o
  // modulo WASM foi buscado, instanciado e esta respondendo.
  await expect(page.locator('#version')).toHaveText(/psx-core \d+\.\d+\.\d+/);
  await expect(page.locator('#onboarding')).toBeVisible();
  await expect(page.locator('#btn-run')).toBeDisabled();
  expect(errors).toEqual([]);
});

test('o canvas obtem um contexto WebGL2', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#version')).toHaveText(/psx-core/);

  // Se `Renderer.create` tivesse falhado, a UI teria sido substituida pela
  // mensagem de erro; checar o contexto direto e mais especifico.
  const hasContext = await page.evaluate(() => {
    const canvas = document.getElementById('screen') as HTMLCanvasElement;
    return canvas.getContext('webgl2') !== null;
  });
  expect(hasContext).toBe(true);
});

test('uma BIOS com tamanho errado e recusada com mensagem clara', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#version')).toHaveText(/psx-core/);

  await upload(page, '#pick-bios', 'errada.bin', 1024);

  await expect(page.locator('#toast')).toBeVisible();
  await expect(page.locator('#toast')).toContainText('BIOS inválida');
  // Continua sem console: nada deve ter sido carregado.
  await expect(page.locator('#btn-run')).toBeDisabled();
  await expect(page.locator('#chip-bios')).toHaveAttribute('data-state', 'off');
});

test('uma BIOS de 512 KB habilita os controles e some com o onboarding', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#version')).toHaveText(/psx-core/);

  await upload(page, '#pick-bios', 'scph1001.bin', BIOS_SIZE);

  await expect(page.locator('#chip-bios')).toHaveAttribute('data-state', 'on');
  await expect(page.locator('#chip-bios')).toContainText('scph1001.bin');
  await expect(page.locator('#onboarding')).toBeHidden();
  await expect(page.locator('#btn-run')).toBeEnabled();
  await expect(page.locator('#btn-reset')).toBeEnabled();
});

test('a BIOS carregada sobrevive a um reload (IndexedDB)', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#version')).toHaveText(/psx-core/);
  await upload(page, '#pick-bios', 'scph1001.bin', BIOS_SIZE);
  await expect(page.locator('#chip-bios')).toHaveAttribute('data-state', 'on');

  await page.reload();

  // Sem reenviar o arquivo: se o chip acender de novo, veio do IndexedDB.
  await expect(page.locator('#chip-bios')).toHaveAttribute('data-state', 'on');
  await expect(page.locator('#btn-run')).toBeEnabled();
});

test('o dialogo de controles lista os 16 botoes do controller digital', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#version')).toHaveText(/psx-core/);

  await page.locator('#btn-keys').click();

  await expect(page.locator('#keys-dialog')).toBeVisible();
  await expect(page.locator('#keys-list li')).toHaveCount(16);
  await expect(page.locator('#keys-list li').first()).toContainText('Select');
});

test('remapear uma tecla persiste a nova ligacao', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('#version')).toHaveText(/psx-core/);
  await page.locator('#btn-keys').click();

  // "Cross" e KeyS no mapa padrao; vamos mover para KeyM.
  const row = page.locator('#keys-list li').filter({ hasText: 'Cross' });
  await row.locator('button').click();
  await expect(row.locator('button')).toHaveText('pressione…');
  await page.keyboard.press('KeyM');
  await expect(row.locator('button')).toHaveText('M');

  await page.reload();
  await page.locator('#btn-keys').click();
  await expect(
    page.locator('#keys-list li').filter({ hasText: 'Cross' }).locator('button'),
  ).toHaveText('M');
});

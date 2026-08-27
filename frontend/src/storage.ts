/**
 * Persistencia local: BIOS, mapa de teclas e memory cards.
 *
 * IndexedDB e usado em vez de `localStorage` porque a BIOS (512 KB) e cada
 * memory card (128 KB) sao binarios — `localStorage` so guarda string e tem
 * cota bem menor.
 *
 * Nada aqui sai do navegador: o projeto nao distribui nem envia BIOS.
 */

const DB_NAME = 'psx-web';
const DB_VERSION = 1;
const STORE = 'blobs';

const BIOS_KEY = 'bios';
const KEYMAP_KEY = 'keymap';
const memoryCardKey = (slot: number) => `memcard:${slot}`;

let dbPromise: Promise<IDBDatabase> | null = null;

function openDatabase(): Promise<IDBDatabase> {
  dbPromise ??= new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE);
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error('IndexedDB indisponivel.'));
  });
  return dbPromise;
}

async function put(key: string, value: unknown): Promise<void> {
  const db = await openDatabase();
  await new Promise<void>((resolve, reject) => {
    const transaction = db.transaction(STORE, 'readwrite');
    transaction.objectStore(STORE).put(value, key);
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error);
  });
}

async function get<T>(key: string): Promise<T | null> {
  const db = await openDatabase();
  return new Promise<T | null>((resolve, reject) => {
    const transaction = db.transaction(STORE, 'readonly');
    const request = transaction.objectStore(STORE).get(key);
    request.onsuccess = () => resolve((request.result as T | undefined) ?? null);
    request.onerror = () => reject(request.error);
  });
}

async function remove(key: string): Promise<void> {
  const db = await openDatabase();
  await new Promise<void>((resolve, reject) => {
    const transaction = db.transaction(STORE, 'readwrite');
    transaction.objectStore(STORE).delete(key);
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error);
  });
}

export interface StoredBios {
  name: string;
  bytes: Uint8Array;
}

export const storage = {
  /** Guarda a BIOS do usuario para nao precisar reenviar a cada visita. */
  async saveBios(name: string, bytes: Uint8Array): Promise<void> {
    // Uma copia do buffer: a view original pode apontar para memoria maior.
    await put(BIOS_KEY, { name, bytes: bytes.slice() });
  },

  async loadBios(): Promise<StoredBios | null> {
    const stored = await get<{ name: string; bytes: Uint8Array | ArrayBuffer }>(BIOS_KEY);
    if (!stored) return null;
    const bytes = stored.bytes instanceof Uint8Array ? stored.bytes : new Uint8Array(stored.bytes);
    return { name: stored.name, bytes };
  },

  async clearBios(): Promise<void> {
    await remove(BIOS_KEY);
  },

  async saveKeymap(keymap: Record<string, string>): Promise<void> {
    await put(KEYMAP_KEY, keymap);
  },

  async loadKeymap(): Promise<Record<string, string> | null> {
    return get<Record<string, string>>(KEYMAP_KEY);
  },

  async saveMemoryCard(slot: number, bytes: Uint8Array): Promise<void> {
    await put(memoryCardKey(slot), bytes.slice());
  },

  async loadMemoryCard(slot: number): Promise<Uint8Array | null> {
    const stored = await get<Uint8Array | ArrayBuffer>(memoryCardKey(slot));
    if (!stored) return null;
    return stored instanceof Uint8Array ? stored : new Uint8Array(stored);
  },

  /** `true` quando o navegador tem IndexedDB utilizavel (modo privado pode nao ter). */
  async available(): Promise<boolean> {
    try {
      await openDatabase();
      return true;
    } catch {
      return false;
    }
  },
};

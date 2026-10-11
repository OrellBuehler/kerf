import { describe, expect, test } from 'bun:test';
import { fxSummary, loadMasterFxOpen, MASTER_FX_KEY, saveMasterFxOpen } from './master-fx';

describe('the folded FX line', () => {
  const bus = { limiter: false, ceiling_db: -1.5, duck_depth_db: null as number | null | undefined };

  test('says Off for a master that does nothing', () => {
    expect(fxSummary(bus)).toEqual({ text: 'Off', active: false });
    expect(fxSummary({ ...bus, duck_depth_db: undefined })).toEqual({ text: 'Off', active: false });
  });

  test('a limiter that is on shows its ceiling, so folding the block does not hide it', () => {
    expect(fxSummary({ ...bus, limiter: true })).toEqual({ text: 'Limiter -1.5 dBFS', active: true });
    expect(fxSummary({ ...bus, limiter: true, ceiling_db: -0.02 }).text).toBe('Limiter 0.0 dBFS');
  });

  test('the speech gate is named, the compressor (the default) is not', () => {
    expect(fxSummary({ ...bus, duck_depth_db: -12 })).toEqual({ text: 'Speech gate', active: true });
    expect(fxSummary({ ...bus, limiter: true, duck_depth_db: -12 })).toEqual({ text: 'Limiter · Gate', active: true });
  });
});

describe('remembering the block', () => {
  test('folded unless it was opened, and a blocked store means folded', () => {
    expect(loadMasterFxOpen()).toBe(false);
    const store = new Map<string, string>();
    (globalThis as { localStorage?: unknown }).localStorage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v)
    };
    try {
      saveMasterFxOpen(true);
      expect(store.get(MASTER_FX_KEY)).toBe('1');
      expect(loadMasterFxOpen()).toBe(true);
      saveMasterFxOpen(false);
      expect(loadMasterFxOpen()).toBe(false);
    } finally {
      delete (globalThis as { localStorage?: unknown }).localStorage;
    }
    expect(loadMasterFxOpen()).toBe(false);
  });
});

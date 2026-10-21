import { describe, expect, it } from 'vitest';

import { primaryLabel, secondaryLabel, setupLine, setupTitle, type SetupInfo } from './setup';

const info: SetupInfo = {
  version: '0.1.1',
  dir: 'C:\\Users\\you\\AppData\\Local\\Scout',
  passive: false,
  run_after: false,
  update: false,
  has_installer: true,
  expected_bytes: 58_000_000,
};

describe('the setup window', () => {
  it('names each phase, for an install and for an update', () => {
    expect(setupTitle('ready', info)).toBe('Install Scout 0.1.1');
    expect(setupTitle('installing', info)).toBe('Installing Scout 0.1.1…');
    expect(setupTitle('done', info)).toBe('Scout is installed');
    expect(setupTitle('failed', info)).toBe('Scout could not be installed');
    const up = { ...info, update: true };
    expect(setupTitle('ready', up)).toBe('Update to Scout 0.1.1');
    expect(setupTitle('installing', up)).toBe('Updating Scout to 0.1.1…');
    expect(setupTitle('done', up)).toBe('Scout 0.1.1 is ready');
    expect(setupTitle('failed', up)).toBe('Scout could not be updated');
    expect(setupTitle('ready', null)).toBe('Install Scout');
  });

  it('says where it installs, how far it is, and what went wrong', () => {
    expect(setupLine('ready', info, 0, null)).toBe(
      'For you only, in C:\\Users\\you\\AppData\\Local\\Scout · about 55.3 MB.',
    );
    expect(setupLine('ready', { ...info, has_installer: false }, 0, null)).toBe(
      'This build of the setup carries no installer.',
    );
    expect(setupLine('ready', null, 0, null)).toBe('');
    expect(setupLine('installing', info, 42, null)).toBe('42 % · C:\\Users\\you\\AppData\\Local\\Scout');
    expect(setupLine('done', info, 100, null)).toBe('Open it now, or later from the Start menu.');
    expect(setupLine('failed', info, 30, 'the installer ended with exit code: 2')).toBe(
      'the installer ended with exit code: 2',
    );
    expect(setupLine('failed', info, 30, null)).toBe('unknown error');
  });

  it('labels the buttons per phase', () => {
    expect(primaryLabel('ready')).toBe('Install');
    expect(primaryLabel('installing')).toBe('Installing…');
    expect(primaryLabel('done')).toBe('Open Scout');
    expect(primaryLabel('failed')).toBe('Try again');
    expect(secondaryLabel('ready')).toBe('Cancel');
    expect(secondaryLabel('installing')).toBeNull();
    expect(secondaryLabel('done')).toBe('Close');
    expect(secondaryLabel('failed')).toBe('Close');
  });
});

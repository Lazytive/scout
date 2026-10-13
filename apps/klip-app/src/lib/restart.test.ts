import { describe, expect, it } from 'vitest';

import { joinReasons, restartMessage } from './restart';

describe('restart warning', () => {
  it('joins what changed', () => {
    expect(joinReasons([])).toBe('');
    expect(joinReasons(['Quality'])).toBe('Quality');
    expect(joinReasons(['Quality', 'Audio'])).toBe('Quality and Audio');
    expect(joinReasons(['Quality', 'Audio', 'Clip folders'])).toBe('Quality, Audio and Clip folders');
  });

  it('says what restarting now costs and what Later means', () => {
    expect(restartMessage(['Quality'], '1:20', false)).toBe(
      'Quality changes only when the recorder restarts. Restarting now clears the replay buffer (1:20 in it now). Later: the new settings apply the next time the recorder starts.',
    );
    expect(restartMessage(['Quality', 'Audio'], null, true)).toBe(
      'Quality and Audio change only when the recorder restarts. Restarting now clears the replay buffer; your recording goes on in a new file. Later: the new settings apply the next time the recorder starts.',
    );
  });
});

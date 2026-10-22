const assert = require('node:assert/strict');
const test = require('node:test');
const { speed, eta } = require('./config/assets/widget-live.js');

test('torrent speeds use binary units at the same boundaries for upload and download', () => {
  for (const [input, expected] of [[0, '0 KiB/s'], [1024, '1 KiB/s'], [524288, '512 KiB/s'], [1048576, '1.0 MiB/s'], [1572864, '1.5 MiB/s']]) {
    assert.equal(speed(input), expected);
  }
  assert.equal(speed(1023, true), '--');
  assert.equal(speed(-2), '0 KiB/s');
  assert.equal(speed(Infinity), '0 KiB/s');
});

test('torrent ETA distinguishes short, unavailable, hourly and infinite estimates', () => {
  for (const [input, expected] of [[-1, '--'], [0, '--'], [1, '<1m'], [59, '<1m'], [60, '1m'], [3599, '59m'], [3600, '1h 0m'], [3660, '1h 1m'], [8640000, '∞'], [8640001, '∞'], [NaN, '--']]) {
    assert.equal(eta(input), expected);
  }
});
